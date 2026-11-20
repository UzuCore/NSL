//! NSL adaptation of NSRP's temporary workspace provider.
//! Do not embed or automatically extract NSRP's external tools or assets.
//! Workspaces are private, temporary and always cleaned up on drop.
use anyhow::{Context, Result};
use std::{env, fs, path::PathBuf};

pub fn temp_workspace(prefix: &str) -> Result<tempfile::TempDir> {
    let root = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("NSL")
        .join("NativeWork");
    fs::create_dir_all(&root)
        .with_context(|| format!("병합 임시 작업 폴더 생성 실패: {}", root.display()))?;
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&root)
        .with_context(|| format!("병합 임시 작업 폴더 생성 실패: {}", root.display()))
}
