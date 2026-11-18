use anyhow::Result;
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    sync::OnceLock,
    thread,
    time::Duration,
};
use wait_timeout::ChildExt;

const FAST_CHUNK: usize = 64 * 1024;

// Test-only timing identifies hot stages in the real-file regression without
// logging game paths or adding any work to production builds.
#[cfg(test)]
pub(crate) mod scan_profile {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Instant,
    };

    pub static SCAN: Stage = Stage::new();
    pub static CONTENTS: Stage = Stage::new();
    pub static NACP: Stage = Stage::new();
    pub static NSTOOL: Stage = Stage::new();
    pub static HACTOOL: Stage = Stage::new();
    pub static NSP_CONTENTS: Stage = Stage::new();
    pub static NSP_NAMES: Stage = Stage::new();
    pub static XCI: Stage = Stage::new();
    pub static CNMT_RAW_OK: AtomicU64 = AtomicU64::new(0);
    pub static CNMT_FALLBACK: AtomicU64 = AtomicU64::new(0);
    pub static NATIVE_CNMT_OK: AtomicU64 = AtomicU64::new(0);
    pub static NATIVE_CNMT_FALLBACK: AtomicU64 = AtomicU64::new(0);
    pub static NATIVE_NACP_OK: AtomicU64 = AtomicU64::new(0);
    pub static NATIVE_NACP_FALLBACK: AtomicU64 = AtomicU64::new(0);

    pub struct Stage {
        count: AtomicU64,
        elapsed_us: AtomicU64,
    }
    impl Stage {
        const fn new() -> Self {
            Self {
                count: AtomicU64::new(0),
                elapsed_us: AtomicU64::new(0),
            }
        }
        pub fn start(&'static self) -> Guard {
            Guard {
                stage: self,
                started: Instant::now(),
            }
        }
        fn snapshot(&self) -> (u64, u64) {
            (
                self.count.load(Ordering::Relaxed),
                self.elapsed_us.load(Ordering::Relaxed),
            )
        }
    }
    pub struct Guard {
        stage: &'static Stage,
        started: Instant,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            self.stage.count.fetch_add(1, Ordering::Relaxed);
            self.stage
                .elapsed_us
                .fetch_add(self.started.elapsed().as_micros() as u64, Ordering::Relaxed);
        }
    }
    pub fn report() -> String {
        [
            ("scan", &SCAN),
            ("header", &CONTENTS),
            ("nacp", &NACP),
            ("nstool", &NSTOOL),
            ("hactool", &HACTOOL),
            ("nsp_contents", &NSP_CONTENTS),
            ("nsp_names", &NSP_NAMES),
            ("xci", &XCI),
        ]
        .into_iter()
        .map(|(label, stage)| {
            let (count, us) = stage.snapshot();
            format!(
                "{label}={count} calls / {:.3} aggregate seconds",
                us as f64 / 1_000_000.0
            )
        })
        .chain([
            format!("cnmt_raw_ok={}", CNMT_RAW_OK.load(Ordering::Relaxed)),
            format!("cnmt_fallback={}", CNMT_FALLBACK.load(Ordering::Relaxed)),
            format!("native_cnmt_ok={}", NATIVE_CNMT_OK.load(Ordering::Relaxed)),
            format!(
                "native_cnmt_fallback={}",
                NATIVE_CNMT_FALLBACK.load(Ordering::Relaxed)
            ),
            format!("native_nacp_ok={}", NATIVE_NACP_OK.load(Ordering::Relaxed)),
            format!(
                "native_nacp_fallback={}",
                NATIVE_NACP_FALLBACK.load(Ordering::Relaxed)
            ),
        ])
        .collect::<Vec<_>>()
        .join(" | ")
    }
}

pub struct ScanResult {
    pub fast_hash: String,
    pub contents: Vec<Content>,
    pub nacp_ko: String,
    pub nacp_en: String,
    pub nacp_ja: String,
}
#[derive(Clone, Debug)]
pub struct Content {
    pub title_id: Option<String>,
    pub version: i64,
    pub content_type: String,
    pub source: String,
}

fn ticket_tid(data: &[u8]) -> Option<String> {
    if data.len() >= 4 {
        let sig = u32::from_le_bytes(data[0..4].try_into().ok()?);
        let body = match sig {
            0x10003 => 0x240,
            0x10004 => 0x140,
            0x10005 => 0x80,
            _ => 0,
        };
        if body > 0 && data.len() >= body + 0x168 {
            let id = hex(&data[body + 0x160..body + 0x168]);
            if id.starts_with("0100") || id.starts_with("0500") {
                return Some(id);
            }
        }
    }
    for off in [0x29e, 0x2a0, 0x39e, 0x3a0, 0x19e, 0x1a0, 0x2b0, 0x1be] {
        if off + 8 <= data.len() && data[off] == 1 && data[off + 1] == 0 {
            return Some(hex(&data[off..off + 8]));
        }
    }
    None
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

fn xml_title_id_regex() -> &'static Regex {
    static XML_TID: OnceLock<Regex> = OnceLock::new();
    XML_TID.get_or_init(|| {
        Regex::new(r"(?is)<(?:TitleId|Id|ApplicationId)[^>]*>\s*(?:0[xX])?([0-9a-f]{16})\s*</")
            .expect("XML title id regex is valid")
    })
}

fn container_contents(file: &mut File, path: &Path) -> Vec<Content> {
    #[cfg(test)]
    let _perf = scan_profile::CONTENTS.start();
    const NSP_OFFSETS: [u64; 6] = [0, 0x200, 0x400, 0x1000, 0x4000, 0x8000];
    const XCI_OFFSETS: [u64; 6] = [0x8000, 0x4000, 0, 0x200, 0x400, 0x1000];
    let size = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return vec![],
    };
    let is_xci = path
        .extension()
        .and_then(|v| v.to_str())
        .map(|v| v.eq_ignore_ascii_case("xci"))
        .unwrap_or(false);
    let offsets = if is_xci { &XCI_OFFSETS } else { &NSP_OFFSETS };
    for base in offsets {
        if base + 16 > size || file.seek(SeekFrom::Start(*base)).is_err() {
            continue;
        }
        let mut header = [0u8; 16];
        if file.read_exact(&mut header).is_err()
            || (&header[..4] != b"PFS0" && &header[..4] != b"HFS0")
        {
            continue;
        }
        let count = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let str_size = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        if count == 0 || count > 16384 || str_size > 64 * 1024 * 1024 {
            continue;
        }
        let entries_start = *base + 16;
        let entry_size = if &header[..4] == b"HFS0" { 0x40 } else { 0x18 };
        let table_size = count.saturating_mul(entry_size);
        if entries_start
            .saturating_add(table_size as u64)
            .saturating_add(str_size as u64)
            > size
        {
            continue;
        }
        let mut table = vec![0u8; table_size];
        if file.read_exact(&mut table).is_err() {
            continue;
        }
        let mut strings = vec![0u8; str_size];
        if file.read_exact(&mut strings).is_err() {
            continue;
        }
        let data_start = entries_start + table_size as u64 + str_size as u64;
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        for i in 0..count {
            let p = i * entry_size;
            let off = u64::from_le_bytes(table[p..p + 8].try_into().unwrap());
            let len = u64::from_le_bytes(table[p + 8..p + 16].try_into().unwrap());
            let no = u32::from_le_bytes(table[p + 16..p + 20].try_into().unwrap()) as usize;
            if no >= strings.len() || data_start.saturating_add(off).saturating_add(len) > size {
                continue;
            }
            let end = strings[no..]
                .iter()
                .position(|x| *x == 0)
                .map(|n| no + n)
                .unwrap_or(strings.len());
            let name = String::from_utf8_lossy(&strings[no..end]).to_string();
            let lower = name.to_ascii_lowercase();
            let mut content = None;
            if lower.ends_with(".tik") && len <= 1_048_576 {
                let mut buf = vec![0; len as usize];
                if file.seek(SeekFrom::Start(data_start + off)).is_ok()
                    && file.read_exact(&mut buf).is_ok()
                {
                    if let Some(tid) = ticket_tid(&buf) {
                        content = Some(Content {
                            content_type: content_type(&tid).into(),
                            title_id: Some(tid),
                            version: 0,
                            source: "header_ticket".into(),
                        })
                    }
                }
            }
            if lower.ends_with(".cnmt") && (13..=1_048_576).contains(&len) {
                let mut b = [0; 13];
                if file.seek(SeekFrom::Start(data_start + off)).is_ok()
                    && file.read_exact(&mut b).is_ok()
                {
                    let tid = format!("{:016X}", u64::from_le_bytes(b[..8].try_into().unwrap()));
                    let ver = u32::from_le_bytes(b[8..12].try_into().unwrap()) as i64;
                    let kind = match b[12] {
                        0x80 => "base",
                        0x81 => "update",
                        0x82 | 0x84 => "dlc",
                        0x83 => "delta",
                        _ => content_type(&tid),
                    };
                    content = Some(Content {
                        title_id: Some(tid),
                        version: ver,
                        content_type: kind.into(),
                        source: "header_cnmt".into(),
                    })
                }
            }
            if lower.ends_with(".cnmt.xml") && len <= 1_048_576 {
                let mut b = vec![0; len as usize];
                if file.seek(SeekFrom::Start(data_start + off)).is_ok()
                    && file.read_exact(&mut b).is_ok()
                {
                    let s = String::from_utf8_lossy(&b);
                    let re = xml_title_id_regex();
                    if let Some(c) = re.captures(&s) {
                        let tid = c[1].to_ascii_uppercase();
                        content = Some(Content {
                            content_type: content_type(&tid).into(),
                            title_id: Some(tid),
                            version: 0,
                            source: "header_xml".into(),
                        })
                    }
                }
            }
            if let Some(c) = content {
                if seen.insert(c.title_id.clone().unwrap_or_default()) {
                    result.push(c)
                }
            }
        }
        if !result.is_empty() {
            return result;
        }
    }
    Vec::new()
}

/// NACP layout: 16 language entries of 0x300 bytes starting at 0, then the
/// application metadata. The human-readable version strings live at 0x3060
/// (display version, e.g. "1.10") and 0x3070 (none).
const NACP_ENTRY_SIZE: usize = 0x300;
const NACP_LANGUAGES: usize = 15;
const NACP_REQUIRED_LEN: usize = NACP_ENTRY_SIZE * NACP_LANGUAGES;

/// Reads one localized application name out of a raw `control.nacp`.
fn nacp_name_at(bytes: &[u8], index: usize) -> String {
    let start = index * NACP_ENTRY_SIZE;
    let end = (start + 0x200).min(bytes.len());
    if start >= end {
        return String::new();
    }
    let field = &bytes[start..end];
    let end = field.iter().position(|v| *v == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end])
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

fn container_nacp(file: &mut File, path: &Path) -> (String, String, String) {
    #[cfg(test)]
    let _perf = scan_profile::NACP.start();
    let size = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return Default::default(),
    };
    let xci = path
        .extension()
        .and_then(|v| v.to_str())
        .map(|v| v.eq_ignore_ascii_case("xci"))
        .unwrap_or(false);
    let offsets: [u64; 6] = if xci {
        [0x8000, 0x4000, 0, 0x200, 0x400, 0x1000]
    } else {
        [0, 0x200, 0x400, 0x1000, 0x4000, 0x8000]
    };
    for base in offsets {
        if base + 16 > size || file.seek(SeekFrom::Start(base)).is_err() {
            continue;
        }
        let mut h = [0; 16];
        if file.read_exact(&mut h).is_err() || (&h[..4] != b"PFS0" && &h[..4] != b"HFS0") {
            continue;
        }
        let n = u32::from_le_bytes(h[4..8].try_into().unwrap()) as usize;
        let sl = u32::from_le_bytes(h[8..12].try_into().unwrap()) as usize;
        if n == 0 || n > 16384 || sl > 64 * 1024 * 1024 {
            continue;
        }
        let start = base + 16;
        let es = if &h[..4] == b"HFS0" { 0x40 } else { 0x18 };
        let tb = n * es;
        if start + tb as u64 + sl as u64 > size {
            continue;
        }
        let mut table = vec![0; tb];
        if file.read_exact(&mut table).is_err() {
            continue;
        }
        let mut strings = vec![0; sl];
        if file.read_exact(&mut strings).is_err() {
            continue;
        }
        let data = start + tb as u64 + sl as u64;
        for i in 0..n {
            let p = i * es;
            let off = u64::from_le_bytes(table[p..p + 8].try_into().unwrap());
            let len = u64::from_le_bytes(table[p + 8..p + 16].try_into().unwrap());
            let no = u32::from_le_bytes(table[p + 16..p + 20].try_into().unwrap()) as usize;
            if no >= sl
                || len < NACP_REQUIRED_LEN as u64
                || len > 0x10000
                || data + off + len > size
            {
                continue;
            }
            let end = strings[no..]
                .iter()
                .position(|c| *c == 0)
                .map(|v| no + v)
                .unwrap_or(sl);
            if !String::from_utf8_lossy(&strings[no..end])
                .to_ascii_lowercase()
                .ends_with(".nacp")
            {
                continue;
            }
            let mut bytes = vec![0; NACP_REQUIRED_LEN];
            if file.seek(SeekFrom::Start(data + off)).is_err()
                || file.read_exact(&mut bytes).is_err()
            {
                continue;
            }
            let en = if !nacp_name_at(&bytes, 0).is_empty() {
                nacp_name_at(&bytes, 0)
            } else {
                nacp_name_at(&bytes, 1)
            };
            let ja = nacp_name_at(&bytes, 2);
            let ko = nacp_name_at(&bytes, 12);
            return (ko, en, ja);
        }
    }
    Default::default()
}

fn nstool_path() -> Option<std::path::PathBuf> {
    let bundled = crate::common::base_dir().join("bin").join("nstool.exe");
    if bundled.is_file() {
        return Some(bundled);
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|v| std::env::split_paths(&v).collect::<Vec<_>>())
        .map(|p| p.join("nstool.exe"))
        .find(|p| p.is_file())
}
fn prod_keys() -> Option<std::path::PathBuf> {
    let root = crate::common::base_dir().join("prod.keys");
    if root.is_file() {
        return Some(root);
    }
    std::env::var_os("USERPROFILE")
        .map(std::path::PathBuf::from)
        .map(|p| p.join(".switch").join("prod.keys"))
        .filter(|p| p.is_file())
}

fn hactool_path() -> Option<std::path::PathBuf> {
    let bundled = crate::common::base_dir().join("bin").join("hactool.exe");
    if bundled.is_file() {
        return Some(bundled);
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|v| std::env::split_paths(&v).collect::<Vec<_>>())
        .flat_map(|dir| [dir.join("hactool.exe"), dir.join("hactool")])
        .find(|p| p.is_file())
}

fn run_hactool(args: &[String], target: &Path) -> Option<(bool, String)> {
    #[cfg(test)]
    let _perf = scan_profile::HACTOOL.start();
    let (engine, keys) = (hactool_path()?, prod_keys()?);
    let mut command = Command::new(engine);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .arg("--disablekeywarns")
        .arg("-k")
        .arg(keys)
        .args(args)
        .arg(target)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let out_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let status = match child.wait_timeout(Duration::from_secs(20)) {
        Ok(Some(status)) => status.success(),
        Ok(None) | Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            false
        }
    };
    let mut bytes = out_thread.join().unwrap_or_default();
    bytes.extend(err_thread.join().unwrap_or_default());
    Some((status, String::from_utf8_lossy(&bytes).into_owned()))
}

/// Extract commands only need the exit code and the generated file. Avoid
/// spawning two pipe-reader threads per child process: a full scan runs many
/// thousands of hactool commands. Output capture above remains available for
/// the XCI --listnca command, which actually needs to parse stdout.
fn run_hactool_quiet(args: &[String], target: &Path) -> bool {
    #[cfg(test)]
    let _perf = scan_profile::HACTOOL.start();
    let (Some(engine), Some(keys)) = (hactool_path(), prod_keys()) else {
        return false;
    };
    let mut command = Command::new(engine);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let Ok(mut child) = command
        .arg("--disablekeywarns")
        .arg("-k")
        .arg(keys)
        .args(args)
        .arg(target)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    match child.wait_timeout(Duration::from_secs(20)) {
        Ok(Some(status)) => status.success(),
        Ok(None) | Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            false
        }
    }
}

pub(crate) fn extract_nca_section0(nca: &Path, output_dir: &Path) -> bool {
    if std::fs::create_dir_all(output_dir).is_err() {
        return false;
    }
    let args = vec![format!("--section0dir={}", output_dir.display())];
    run_hactool_quiet(&args, nca)
}

fn nacp_file_names(path: &Path) -> Option<(String, String, String)> {
    let data = std::fs::read(path).ok()?;
    nacp_names_from_bytes(&data)
}

pub(crate) fn nacp_names_from_bytes(data: &[u8]) -> Option<(String, String, String)> {
    if data.len() < NACP_REQUIRED_LEN {
        return None;
    }
    let first = nacp_name_at(data, 0);
    let en = if first.is_empty() {
        nacp_name_at(data, 1)
    } else {
        first
    };
    let ja = nacp_name_at(data, 2);
    let ko = nacp_name_at(data, 12);
    if ko.is_empty() && en.is_empty() && ja.is_empty() {
        None
    } else {
        Some((ko, en, ja))
    }
}

#[derive(Clone)]
struct PfsNca {
    name: String,
    offset: u64,
    size: u64,
}

fn pfs_ncas(path: &Path) -> Option<Vec<PfsNca>> {
    let mut file = File::open(path).ok()?;
    let file_size = file.metadata().ok()?.len();
    for base in [0u64, 0x200, 0x400, 0x1000, 0x4000, 0x8000] {
        if base + 16 > file_size || file.seek(SeekFrom::Start(base)).is_err() {
            continue;
        }
        let mut header = [0; 16];
        if file.read_exact(&mut header).is_err() || &header[..4] != b"PFS0" {
            continue;
        }
        let count = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
        let strings_size = u32::from_le_bytes(header[8..12].try_into().ok()?) as usize;
        if count == 0 || count > 16384 || strings_size > 64 * 1024 * 1024 {
            continue;
        }
        let table_size = count.checked_mul(0x18)?;
        let data_start = base
            .checked_add(16)?
            .checked_add(table_size as u64)?
            .checked_add(strings_size as u64)?;
        if data_start > file_size {
            continue;
        }
        let mut table = vec![0; table_size];
        let mut strings = vec![0; strings_size];
        if file.read_exact(&mut table).is_err() || file.read_exact(&mut strings).is_err() {
            continue;
        }
        let mut entries = Vec::new();
        for i in 0..count {
            let p = i * 0x18;
            let relative = u64::from_le_bytes(table[p..p + 8].try_into().ok()?);
            let size = u64::from_le_bytes(table[p + 8..p + 16].try_into().ok()?);
            let name_offset = u32::from_le_bytes(table[p + 16..p + 20].try_into().ok()?) as usize;
            let Some(name_data) = strings.get(name_offset..) else {
                continue;
            };
            let name_end = name_data
                .iter()
                .position(|b| *b == 0)
                .unwrap_or(name_data.len());
            let name = String::from_utf8_lossy(&name_data[..name_end]).into_owned();
            if !name.to_ascii_lowercase().ends_with(".nca") {
                continue;
            }
            let Some(offset) = data_start.checked_add(relative) else {
                continue;
            };
            if offset.checked_add(size).map(|end| end <= file_size) != Some(true) {
                continue;
            }
            entries.push(PfsNca { name, offset, size });
        }
        if !entries.is_empty() {
            entries.sort_by_key(|entry| entry.size);
            return Some(entries);
        }
    }
    None
}

pub fn has_prod_keys() -> bool {
    prod_keys().is_some()
}

/// Encrypted CNMT metadata archives are tiny (a few KB), so this must stay well
/// below 1 MiB or title ids silently disappear from the library.
fn is_cnmt_candidate_size(size: u64) -> bool {
    (1024..=64 * 1024 * 1024).contains(&size)
}

fn hactool_nsp_names(path: &Path) -> Option<(String, String, String)> {
    hactool_nsp_names_with_native(path, true)
}

fn hactool_nsp_names_with_native(
    path: &Path,
    enable_native: bool,
) -> Option<(String, String, String)> {
    #[cfg(test)]
    let _perf = scan_profile::NSP_NAMES.start();
    let mut entries = pfs_ncas(path)?;
    entries.retain(|entry| {
        !entry.name.to_ascii_lowercase().ends_with(".cnmt.nca")
            && (64 * 1024..=512 * 1024 * 1024).contains(&entry.size)
    });
    let mut source = File::open(path).ok()?;
    let native_keys = enable_native
        .then(|| prod_keys().and_then(|path| crate::native_nca::cached_keys(&path)))
        .flatten();
    if let Some(ref keys) = native_keys {
        // Match hactool's first readable Control NCA in the same size-sorted
        // order. RomFS root-file traversal (native_nca::extract_nacp) now
        // selects exactly the same NACP even when remaster data also exists.
        for entry in entries.iter().take(12) {
            if let Some(kind) =
                crate::native_nca::content_kind(&mut source, entry.offset, entry.size, keys)
            {
                if kind != 2 {
                    continue;
                }
                if let Some(names) =
                    crate::native_nca::extract_nacp(&mut source, entry.offset, entry.size, keys)
                {
                    #[cfg(test)]
                    scan_profile::NATIVE_NACP_OK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return Some(names);
                }
            }
        }
    }
    // Unrecognized headers and missing keys use the original verified hactool.
    let work = tempfile::tempdir().ok()?;
    for (index, entry) in entries.iter().take(12).enumerate() {
        #[cfg(test)]
        scan_profile::NATIVE_NACP_FALLBACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nca = work.path().join(format!("candidate_{index}.nca"));
        source.seek(SeekFrom::Start(entry.offset)).ok()?;
        let mut limited = source.by_ref().take(entry.size);
        let mut output = File::create(&nca).ok()?;
        if std::io::copy(&mut limited, &mut output).is_err() {
            continue;
        }
        let section = work.path().join(format!("section_{index}"));
        std::fs::create_dir_all(&section).ok()?;
        // Most package NCAs are data/program archives, not Control NCAs.
        // The bundled hactool can reject their extraction immediately by
        // reading the NCA content type, avoiding unnecessary RomFS writes.
        let args = vec![
            "--xcontenttype=control".to_owned(),
            format!("--section0dir={}", section.display()),
        ];
        if run_hactool_quiet(&args, &nca) {
            if let Some(names) = nacp_file_names(&section.join("control.nacp")) {
                return Some(names);
            }
        }
    }
    None
}

fn parse_cnmt_path(path: &Path) -> Option<Content> {
    let mut file = File::open(path).ok()?;
    let mut data = [0u8; 13];
    file.read_exact(&mut data).ok()?;
    parse_cnmt_header(&data)
}

pub(crate) fn parse_cnmt_header(data: &[u8]) -> Option<Content> {
    if data.len() < 13 {
        return None;
    }
    let title_id = format!("{:016X}", u64::from_le_bytes(data[..8].try_into().ok()?));
    if title_id == "0000000000000000" {
        return None;
    }
    let version = u32::from_le_bytes(data[8..12].try_into().ok()?) as i64;
    let kind = match data[12] {
        0x80 => "base",
        0x81 => "update",
        0x82 | 0x84 => "dlc",
        0x83 => "delta",
        _ => content_type(&title_id),
    };
    Some(Content {
        title_id: Some(title_id),
        version,
        content_type: kind.into(),
        source: "header_hactool".into(),
    })
}

/// The decrypted section of a Meta NCA is a PFS0 container. Read its CNMT
/// header directly instead of asking hactool to materialize a directory tree
/// for every individual NCA. Non-PFS sections still use the legacy fallback.
fn cnmt_from_raw_section(path: &Path) -> Vec<Content> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(size) = file.metadata().map(|meta| meta.len()) else {
        return Vec::new();
    };
    if size < 16 {
        return Vec::new();
    }
    let mut header = [0u8; 16];
    if file.read_exact(&mut header).is_err() || &header[..4] != b"PFS0" {
        return Vec::new();
    }
    let count = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let string_size = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    if count == 0 || count > 16_384 || string_size > 64 * 1024 * 1024 {
        return Vec::new();
    }
    let Some(table_size) = count.checked_mul(0x18) else {
        return Vec::new();
    };
    let Some(data_start) = 16u64
        .checked_add(table_size as u64)
        .and_then(|value| value.checked_add(string_size as u64))
    else {
        return Vec::new();
    };
    if data_start > size {
        return Vec::new();
    }
    let mut table = vec![0u8; table_size];
    let mut strings = vec![0u8; string_size];
    if file.read_exact(&mut table).is_err() || file.read_exact(&mut strings).is_err() {
        return Vec::new();
    }
    let mut results = Vec::new();
    for index in 0..count {
        let p = index * 0x18;
        let offset = u64::from_le_bytes(table[p..p + 8].try_into().unwrap());
        let length = u64::from_le_bytes(table[p + 8..p + 16].try_into().unwrap());
        let name_off = u32::from_le_bytes(table[p + 16..p + 20].try_into().unwrap()) as usize;
        if name_off >= strings.len()
            || !(13..=1_048_576).contains(&length)
            || data_start
                .checked_add(offset)
                .and_then(|start| start.checked_add(length))
                .is_none_or(|end| end > size)
        {
            continue;
        }
        let name = strings[name_off..]
            .split(|byte| *byte == 0)
            .next()
            .unwrap_or_default();
        if !name.to_ascii_lowercase().ends_with(b".cnmt") {
            continue;
        }
        let mut data = [0u8; 13];
        if file.seek(SeekFrom::Start(data_start + offset)).is_ok()
            && file.read_exact(&mut data).is_ok()
        {
            if let Some(content) = parse_cnmt_header(&data) {
                if !results
                    .iter()
                    .any(|other: &Content| other.title_id == content.title_id)
                {
                    results.push(content);
                }
            }
        }
    }
    results
}

fn hactool_nsp_contents(path: &Path) -> Vec<Content> {
    hactool_nsp_contents_with_native(path, true)
}

fn hactool_nsp_contents_with_native(path: &Path, enable_native: bool) -> Vec<Content> {
    #[cfg(test)]
    let _perf = scan_profile::NSP_CONTENTS.start();
    let Some(mut entries) = pfs_ncas(path) else {
        return Vec::new();
    };
    // CNMT NCAs are tiny (a few KB to a few hundred KB), so requiring 1 MiB
    // silently skipped every metadata archive in the library.
    entries.retain(|entry| {
        entry.name.to_ascii_lowercase().ends_with(".cnmt.nca") && is_cnmt_candidate_size(entry.size)
    });
    let Ok(mut source) = File::open(path) else {
        return Vec::new();
    };
    let native_keys = enable_native
        .then(|| prod_keys().and_then(|path| crate::native_nca::cached_keys(&path)))
        .flatten();
    // The legacy extraction workspace is only needed if native crypto cannot
    // handle a particular NCA. Normal metadata extraction is diskless.
    let mut work = None;
    let mut contents = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if let Some(ref keys) = native_keys {
            if let Some(extracted) =
                crate::native_nca::extract_cnmt(&mut source, entry.offset, entry.size, keys)
            {
                #[cfg(test)]
                scan_profile::NATIVE_CNMT_OK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                for content in extracted {
                    if !contents
                        .iter()
                        .any(|known: &Content| known.title_id == content.title_id)
                    {
                        contents.push(content);
                    }
                }
                continue;
            }
        }
        #[cfg(test)]
        scan_profile::NATIVE_CNMT_FALLBACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if work.is_none() {
            work = tempfile::tempdir().ok();
        }
        let Some(ref work) = work else {
            continue;
        };
        let nca = work.path().join(format!("meta_{index}.nca"));
        if source.seek(SeekFrom::Start(entry.offset)).is_err() {
            continue;
        }
        let mut limited = source.by_ref().take(entry.size);
        let Ok(mut output) = File::create(&nca) else {
            continue;
        };
        if std::io::copy(&mut limited, &mut output).is_err() {
            continue;
        }
        let section_file = work.path().join(format!("meta_section_{index}.pfs0"));
        let raw_args = vec![format!("--section0={}", section_file.display())];
        let mut extracted = if run_hactool_quiet(&raw_args, &nca) {
            cnmt_from_raw_section(&section_file)
        } else {
            Vec::new()
        };
        if !extracted.is_empty() {
            #[cfg(test)]
            scan_profile::CNMT_RAW_OK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        } else {
            // Fallback for unsupported or future NCA section layouts.
            #[cfg(test)]
            scan_profile::CNMT_FALLBACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let section = work.path().join(format!("meta_section_{index}"));
            if std::fs::create_dir_all(&section).is_err() {
                continue;
            }
            let args = vec![format!("--section0dir={}", section.display())];
            if !run_hactool_quiet(&args, &nca) {
                continue;
            }
            for item in walkdir::WalkDir::new(&section)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
            {
                if item
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("cnmt"))
                {
                    if let Some(content) = parse_cnmt_path(item.path()) {
                        extracted.push(content);
                    }
                }
            }
        }
        for content in extracted {
            if !contents
                .iter()
                .any(|known: &Content| known.title_id == content.title_id)
            {
                contents.push(content);
            }
        }
    }
    contents
}

fn hactool_xci_names(path: &Path) -> Option<(String, String, String)> {
    let list_args = vec!["-t".into(), "xci".into(), "--listnca".into()];
    let (success, output) = run_hactool(&list_args, path)?;
    if !success {
        return None;
    }
    let target = output
        .lines()
        .find(|line| line.to_ascii_lowercase().contains("control"))?
        .split_whitespace()
        .last()?;
    let work = tempfile::tempdir().ok()?;
    let output_dir = work.path().join("nca");
    std::fs::create_dir_all(&output_dir).ok()?;
    let extract_args = vec![
        "-t".into(),
        "xci".into(),
        format!("--nca={target}"),
        format!("--outdir={}", output_dir.display()),
    ];
    if !run_hactool_quiet(&extract_args, path) {
        return None;
    }
    let nca = output_dir.join(target);
    let nca = if nca.is_file() {
        nca
    } else {
        walkdir::WalkDir::new(&output_dir)
            .into_iter()
            .filter_map(Result::ok)
            .find(|entry| {
                entry.file_type().is_file()
                    && entry
                        .path()
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .map(|extension| extension.eq_ignore_ascii_case("nca"))
                        .unwrap_or(false)
            })?
            .into_path()
    };
    let section = work.path().join("section");
    std::fs::create_dir_all(&section).ok()?;
    let args = vec![format!("--section0dir={}", section.display())];
    if !run_hactool_quiet(&args, &nca) {
        return None;
    }
    nacp_file_names(&section.join("control.nacp"))
}

fn hactool_nacp_names(path: &Path) -> Option<(String, String, String)> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "nsp" | "nsz" => hactool_nsp_names(path),
        "xci" | "xcz" => hactool_xci_names(path),
        _ => None,
    }
}

pub(crate) fn run_nstool(path: &Path, extra: &[String]) -> Option<String> {
    #[cfg(test)]
    let _perf = scan_profile::NSTOOL.start();
    let (Some(engine), Some(keys)) = (nstool_path(), prod_keys()) else {
        return None;
    };
    let mut command = Command::new(engine);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let Ok(mut child) = command
        .arg("--keyset")
        .arg(keys)
        .args(extra)
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return None;
    };
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out_thread = thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stdout.read_to_end(&mut v);
        v
    });
    let err_thread = thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stderr.read_to_end(&mut v);
        v
    });
    match child.wait_timeout(Duration::from_secs(60)) {
        Ok(Some(_)) => {}
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    }
    let mut bytes = out_thread.join().unwrap_or_default();
    bytes.extend(err_thread.join().unwrap_or_default());
    let text = String::from_utf8_lossy(&bytes);
    Some(text.into_owned())
}

fn nstool_scan(path: &Path) -> Vec<Content> {
    let Some(text) = run_nstool(path, &[]) else {
        return vec![];
    };
    static TID: OnceLock<Regex> = OnceLock::new();
    static VER: OnceLock<Regex> = OnceLock::new();
    let re=TID.get_or_init(||Regex::new(r"(?i)(?:TitleId|ProgramId|ProgId|Title\s*Id|ApplicationId|Application\s*Id|PatchId|Patch\s*Id|AddOnContentId|AddOn\s*Content\s*Id|AddonContentId|Addon\s*Content\s*Id)\s*[:\s]+(?:0[xX])?(0[15][0-9a-f]{14})").unwrap());
    let v_re = VER.get_or_init(|| Regex::new(r"(?i)(?:Version\s*[:\s]+|\(v)(\d+)\)?").unwrap());
    let mut found = HashMap::new();
    let mut current = None;
    for line in text.lines() {
        if let Some(c) = re.captures(line) {
            let id = c[1].to_ascii_uppercase();
            let lower = line.to_ascii_lowercase().replace(' ', "");
            let kind = if lower.contains("patchid") {
                "update"
            } else if lower.contains("addoncontentid") || lower.contains("addonid") {
                "dlc"
            } else if lower.contains("applicationid") || lower.contains("programid") {
                "base"
            } else {
                content_type(&id)
            };
            current = Some(id.clone());
            found.entry(id.clone()).or_insert(Content {
                title_id: Some(id),
                version: 0,
                content_type: kind.into(),
                source: "header_nstool".into(),
            });
            continue;
        }
        if let Some(id) = &current {
            if line.to_ascii_lowercase().contains("version")
                && !line.to_ascii_lowercase().contains("required")
            {
                if let Some(c) = v_re.captures(line) {
                    if let Ok(v) = c[1].parse::<i64>() {
                        if let Some(item) = found.get_mut(id) {
                            item.version = v
                        }
                    }
                }
            }
        }
    }
    found.into_values().collect()
}

pub fn fast_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut context = md5::Context::new();
    context.consume(size.to_string().as_bytes());
    let mut buffer = vec![0; FAST_CHUNK];
    let count = file.read(&mut buffer)?;
    context.consume(&buffer[..count]);
    if size > (FAST_CHUNK * 2) as u64 {
        file.seek(SeekFrom::End(-(FAST_CHUNK as i64)))?;
        let count = file.read(&mut buffer)?;
        context.consume(&buffer[..count]);
    }
    Ok(format!("{:x}", context.compute()))
}

pub fn scan_file_hashed(path: &Path, filename: &str, hash: String) -> Result<ScanResult> {
    #[cfg(test)]
    let _perf = scan_profile::SCAN.start();
    let mut file = File::open(path)?;
    anyhow::ensure!(
        file.metadata()?.len() >= 16,
        "파일이 비어 있거나 패키지 헤더가 손상됨: {}",
        path.display()
    );
    let mut contents = container_contents(&mut file, path);
    let (nacp_ko, nacp_en, nacp_ja) = container_nacp(&mut file, path);
    if path
        .extension()
        .and_then(|v| v.to_str())
        .map(|v| v.eq_ignore_ascii_case("xci"))
        .unwrap_or(false)
    {
        #[cfg(test)]
        let _xci_perf = scan_profile::XCI.start();
        let (xci_contents, names) = crate::xci::scan(path);
        if !xci_contents.is_empty() {
            contents = xci_contents;
        }
        // Use XCI control metadata when the package-level PFS did not expose NACP directly.
        let mut names = if !names.0.is_empty() || !names.1.is_empty() || !names.2.is_empty() {
            names
        } else {
            (nacp_ko, nacp_en, nacp_ja)
        };
        if names.0.is_empty() && names.1.is_empty() && names.2.is_empty() {
            if let Some(found) = hactool_nacp_names(path) {
                names = found;
            }
        }
        return finish_scan(path, filename, hash, contents, names);
    }
    let nsp = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            extension.eq_ignore_ascii_case("nsp") || extension.eq_ignore_ascii_case("nsz")
        })
        .unwrap_or(false);
    let header_has_cnmt = contents
        .iter()
        .any(|content| content.source == "header_cnmt");
    // Hactool CNMT data superseded nstool's results in the legacy scan path.
    // Run the authoritative step first and avoid a redundant nstool process
    // when CNMT extraction succeeds. Keep the existing fallback when it does
    // not, and preserve the older header-CNMT path unchanged.
    let mut extracted_cnmt = false;
    let mut names = (nacp_ko, nacp_en, nacp_ja);
    let needs_names = names.0.is_empty() && names.1.is_empty() && names.2.is_empty();
    let mut preloaded_names = None;
    let mut preloaded_names_attempted = false;
    if nsp && !header_has_cnmt {
        // These two reads are independent. Rayon joins them inside the same
        // bounded scan pool, avoiding extra OS threads and allowing a file
        // with many CNMT archives to proceed alongside NACP extraction.
        let deep = if needs_names {
            let (metadata, found_names) =
                rayon::join(|| hactool_nsp_contents(path), || hactool_nacp_names(path));
            preloaded_names = found_names;
            preloaded_names_attempted = true;
            metadata
        } else {
            hactool_nsp_contents(path)
        };
        if !deep.is_empty() {
            contents = deep;
            extracted_cnmt = true;
        }
    }
    if !extracted_cnmt && (!header_has_cnmt || needs_names) {
        let deep = nstool_scan(path);
        if !deep.is_empty() {
            contents = deep;
        }
    }
    if needs_names {
        let found_names = if preloaded_names_attempted {
            preloaded_names
        } else {
            hactool_nacp_names(path)
        };
        if let Some(found) = found_names {
            names = found;
        }
    }

    finish_scan(path, filename, hash, contents, names)
}

fn finish_scan(
    path: &Path,
    filename: &str,
    hash: String,
    contents: Vec<Content>,
    names: (String, String, String),
) -> Result<ScanResult> {
    static TID: OnceLock<Regex> = OnceLock::new();
    static VERSION: OnceLock<Regex> = OnceLock::new();
    let tid_re = TID.get_or_init(|| Regex::new(r"(?i)\[([0-9a-f]{16})\]").unwrap());
    let ver_re = VERSION.get_or_init(|| Regex::new(r"(?i)\[v(\d+)\]").unwrap());
    let mut contents = contents;
    let version = ver_re
        .captures(filename)
        .and_then(|m| m[1].parse().ok())
        .unwrap_or(0);
    let is_xci = path
        .extension()
        .and_then(|v| v.to_str())
        .map(|v| v.eq_ignore_ascii_case("xci") || v.eq_ignore_ascii_case("xcz"))
        .unwrap_or(false);
    if contents.is_empty() {
        let title_id = tid_re.captures(filename).map(|m| m[1].to_ascii_uppercase());
        let kind = title_id.as_deref().map(content_type).unwrap_or("unknown");
        contents.push(Content {
            source: if title_id.is_some() {
                "filename"
            } else {
                "none"
            }
            .into(),
            title_id,
            version,
            content_type: if is_xci { "base" } else { kind }.into(),
        });
    }
    // Multi-game XCI/XCZ bundles can expose update/DLC entries only. The library
    // filters on base records, so derive the base title id like the Python build.
    if is_xci && !contents.iter().any(|c| c.content_type == "base") {
        if let Some(first) = contents
            .iter()
            .filter_map(|c| c.title_id.as_deref())
            .find(|tid| tid.starts_with("01") && tid.len() >= 13)
        {
            let derived = format!("{}000", &first[..13]);
            if !contents
                .iter()
                .any(|c| c.title_id.as_deref() == Some(derived.as_str()))
            {
                contents.push(Content {
                    title_id: Some(derived),
                    version: 0,
                    content_type: "base".into(),
                    source: "derived_base".into(),
                });
            }
        }
    }
    Ok(ScanResult {
        fast_hash: hash,
        contents,
        nacp_ko: names.0,
        nacp_en: names.1,
        nacp_ja: names.2,
    })
}

pub fn content_type(title_id: &str) -> &'static str {
    let Ok(id) = u64::from_str_radix(title_id, 16) else {
        return "unknown";
    };
    match id & 0x1fff {
        0 => "base",
        0x800 => "update",
        _ => "dlc",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    fn pfs0(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut strings = Vec::new();
        let mut table = Vec::new();
        let mut payload = Vec::new();
        for (name, bytes) in entries {
            let name_offset = strings.len() as u32;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            table.extend_from_slice(&(payload.len() as u64).to_le_bytes());
            table.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            table.extend_from_slice(&name_offset.to_le_bytes());
            table.extend_from_slice(&0u32.to_le_bytes());
            payload.extend_from_slice(bytes);
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"PFS0");
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        out.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend(table);
        out.extend(strings);
        out.extend(payload);
        out
    }

    fn write_temp_file(bytes: &[u8], name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn parses_raw_decrypted_cnmt_section_without_directory_extraction() {
        let mut cnmt = Vec::new();
        cnmt.extend_from_slice(&0x0100_D2C0_115B_6800u64.to_le_bytes());
        cnmt.extend_from_slice(&131072u32.to_le_bytes());
        cnmt.push(0x81);
        let (_dir, path) = write_temp_file(&pfs0(&[("meta.cnmt", cnmt)]), "section0.pfs0");
        let found = cnmt_from_raw_section(&path);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title_id.as_deref(), Some("0100D2C0115B6800"));
        assert_eq!(found[0].version, 131072);
        assert_eq!(found[0].content_type, "update");
        assert_eq!(found[0].source, "header_hactool");
        fs::write(&path, b"PFS0\x01").unwrap();
        assert!(cnmt_from_raw_section(&path).is_empty());
    }

    #[test]
    fn cnmt_candidates_include_small_encrypted_metadata_archives() {
        // Real CNMT NCAs are small: a 3.5 KB and a 36 KB archive were both
        // skipped by an earlier 1 MiB lower bound, which silently dropped the
        // title id for most of the library. Guard the accepted size window.
        for size in [1_024u64, 3_584, 36_864, 629_248] {
            assert!(
                is_cnmt_candidate_size(size),
                "CNMT archive of {size} bytes must be considered"
            );
        }
        assert!(!is_cnmt_candidate_size(512), "tiny files are not NCAs");
        assert!(!is_cnmt_candidate_size(65 * 1024 * 1024));
    }

    #[test]
    fn parses_cnmt_metadata_from_pfs0() {
        let mut cnmt = Vec::new();
        cnmt.extend_from_slice(&0x0100_D2C0_115B_6800u64.to_le_bytes());
        cnmt.extend_from_slice(&65536u32.to_le_bytes());
        cnmt.push(0x81);
        let bytes = pfs0(&[("meta.cnmt", cnmt)]);
        let (_dir, path) = write_temp_file(&bytes, "sample.nsp");
        let mut file = File::open(&path).unwrap();

        let contents = container_contents(&mut file, &path);

        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].title_id.as_deref(), Some("0100D2C0115B6800"));
        assert_eq!(contents[0].version, 65536);
        assert_eq!(contents[0].content_type, "update");
        assert_eq!(contents[0].source, "header_cnmt");
    }

    #[test]
    fn reads_localized_names_from_control_nacp() {
        let mut nacp = vec![0; NACP_REQUIRED_LEN];
        for (index, name) in [(0usize, "English title"), (2, "日本語"), (12, "한국어")] {
            let offset = index * 0x300;
            nacp[offset..offset + name.len()].copy_from_slice(name.as_bytes());
        }
        let bytes = pfs0(&[("control.nacp", nacp)]);
        let (_dir, path) = write_temp_file(&bytes, "sample.nsp");
        let mut file = File::open(&path).unwrap();

        assert_eq!(
            container_nacp(&mut file, &path),
            ("한국어".into(), "English title".into(), "日本語".into())
        );
    }

    #[test]
    fn derives_base_record_for_multi_game_xci_bundles() {
        let path = Path::new("Bundle [0100D2C0115B6800][v65536].xci");
        let result = finish_scan(
            path,
            "Bundle [0100D2C0115B6800][v65536].xci",
            "hash".into(),
            vec![
                Content {
                    title_id: Some("0100D2C0115B6800".into()),
                    version: 65536,
                    content_type: "update".into(),
                    source: "header_cnmt".into(),
                },
                Content {
                    title_id: Some("0100D2C0115B7001".into()),
                    version: 0,
                    content_type: "dlc".into(),
                    source: "header_cnmt".into(),
                },
            ],
            Default::default(),
        )
        .unwrap();

        let derived = result
            .contents
            .iter()
            .find(|c| c.source == "derived_base")
            .expect("bundle without base entry must derive one");
        assert_eq!(derived.title_id.as_deref(), Some("0100D2C0115B6000"));
        assert_eq!(derived.content_type, "base");
    }

    #[test]
    fn keeps_existing_base_record_in_xci_bundles() {
        let path = Path::new("Bundle.xci");
        let result = finish_scan(
            path,
            "Bundle.xci",
            "hash".into(),
            vec![Content {
                title_id: Some("0100D2C0115B6000".into()),
                version: 0,
                content_type: "base".into(),
                source: "header_cnmt".into(),
            }],
            Default::default(),
        )
        .unwrap();

        assert!(!result.contents.iter().any(|c| c.source == "derived_base"));
    }

    #[test]
    fn filename_fallback_classifies_ids_and_versions() {
        let path = Path::new("Game [0100D2C0115B6800][v65536].nsp");

        let result = finish_scan(
            path,
            "Game [0100D2C0115B6800][v65536].nsp",
            "hash".into(),
            vec![],
            Default::default(),
        )
        .unwrap();

        assert_eq!(
            result.contents[0].title_id.as_deref(),
            Some("0100D2C0115B6800")
        );
        assert_eq!(result.contents[0].version, 65536);
        assert_eq!(result.contents[0].content_type, "update");
        assert_eq!(content_type("0100D2C0115B6000"), "base");
        assert_eq!(content_type("0100D2C0115B6800"), "update");
        assert_eq!(content_type("0100D2C0115B7001"), "dlc");
    }

    #[test]
    fn empty_package_is_an_error_not_a_successful_scan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.nsp");
        std::fs::write(&path, []).unwrap();
        let hash = fast_hash(&path).unwrap();
        assert!(scan_file_hashed(&path, "broken.nsp", hash).is_err());
    }

    #[test]
    #[ignore = "requires the local game corpus"]
    fn scans_small_real_nsp_and_nsz_against_library_records() {
        use rusqlite::{Connection, OpenFlags};

        let db_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("library.db");
        let db = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        for extension in ["nsp", "nsz"] {
            let (path, expected_id, expected_version, expected_kind, expected_hash): (
                String,
                String,
                i64,
                String,
                String,
            ) = db
                .query_row(
                    "SELECT gf.path,fc.title_id,fc.version,fc.content_type,COALESCE(gf.fast_hash,'') FROM game_files gf JOIN file_contents fc ON fc.path=gf.path WHERE lower(gf.file_type)=?1 AND fc.title_id IS NOT NULL AND EXISTS(SELECT 1 FROM file_contents c WHERE c.path=gf.path AND c.title_id IS NOT NULL) ORDER BY gf.size LIMIT 1",
                    [extension],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .unwrap();
            let path = PathBuf::from(path);
            let hash = fast_hash(&path).unwrap();
            assert_eq!(
                hash, expected_hash,
                "{extension} fast hash differs from library"
            );
            let result = scan_file_hashed(
                &path,
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default(),
                hash,
            )
            .unwrap();
            assert_eq!(result.fast_hash, expected_hash);
            assert!(
                result.contents.iter().any(|content| {
                    content.title_id.as_deref() == Some(&expected_id)
                        && content.version == expected_version
                        && content.content_type == expected_kind
                }),
                "{extension} scan missed library title ID {expected_id}"
            );
        }
    }

    /// Compare freshly decoded native metadata with freshly extracted hactool
    /// results. The existing library database may contain older name values.
    #[test]
    #[ignore = "reads all local games and starts hactool for differential QA"]
    fn native_metadata_matches_legacy_hactool_real_corpus() {
        use rayon::prelude::*;
        use rusqlite::{Connection, OpenFlags};

        let reference = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("library.db");
        let db = Connection::open_with_flags(reference, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let paths: Vec<String> = db
            .prepare(
                "SELECT path FROM game_files WHERE lower(file_type) IN ('nsp','nsz') ORDER BY path",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(paths.len(), 846);
        let started = std::time::Instant::now();
        let names_only = std::env::var_os("NSL_QA_NAMES_ONLY").is_some();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap();
        let compared: Vec<_> = pool.install(|| {
            paths
                .par_iter()
                .map(|name| {
                    let path = Path::new(name);
                    let content_equal = if names_only {
                        true
                    } else {
                        let native = hactool_nsp_contents_with_native(path, true);
                        let legacy = hactool_nsp_contents_with_native(path, false);
                        let as_set = |contents: Vec<Content>| {
                            contents.into_iter()
                                .map(|entry| (entry.title_id, entry.version, entry.content_type))
                                .collect::<HashSet<_>>()
                        };
                        as_set(native) == as_set(legacy)
                    };
                    let native_names = hactool_nsp_names_with_native(path, true);
                    let legacy_names = hactool_nsp_names_with_native(path, false);
                    let names_equal = native_names == legacy_names;
                    if !names_equal {
                        eprintln!("NACP_PARITY_DETAIL filename={:?} native={native_names:?} legacy={legacy_names:?}",path.file_name());
                    }
                    (name.clone(), content_equal, names_equal)
                })
                .collect()
        });
        let different_content: Vec<_> = compared.iter().filter(|(_, same, _)| !same).collect();
        let different_names: Vec<_> = compared.iter().filter(|(_, _, same)| !same).collect();
        eprintln!("NATIVE_HACTOOL_DIFFERENTIAL files={} content_differences={} names_differences={} elapsed={:.2}s",
            compared.len(), different_content.len(), different_names.len(), started.elapsed().as_secs_f64());
        assert!(
            different_content.is_empty(),
            "CNMT differences on {} files",
            different_content.len()
        );
        assert!(
            different_names.is_empty(),
            "NACP differences on {} files",
            different_names.len()
        );
    }
}
