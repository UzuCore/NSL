use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Explicit, user-confirmed cleanup only. Never called on startup.
/// Preserves recent conversion outputs and unknown names. The timestamp is
/// embedded in our own workspace names so opening files does not reset age.
pub fn cleanup_workspaces(work_root: &Path, min_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(work_root) else {
        return 0;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let min_age_ms = min_age.as_millis();
    let mut removed = 0;
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(stamp) = name
            .strip_prefix("NSL_Workspace_")
            .and_then(|value| value.parse::<u128>().ok())
        else {
            continue;
        };
        if now.saturating_sub(stamp) < min_age_ms {
            continue;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// NSZ/XCZ -> NSP/XCI entirely in Rust, without invoking external binaries.
/// The source stays read-only. A complete output is published atomically.
pub fn decompress(src: &Path, work_root: &Path) -> Result<PathBuf> {
    let kind = src
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    anyhow::ensure!(
        kind == "nsz" || kind == "xcz",
        "NSZ 또는 XCZ 파일만 변환할 수 있습니다"
    );
    anyhow::ensure!(src.is_file(), "원본 파일을 찾을 수 없습니다");
    std::fs::create_dir_all(work_root)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let workspace = (0..100u128)
        .find_map(|i| {
            let candidate = work_root.join(format!("NSL_Workspace_{}", stamp + i));
            match std::fs::create_dir(&candidate) {
                Ok(()) => Some(candidate),
                Err(_) => None,
            }
        })
        .context("새 변환 작업폴더를 생성할 수 없습니다")?;
    let extension = if kind == "nsz" { "nsp" } else { "xci" };
    let name = src
        .file_stem()
        .context("압축 파일 이름을 찾을 수 없습니다")?
        .to_string_lossy();
    let destination = workspace.join(format!("{name}.{extension}"));
    let mut staging = tempfile::NamedTempFile::new_in(&workspace)?;
    if kind == "nsz" {
        crate::native_package::unpack_nsz(src, staging.as_file_mut())?;
    } else {
        crate::native_package::unpack_xcz(src, staging.as_file_mut())?;
    }
    staging.as_file_mut().sync_all()?;
    staging.persist(&destination).map_err(|error| error.error)?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_never_removes_recent_or_unknown_conversion_outputs() {
        let root = tempfile::tempdir().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let recent = root.path().join(format!("NSL_Workspace_{now}"));
        let stale = root.path().join("NSL_Workspace_1");
        let unrecognized = root.path().join("NSL_Workspace_recent");
        for path in [&recent, &stale, &unrecognized] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::write(path.join("converted.xci"), b"test").unwrap();
        }
        assert_eq!(
            cleanup_workspaces(root.path(), Duration::from_secs(7 * 86400)),
            1
        );
        assert!(recent.exists());
        assert!(unrecognized.exists());
        assert!(!stale.exists());
    }
}
