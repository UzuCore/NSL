//! One writer per persisted torrent session, including separate NSL processes.
//!
//! The lock file is a sibling of `torrent-session`, not *inside* it: the user
//! can safely clear the session directory while the lock is owned, and there
//! is no gap where a second process could recreate session.json concurrently.
//! Keep the file handle alive for the full underlying librqbit runtime.

use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub(super) struct SessionLock {
    _file: File,
}

impl SessionLock {
    pub fn path(default_folder: &Path) -> PathBuf {
        default_folder
            .parent()
            .unwrap_or(default_folder)
            .join(".nsl-torrent-session.lock")
    }

    pub fn acquire(default_folder: &Path) -> Result<Self, String> {
        let path = Self::path(default_folder);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err("토렌트 세션 잠금 파일이 안전한 일반 파일이 아닙니다".into());
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("토렌트 세션 잠금 파일 확인 실패: {error}"));
            }
            _ => {}
        }

        #[allow(unused_mut)]
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(windows)]
        {
            // Prevent the lock file being removed/renamed while a process
            // owns it, so a second instance cannot bypass this lock by
            // creating a different inode at the same path.
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0x1 | 0x2);
        }
        let file = options
            .open(&path)
            .map_err(|error| format!("토렌트 세션 잠금 파일 열기 실패: {error}"))?;
        let start = Instant::now();
        loop {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(Self { _file: file }),
                Err(error) => {
                    let busy = error.kind() == std::io::ErrorKind::WouldBlock
                        || matches!(error.raw_os_error(), Some(32 | 33 | 11));
                    if !busy {
                        return Err(format!("토렌트 세션 잠금 획득 실패: {error}"));
                    }
                    if start.elapsed() >= Duration::from_secs(3) {
                        return Err("다른 NSL 인스턴스가 동일 토렌트 세션을 사용 중입니다. 기존 프로그램을 닫은 뒤 다시 시도하세요".into());
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}
