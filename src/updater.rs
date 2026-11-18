use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const RELEASE_URL: &str = "https://api.github.com/repos/UzuCore/NSL/releases/latest";
const RUST_ASSET: &str = "nsl-rust-win64.zip";

#[derive(Clone, Debug)]
pub struct ReleaseInfo {
    pub current_version: String,
    pub latest_version: String,
    pub is_newer: bool,
    pub notes: String,
    pub download_url: Option<String>,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct PackageManifest {
    application: String,
    version: String,
    platform: String,
    sha256: HashMap<String, String>,
}

fn hash_file(path: &Path) -> Result<String> {
    let mut reader = io::BufReader::new(fs::File::open(path)?);
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn trusted_release_url(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.path().starts_with("/UzuCore/NSL/releases/download/")
    })
}

fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(timeout).build()
}

fn numeric_version(value: &str) -> Vec<u64> {
    value
        .trim_start_matches(['v', 'V'])
        .split(['-', '+'])
        .next()
        .unwrap_or("")
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn is_newer(candidate: &str, current: &str) -> bool {
    let mut a = numeric_version(candidate);
    let mut b = numeric_version(current);
    let length = a.len().max(b.len());
    a.resize(length, 0);
    b.resize(length, 0);
    a > b
}

pub fn check_latest() -> Result<ReleaseInfo> {
    let response: GitHubRelease = http_agent(Duration::from_secs(10))
        .get(RELEASE_URL)
        .set("User-Agent", "NSL-Rust")
        .call()
        .context("GitHub release check failed")?
        .into_json()
        .context("GitHub returned invalid release JSON")?;
    let latest_version = response.tag_name.trim_start_matches(['v', 'V']).to_owned();
    let current_version = env!("CARGO_PKG_VERSION").to_owned();
    let download_url = response
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case(RUST_ASSET))
        .map(|asset| asset.browser_download_url.clone());
    Ok(ReleaseInfo {
        is_newer: is_newer(&latest_version, &current_version),
        current_version,
        latest_version,
        notes: response.body,
        download_url,
    })
}

pub fn download_and_prepare(url: &str, expected_version: &str, base_dir: &Path) -> Result<PathBuf> {
    anyhow::ensure!(
        trusted_release_url(url),
        "update URL is not a trusted NSL GitHub release"
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs();
    let task_name = format!("nsl_rust_update_{}_{}", std::process::id(), now);
    let task_dir = base_dir.join("temp").join(task_name);
    let archive = task_dir.join("update.zip");
    let extraction = task_dir.join("package");
    fs::create_dir_all(&extraction)?;

    let response = http_agent(Duration::from_secs(120))
        .get(url)
        .set("User-Agent", "NSL-Rust")
        .call()
        .context("Rust update download failed")?;
    let mut file = fs::File::create(&archive)?;
    const MAX_PACKAGE_SIZE: u64 = 512 * 1024 * 1024;
    let downloaded = io::copy(
        &mut response.into_reader().take(MAX_PACKAGE_SIZE + 1),
        &mut file,
    )
    .context("could not save update archive")?;
    anyhow::ensure!(
        downloaded <= MAX_PACKAGE_SIZE,
        "update archive exceeds 512 MB"
    );
    drop(file);

    let script = task_dir.join("extract.ps1");
    fs::write(&script, include_str!("../scripts/extract-update.ps1"))?;
    let mut command = Command::new("powershell.exe");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .arg("-ArchivePath")
        .arg(&archive)
        .arg("-DestinationPath")
        .arg(&extraction)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("could not extract the Rust update package")?;
    if !output.status.success() {
        anyhow::bail!(
            "update package extraction failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let manifest_path = extraction.join("nsl-rust-manifest.json");
    let manifest: PackageManifest = serde_json::from_slice(
        &fs::read(&manifest_path).context("Rust update manifest is missing")?,
    )
    .context("Rust update manifest is invalid")?;
    if manifest.application != "NSL-Rust"
        || manifest.platform != "windows-x86_64"
        || numeric_version(&manifest.version) != numeric_version(expected_version)
    {
        anyhow::bail!("update archive is not an NSL Rust package for this release");
    }
    for relative in [
        "nsl.exe",
        "bin/nstool.exe",
        "bin/hactool.exe",
        "titledb/titles.json",
    ] {
        if !extraction.join(relative).is_file() {
            anyhow::bail!("update package is incomplete: missing {relative}");
        }
    }
    for relative in ["nsl.exe", "bin/nstool.exe", "bin/hactool.exe"] {
        let expected = manifest
            .sha256
            .get(relative)
            .with_context(|| format!("update manifest is missing SHA-256 for {relative}"))?;
        anyhow::ensure!(
            expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid SHA-256 for {relative}"
        );
        anyhow::ensure!(
            hash_file(&extraction.join(relative))?.eq_ignore_ascii_case(expected),
            "update package integrity check failed for {relative}"
        );
    }
    Ok(task_dir)
}

pub fn launch_apply_and_exit(task_dir: &Path, base_dir: &Path) -> Result<()> {
    let base = base_dir
        .canonicalize()
        .context("could not resolve app folder")?;
    let running = std::env::current_exe()
        .context("could not resolve current executable")?
        .canonicalize()?;
    if running.file_name().and_then(|name| name.to_str()) != Some("nsl.exe")
        || running.parent() != Some(base.as_path())
    {
        anyhow::bail!("in-place updates require the packaged nsl.exe build");
    }
    let task = task_dir
        .canonicalize()
        .context("could not resolve staged update folder")?;
    let temp = base
        .join("temp")
        .canonicalize()
        .context("update folder is outside the app folder")?;
    if !task.starts_with(&temp) || !task.is_dir() {
        anyhow::bail!("staged update path is outside the app temp folder");
    }
    task.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.starts_with("nsl_rust_update_"))
        .ok_or_else(|| anyhow!("staged update folder has an unexpected name"))?;
    // Only application binaries and manifest are installed; user data and
    // title databases are never overwritten during an automatic update.
    let script = task.join("apply.ps1");
    fs::write(&script, include_str!("../scripts/apply-update.ps1"))?;
    let mut command = Command::new("powershell.exe");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .arg("-BaseDir")
        .arg(&base)
        .arg("-TaskDir")
        .arg(&task)
        .current_dir(&base)
        .spawn()
        .context("could not start update replacement script")?;
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_release_versions_with_prefixes_and_suffixes() {
        assert!(is_newer("v1.5.0", "1.4.9"));
        assert!(is_newer("1.4.9-beta", "1.4.8"));
        assert!(!is_newer("1.4.8+build.7", "v1.4.8"));
        assert!(!is_newer("1.4.8", "1.4.8.0"));
        assert!(is_newer("2.0.1", "2.0.0"));
        assert!(!is_newer("2.0.1", "2.0.1"));
    }
}
