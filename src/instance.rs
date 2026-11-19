//! Process-wide guard for one writable NSL data directory.
//! A Windows named mutex is owned by the kernel, not by a stale lock file:
//! an abnormal termination releases the handle automatically.

use anyhow::Result;
use std::path::Path;

#[cfg(windows)]
mod platform {
    use super::*;
    use anyhow::Context;
    use sha2::{Digest, Sha256};
    use std::{ffi::c_void, ptr};

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(
            attributes: *const c_void,
            initial_owner: i32,
            name: *const u16,
        ) -> *mut c_void;
        fn GetLastError() -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(hwnd: *mut c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }

    pub struct Guard(*mut c_void);

    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn mutex_name(base_dir: &Path) -> Result<String> {
        let path = base_dir.canonicalize().or_else(|_| {
            let cwd = std::env::current_dir()?;
            Ok::<_, std::io::Error>(if base_dir.is_absolute() {
                base_dir.to_path_buf()
            } else {
                cwd.join(base_dir)
            })
        })?;
        let key = path.to_string_lossy().replace('/', "\\").to_lowercase();
        let digest = Sha256::digest(key.as_bytes());
        // Global so separate Windows desktop sessions cannot corrupt the same DB.
        Ok(format!("Global\\NSL_Data_{:x}", digest))
    }

    pub fn acquire(base_dir: &Path) -> Result<Option<Guard>> {
        let name = wide(&mutex_name(base_dir)?);
        let handle = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error())
                .context("NSL 중복 실행 잠금을 만들 수 없습니다");
        }
        let already_exists = unsafe { GetLastError() } == 183;
        if already_exists {
            unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        Ok(Some(Guard(handle)))
    }

    pub fn warn_duplicate() {
        let message = wide(
            "같은 데이터 폴더를 사용하는 NSL이 이미 실행 중입니다.\n\n기존 창을 이용하세요. 데이터베이스·미디어 캐시·다운로드 충돌을 방지하기 위해 새 창을 열지 않습니다.",
        );
        let title = wide("NSL - 중복 실행 방지");
        unsafe { MessageBoxW(ptr::null_mut(), message.as_ptr(), title.as_ptr(), 0x40) };
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn same_data_dir_cannot_be_opened_twice_and_lock_is_released() {
            let tmp = tempfile::tempdir().unwrap();
            let guard = acquire(tmp.path()).unwrap().unwrap();
            assert!(acquire(tmp.path()).unwrap().is_none());
            drop(guard);
            assert!(acquire(tmp.path()).unwrap().is_some());
        }

        #[test]
        fn child_expects_parent_mutex() {
            let Ok(dir) = std::env::var("NSL_TEST_MUTEX_DIR") else {
                return;
            };
            assert!(acquire(Path::new(&dir)).unwrap().is_none());
        }

        #[test]
        fn named_mutex_rejects_a_second_process_using_the_same_library() {
            let tmp = tempfile::tempdir().unwrap();
            let _guard = acquire(tmp.path()).unwrap().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "instance::platform::tests::child_expects_parent_mutex",
                ])
                .env("NSL_TEST_MUTEX_DIR", tmp.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "second process bypassed lock: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    pub struct Guard;
    pub fn acquire(_base_dir: &Path) -> Result<Option<Guard>> {
        Ok(Some(Guard))
    }
    pub fn warn_duplicate() {
        eprintln!("NSL is already running");
    }
}

pub use platform::{acquire, warn_duplicate};
