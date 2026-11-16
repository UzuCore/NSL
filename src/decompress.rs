use anyhow::{Context, Result};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use wait_timeout::ChildExt;

fn nsz_tool() -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .flat_map(|p| [p.join("nsz.exe"), p.join("nsz")])
        .find(|p| p.is_file())
}

pub fn decompress(src: &Path, work_root: &Path) -> Result<PathBuf> {
    let tool = nsz_tool().context("NSZ 압축 해제 도구(nsz)를 PATH에서 찾지 못했습니다")?;
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
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let workspace = work_root.join(format!("NSL_Workspace_{stamp}"));
    std::fs::create_dir_all(&workspace)?;
    let mut cmd = Command::new(tool);
    cmd.args(["-D", "-w", "-o"])
        .arg(&workspace)
        .arg(src)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let mut child = cmd.spawn().context("nsz 실행에 실패했습니다")?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out_thread = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stdout.read_to_end(&mut b);
        b
    });
    let err_thread = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stderr.read_to_end(&mut b);
        b
    });
    let status = match child.wait_timeout(Duration::from_secs(4 * 60 * 60))? {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("압축 해제 제한 시간(4시간)을 초과했습니다")
        }
    };
    let mut output = out_thread.join().unwrap_or_default();
    output.extend(err_thread.join().unwrap_or_default());
    if !status.success() {
        let details = String::from_utf8_lossy(&output);
        anyhow::bail!(
            "nsz 종료 코드 {:?}: {}",
            status.code(),
            details.chars().take(700).collect::<String>()
        )
    }
    let target_ext = if kind == "nsz" { "nsp" } else { "xci" };
    let stem = src.file_stem().unwrap_or_default().to_string_lossy();
    let expected = workspace.join(format!("{stem}.{target_ext}"));
    if expected.is_file() {
        return Ok(expected);
    }
    let mut candidates: Vec<PathBuf> = walkdir::WalkDir::new(&workspace)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().is_file()
                && e.path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case(target_ext))
                    .unwrap_or(false)
        })
        .map(|e| e.path().to_path_buf())
        .collect();
    candidates.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH)
    });
    candidates
        .pop()
        .context("변환은 끝났지만 출력 파일을 찾지 못했습니다")
}
