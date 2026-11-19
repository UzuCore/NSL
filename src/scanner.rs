use anyhow::Result;
use regex::Regex;
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::OnceLock,
};

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
    pub static NSP_CONTENTS: Stage = Stage::new();
    pub static NSP_NAMES: Stage = Stage::new();
    pub static XCI: Stage = Stage::new();
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
        let Some(index) = crate::container_index::read_for_metadata(file, *base, size, None) else {
            continue;
        };
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        for entry in &index.entries {
            let len = entry.size;
            let absolute = index.data_start + entry.offset;
            let lower = entry.name.to_ascii_lowercase();
            let mut content = None;
            if lower.ends_with(".tik") && len <= 1_048_576 {
                let mut buf = vec![0; len as usize];
                if file.seek(SeekFrom::Start(absolute)).is_ok() && file.read_exact(&mut buf).is_ok()
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
                if file.seek(SeekFrom::Start(absolute)).is_ok() && file.read_exact(&mut b).is_ok() {
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
                if file.seek(SeekFrom::Start(absolute)).is_ok() && file.read_exact(&mut b).is_ok() {
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
        let Some(index) = crate::container_index::read_for_metadata(file, base, size, None) else {
            continue;
        };
        for entry in &index.entries {
            if !(NACP_REQUIRED_LEN as u64..=0x10000).contains(&entry.size)
                || !entry.name.to_ascii_lowercase().ends_with(".nacp")
            {
                continue;
            }
            let mut bytes = vec![0; NACP_REQUIRED_LEN];
            if file
                .seek(SeekFrom::Start(index.data_start + entry.offset))
                .is_err()
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

pub(crate) fn prod_keys() -> Option<std::path::PathBuf> {
    let root = crate::common::base_dir().join("prod.keys");
    if root.is_file() {
        return Some(root);
    }
    std::env::var_os("USERPROFILE")
        .map(std::path::PathBuf::from)
        .map(|p| p.join(".switch").join("prod.keys"))
        .filter(|p| p.is_file())
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

#[cfg(test)]
thread_local! {
    static NATIVE_INDEX_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn pfs_ncas(path: &Path) -> Option<Vec<PfsNca>> {
    #[cfg(test)]
    NATIVE_INDEX_READS.with(|counter| counter.set(counter.get() + 1));
    let mut file = File::open(path).ok()?;
    let file_size = file.metadata().ok()?.len();
    for base in [0u64, 0x200, 0x400, 0x1000, 0x4000, 0x8000] {
        let Some(index) = crate::container_index::read_for_metadata(
            &mut file,
            base,
            file_size,
            Some(crate::container_index::Kind::Pfs0),
        ) else {
            continue;
        };
        let data_start = index.data_start;
        let mut entries: Vec<PfsNca> = index
            .entries
            .into_iter()
            .filter(|entry| {
                let name = entry.name.to_ascii_lowercase();
                name.ends_with(".nca") || name.ends_with(".ncz")
            })
            .map(|entry| PfsNca {
                name: entry.name,
                offset: data_start + entry.offset,
                size: entry.size,
            })
            .collect();
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

/// A single package index and a single validated key cache serve both
/// metadata paths. Each worker still opens its own file handle: sharing a
/// seek cursor across the parallel NACP/CNMT readers would corrupt offsets.
fn native_nsp_metadata(
    path: &Path,
    needs_names: bool,
) -> (Vec<Content>, Option<(String, String, String)>) {
    let Some(entries) = pfs_ncas(path) else {
        return (Vec::new(), None);
    };
    let Some(keys) = prod_keys().and_then(|p| crate::native_nca::cached_keys(&p)) else {
        return (Vec::new(), None);
    };
    if needs_names {
        rayon::join(
            || native_nsp_contents(path, &entries, &keys),
            || native_nsp_names(path, &entries, &keys),
        )
    } else {
        (native_nsp_contents(path, &entries, &keys), None)
    }
}

/// Native NSP/NSZ title metadata, with bounded NCZ decoding. No NCA is
/// extracted to disk and no external process is started.
fn native_nsp_contents(
    path: &Path,
    entries: &[PfsNca],
    keys: &crate::native_nca::Keys,
) -> Vec<Content> {
    #[cfg(test)]
    let _perf = scan_profile::NSP_CONTENTS.start();
    let Ok(mut input) = File::open(path) else {
        return Vec::new();
    };
    let mut result: Vec<Content> = Vec::new();
    for entry in entries.iter().filter(|e| {
        (e.name.to_ascii_lowercase().ends_with(".cnmt.nca")
            || e.name.to_ascii_lowercase().ends_with(".cnmt.ncz"))
            && is_cnmt_candidate_size(e.size)
    }) {
        let compressed = entry.name.to_ascii_lowercase().ends_with(".ncz");
        if let Some(found) = crate::native_nca::extract_cnmt_package(
            &mut input,
            entry.offset,
            entry.size,
            compressed,
            keys,
        ) {
            #[cfg(test)]
            scan_profile::NATIVE_CNMT_OK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            for content in found {
                if let Some(old) = result.iter_mut().find(|c| c.title_id == content.title_id) {
                    if content.version > old.version {
                        *old = content;
                    }
                } else {
                    result.push(content);
                }
            }
        } else {
            #[cfg(test)]
            scan_profile::NATIVE_CNMT_FALLBACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    result
}

fn native_nsp_names(
    path: &Path,
    entries: &[PfsNca],
    keys: &crate::native_nca::Keys,
) -> Option<(String, String, String)> {
    #[cfg(test)]
    let _perf = scan_profile::NSP_NAMES.start();
    let candidates = entries.iter().filter(|entry| {
        !entry.name.to_ascii_lowercase().contains(".cnmt.")
            && (64 * 1024..=512 * 1024 * 1024).contains(&entry.size)
    });
    let mut input = File::open(path).ok()?;
    for entry in candidates.take(32) {
        if crate::native_nca::content_kind(&mut input, entry.offset, entry.size, keys) != Some(2) {
            continue;
        }
        let compressed = entry.name.to_ascii_lowercase().ends_with(".ncz");
        if let Some(names) = crate::native_nca::extract_nacp_package(
            &mut input,
            entry.offset,
            entry.size,
            compressed,
            keys,
        ) {
            #[cfg(test)]
            scan_profile::NATIVE_NACP_OK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Some(names);
        }
        #[cfg(test)]
        scan_profile::NATIVE_NACP_FALLBACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    None
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
        source: "header_native".into(),
    })
}

/// The decrypted section of a Meta NCA is a PFS0 container. Read its CNMT
/// header directly instead of asking hactool to materialize a directory tree
/// for every individual NCA. Non-PFS sections still use the legacy fallback.
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
    // Both XCI and XCZ are game-card HFS0 containers. The previous XCZ
    // branch treated them like NSPs and silently missed the update/DLC
    // partitions inside combined dumps.
    if path
        .extension()
        .and_then(|v| v.to_str())
        .map(|v| v.eq_ignore_ascii_case("xci") || v.eq_ignore_ascii_case("xcz"))
        .unwrap_or(false)
    {
        #[cfg(test)]
        let _xci_perf = scan_profile::XCI.start();
        let (contents, names) = crate::xci::scan(path);
        return finish_scan(path, filename, hash, contents, names);
    }
    let mut contents = container_contents(&mut file, path);
    let (nacp_ko, nacp_en, nacp_ja) = container_nacp(&mut file, path);
    let nsp = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            extension.eq_ignore_ascii_case("nsp") || extension.eq_ignore_ascii_case("nsz")
        })
        .unwrap_or(false);
    // Native CNMT/NACP extraction reads both NCA and NCZ packages directly.
    // Unrecognized or rights-controlled archive formats retain the header,
    // ticket, XML and filename metadata rather than guessing decrypted data.
    let mut names = (nacp_ko, nacp_en, nacp_ja);
    let needs_names = names.0.is_empty() && names.1.is_empty() && names.2.is_empty();
    if nsp {
        let (deep, extracted_names) = native_nsp_metadata(path, needs_names);
        if !deep.is_empty() {
            contents = deep;
        }
        if let Some(found) = extracted_names {
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
    fn native_metadata_parses_a_shared_package_index_only_once() {
        let (_dir, path) = write_temp_file(
            &pfs0(&[("control.nca", vec![0u8; 0xC00])]),
            "shared-index.nsp",
        );
        let before = NATIVE_INDEX_READS.with(std::cell::Cell::get);
        let (contents, names) = native_nsp_metadata(&path, true);
        assert!(contents.is_empty());
        assert!(names.is_none());
        assert_eq!(
            NATIVE_INDEX_READS.with(std::cell::Cell::get) - before,
            1,
            "parallel CNMT/NACP metadata readers must share one parsed PFS index"
        );
    }

    #[test]
    fn parses_cnmt_header_directly_without_directory_extraction() {
        let mut cnmt = Vec::new();
        cnmt.extend_from_slice(&0x0100_D2C0_115B_6800u64.to_le_bytes());
        cnmt.extend_from_slice(&131072u32.to_le_bytes());
        cnmt.push(0x81);
        let found = parse_cnmt_header(&cnmt).unwrap();
        assert_eq!(found.title_id.as_deref(), Some("0100D2C0115B6800"));
        assert_eq!(found.version, 131072);
        assert_eq!(found.content_type, "update");
        assert!(parse_cnmt_header(b"\0\0\0").is_none());
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

    /// Read-only native scan of representative NSP/NSZ game files. No tool binaries.
    #[test]
    #[ignore = "requires local game corpus, prod.keys and read-only library.db"]
    fn native_scan_real_files_without_external_tools() {
        use rusqlite::{Connection, OpenFlags};
        let reference = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("library.db");
        let conn =
            Connection::open_with_flags(reference, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut checked = 0;
        for extension in ["nsp", "nsz"] {
            let mut stmt = conn.prepare("SELECT gf.path, fc.title_id FROM game_files gf JOIN file_contents fc ON gf.path=fc.path WHERE lower(gf.file_type)=?1 AND fc.title_id IS NOT NULL ORDER BY gf.size LIMIT 8").unwrap();
            let paths = stmt
                .query_map([extension], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .unwrap()
                .map(|entry| entry.unwrap())
                .collect::<Vec<_>>();
            for (path, id) in paths {
                let file = Path::new(&path);
                if !file.is_file() {
                    continue;
                }
                let hash = fast_hash(file).unwrap();
                let scan = scan_file_hashed(
                    file,
                    file.file_name().unwrap().to_str().unwrap_or_default(),
                    hash,
                )
                .unwrap();
                assert!(!scan.contents.is_empty(), "{extension} metadata missing");
                assert!(
                    scan.contents
                        .iter()
                        .any(|c| c.title_id.as_deref() == Some(id.as_str())),
                    "{extension} title ID lost"
                );
                checked += 1;
            }
        }
        assert!(checked >= 2);
        eprintln!("NATIVE_SCAN_READ_ONLY_FILES={checked}");
    }
}
