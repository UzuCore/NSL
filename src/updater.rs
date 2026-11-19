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
const COMPARE_URL: &str = "https://api.github.com/repos/UzuCore/NSL/compare";
const RELEASE_ASSET: &str = "nsl-win64.zip";

pub fn build_commit() -> &'static str {
    env!("NSL_BUILD_COMMIT")
}

pub fn short_commit(value: &str) -> &str {
    value.get(..7).unwrap_or(value)
}

pub fn build_commit_short() -> &'static str {
    short_commit(build_commit())
}

#[derive(Clone, Debug)]
pub struct ReleaseInfo {
    pub current_version: String,
    pub latest_version: String,
    pub latest_commit: String,
    pub release_tag: String,
    pub is_newer: bool,
    pub notes: String,
    pub download_url: Option<String>,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    target_commitish: String,
    #[serde(default)]
    published_at: Option<String>,
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
struct GitHubComparison {
    status: String,
}

#[derive(Deserialize)]
struct PackageManifest {
    application: String,
    version: String,
    commit: String,
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

fn valid_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The manual Windows workflow creates a KST date/time tag for each published ZIP.
fn dated_release_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix("build-") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    parts.len() == 4
        && parts[0].len() == 8
        && parts[0].bytes().all(|c| c.is_ascii_digit())
        && parts[1].len() == 6
        && parts[1].bytes().all(|c| c.is_ascii_digit())
        && parts[2] == "KST"
        && parts[3]
            .strip_prefix('r')
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_digit()))
}

fn trusted_release_url(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|url| {
        let parts: Vec<&str> = url
            .path_segments()
            .map(|segments| segments.collect())
            .unwrap_or_default();
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.query().is_none()
            && url.fragment().is_none()
            && parts.len() == 6
            && parts[..4] == ["UzuCore", "NSL", "releases", "download"]
            && dated_release_tag(parts[4])
            && parts[5] == RELEASE_ASSET
    })
}

fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(timeout).build()
}

fn published_after_local_commit(published_at: Option<&str>) -> bool {
    let local_timestamp = env!("NSL_BUILD_COMMIT_TIME").parse::<i64>().unwrap_or(0);
    let remote_timestamp = published_at
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|date| date.timestamp())
        .unwrap_or(0);
    local_timestamp > 0 && remote_timestamp > local_timestamp
}

fn commit_is_newer(candidate: &str, current: &str, published_at: Option<&str>) -> Result<bool> {
    if candidate.eq_ignore_ascii_case(current) {
        return Ok(false);
    }
    anyhow::ensure!(valid_commit(candidate), "invalid release commit SHA");
    if !valid_commit(current) {
        return Ok(published_after_local_commit(published_at));
    }
    let response = http_agent(Duration::from_secs(10))
        .get(&format!("{COMPARE_URL}/{current}...{candidate}"))
        .set("User-Agent", "NSL")
        .call();
    match response {
        Ok(result) => {
            let comparison: GitHubComparison = result
                .into_json()
                .context("GitHub returned an invalid commit comparison")?;
            // A one-commit history rewrite makes previously tagged releases
            // diverge. Use publication time only in that migration case.
            Ok(match comparison.status.as_str() {
                "ahead" => true,
                "diverged" => published_after_local_commit(published_at),
                _ => false,
            })
        }
        // A local development build may use a Git history which was never
        // uploaded. Use publication time only for that unavailable SHA.
        Err(ureq::Error::Status(404, _)) => Ok(published_after_local_commit(published_at)),
        Err(error) => Err(anyhow!("GitHub commit comparison failed: {error}")),
    }
}

pub fn check_latest() -> Result<ReleaseInfo> {
    let response: GitHubRelease = http_agent(Duration::from_secs(10))
        .get(RELEASE_URL)
        .set("User-Agent", "NSL")
        .call()
        .context("GitHub release check failed")?
        .into_json()
        .context("GitHub returned invalid release JSON")?;
    let current_version = build_commit_short().to_owned();
    let release_commit = response.target_commitish.to_ascii_lowercase();
    let compatible_release = dated_release_tag(&response.tag_name) && valid_commit(&release_commit);
    let latest_version = if compatible_release {
        short_commit(&release_commit).to_owned()
    } else {
        current_version.clone()
    };
    let download_url = response
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case(RELEASE_ASSET))
        .map(|asset| asset.browser_download_url.clone())
        .filter(|url| trusted_release_url(url));
    let is_newer = compatible_release
        && commit_is_newer(
            &release_commit,
            build_commit(),
            response.published_at.as_deref(),
        )?;
    Ok(ReleaseInfo {
        is_newer,
        current_version,
        latest_version,
        latest_commit: release_commit,
        release_tag: response.tag_name,
        notes: response.body,
        download_url,
    })
}

pub fn download_and_prepare_with_progress(
    url: &str,
    expected_commit: &str,
    base_dir: &Path,
    progress: Option<&crate::progress::TaskProgress>,
) -> Result<PathBuf> {
    if let Some(progress) = progress {
        progress.begin("업데이트 서버 연결 중", None, true);
    }
    anyhow::ensure!(
        trusted_release_url(url),
        "update URL is not a trusted NSL GitHub release"
    );
    anyhow::ensure!(
        valid_commit(expected_commit),
        "invalid expected release commit"
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs();
    let task_name = format!("nsl_update_{}_{}", std::process::id(), now);
    let task_dir = base_dir.join("temp").join(task_name);
    let archive = task_dir.join("update.zip");
    let extraction = task_dir.join("package");
    fs::create_dir_all(&extraction)?;

    let response = http_agent(Duration::from_secs(120))
        .get(url)
        .set("User-Agent", "NSL")
        .call()
        .context("NSL update download failed")?;
    let mut file = fs::File::create(&archive)?;
    const MAX_PACKAGE_SIZE: u64 = 512 * 1024 * 1024;
    if let Some(progress) = progress {
        let length = response
            .header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok());
        progress.begin("업데이트 패키지 다운로드", length, true);
    }
    let mut reader = response.into_reader().take(MAX_PACKAGE_SIZE + 1);
    let downloaded = if let Some(progress) = progress {
        io::copy(
            &mut crate::progress::MeteredReader::new(&mut reader, progress),
            &mut file,
        )
    } else {
        io::copy(&mut reader, &mut file)
    }
    .context("could not save update archive")?;
    anyhow::ensure!(
        downloaded <= MAX_PACKAGE_SIZE,
        "update archive exceeds 512 MB"
    );
    drop(file);
    if let Some(progress) = progress {
        progress.begin("업데이트 패키지 압축 해제", None, false);
    }

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
        .context("could not extract the NSL update package")?;
    if !output.status.success() {
        anyhow::bail!(
            "update package extraction failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let manifest_path = extraction.join("nsl-manifest.json");
    let manifest: PackageManifest = serde_json::from_slice(
        &fs::read(&manifest_path).context("NSL update manifest is missing")?,
    )
    .context("NSL update manifest is invalid")?;
    if manifest.application != "NSL"
        || manifest.platform != "windows-x86_64"
        || manifest.version.trim().is_empty()
        || !valid_commit(&manifest.commit)
        || !manifest.commit.eq_ignore_ascii_case(expected_commit)
    {
        anyhow::bail!("update archive does not match the GitHub release commit");
    }
    // Public Actions builds intentionally do not package a user's title DB.
    // Only the executable and its manifest are required for an app update.
    anyhow::ensure!(
        extraction.join("nsl.exe").is_file(),
        "update package is incomplete: missing nsl.exe"
    );
    if let Some(progress) = progress {
        progress.begin("업데이트 파일 무결성 검증", Some(1), false);
    }
    for (index, relative) in ["nsl.exe"].into_iter().enumerate() {
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
        if let Some(progress) = progress {
            progress.set(index as u64 + 1);
        }
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
        .filter(|name| name.starts_with("nsl_update_"))
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
    fn accepts_only_manual_dated_release_zips_and_full_commit_hashes() {
        let tag = "build-20261119-103500-KST-r7";
        assert!(dated_release_tag(tag));
        assert!(!dated_release_tag("1.4.8"));
        assert!(!dated_release_tag("build-20261119-KST-r7"));
        assert!(!dated_release_tag("build-20261119-103500-KST-r7/extra"));
        assert!(trusted_release_url(&format!(
            "https://github.com/UzuCore/NSL/releases/download/{tag}/nsl-win64.zip"
        )));
        assert!(!trusted_release_url(&format!(
            "https://github.com/UzuCore/NSL/releases/download/{tag}/nsl.exe"
        )));
        assert!(!trusted_release_url(&format!(
            "https://example.org/UzuCore/NSL/releases/download/{tag}/nsl-win64.zip"
        )));
        assert!(!trusted_release_url(&format!(
            "https://github.com/UzuCore/NSL/releases/download/{tag}/nsl-rust-win64.zip"
        )));
        assert!(valid_commit(&"a".repeat(40)));
        assert!(!valid_commit("2.0.1"));
        assert_eq!(short_commit(&"a".repeat(40)), "aaaaaaa");
    }

    #[test]
    fn built_version_label_is_git_commit() {
        assert!(
            valid_commit(build_commit()),
            "Git build must embed full commit SHA"
        );
        assert_eq!(build_commit_short().len(), 7);
    }

    #[test]
    #[ignore = "requires a published manual GitHub Release ZIP and an internet connection"]
    fn published_manual_release_downloads_and_prepares_without_user_data() {
        let info = check_latest().unwrap();
        assert!(dated_release_tag(&info.release_tag));
        assert!(valid_commit(&info.latest_commit));
        let url = info.download_url.expect("manual release ZIP is missing");
        let scratch = tempfile::tempdir().unwrap();
        let prepared =
            download_and_prepare_with_progress(&url, &info.latest_commit, scratch.path(), None)
                .unwrap();
        let manifest: PackageManifest =
            serde_json::from_slice(&fs::read(prepared.join("package/nsl-manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest.commit, info.latest_commit);
        assert!(!prepared.join("package/library.db").exists());
        assert!(!prepared.join("package/prod.keys").exists());
        assert!(prepared.join("package/nsl.exe").is_file());
    }
}
