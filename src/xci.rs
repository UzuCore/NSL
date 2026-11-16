use crate::scanner::{self, Content};
use regex::Regex;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
};

#[derive(Clone)]
struct Entry {
    offset: u64,
    size: u64,
    name: String,
}
struct Index {
    entries: Vec<Entry>,
    data_start: u64,
}

fn read_index(file: &mut File, base: u64, file_size: u64) -> Option<Index> {
    if base.checked_add(16)? > file_size || file.seek(SeekFrom::Start(base)).is_err() {
        return None;
    }
    let mut header = [0; 16];
    file.read_exact(&mut header).ok()?;
    if &header[..4] != b"PFS0" && &header[..4] != b"HFS0" {
        return None;
    }
    let count = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
    let strings_size = u32::from_le_bytes(header[8..12].try_into().ok()?) as usize;
    if count == 0 || count > 16384 || strings_size > 64 * 1024 * 1024 {
        return None;
    }
    let entry_size = if &header[..4] == b"HFS0" { 0x40 } else { 0x18 };
    let table_size = count.checked_mul(entry_size)?;
    let strings_at = base.checked_add(16)?.checked_add(table_size as u64)?;
    let data_start = strings_at.checked_add(strings_size as u64)?;
    if data_start > file_size {
        return None;
    }
    let mut table = vec![0; table_size];
    file.read_exact(&mut table).ok()?;
    let mut strings = vec![0; strings_size];
    file.read_exact(&mut strings).ok()?;
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let p = i * entry_size;
        let offset = u64::from_le_bytes(table[p..p + 8].try_into().ok()?);
        let size = u64::from_le_bytes(table[p + 8..p + 16].try_into().ok()?);
        let name_offset = u32::from_le_bytes(table[p + 16..p + 20].try_into().ok()?) as usize;
        if name_offset >= strings.len()
            || data_start.checked_add(offset)?.checked_add(size)? > file_size
        {
            continue;
        }
        let end = strings[name_offset..]
            .iter()
            .position(|b| *b == 0)
            .map(|n| name_offset + n)
            .unwrap_or(strings.len());
        let name = String::from_utf8_lossy(&strings[name_offset..end]).into_owned();
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
            continue;
        }
        entries.push(Entry { offset, size, name });
    }
    Some(Index {
        entries,
        data_start,
    })
}

fn root_index(file: &mut File, len: u64) -> Option<Index> {
    let mut candidates = Vec::new();
    for header_base in [0x1000u64, 0] {
        if file.seek(SeekFrom::Start(header_base + 0x100)).is_ok() {
            let mut magic = [0; 4];
            if file.read_exact(&mut magic).is_ok()
                && &magic == b"HEAD"
                && file.seek(SeekFrom::Start(header_base + 0x130)).is_ok()
            {
                let mut raw = [0; 8];
                if file.read_exact(&mut raw).is_ok() {
                    let addr = u64::from_le_bytes(raw);
                    if addr > 0 && addr < (1 << 40) {
                        candidates.push(addr);
                    }
                    if addr > 0 && addr < (1 << 32) {
                        candidates.push(addr.saturating_mul(0x200));
                    }
                }
            }
        }
    }
    candidates.extend([0x10000, 0xF000, 0x8000, 0x4000, 0x1000, 0]);
    let mut seen = std::collections::HashSet::new();
    for offset in candidates {
        if !seen.insert(offset) {
            continue;
        }
        if let Some(index) = read_index(file, offset, len) {
            if index
                .entries
                .iter()
                .any(|e| e.name.eq_ignore_ascii_case("secure"))
            {
                return Some(index);
            }
        }
    }
    None
}

fn extract(file: &mut File, index: &Index, entry: &Entry, target: &Path) -> anyhow::Result<()> {
    file.seek(SeekFrom::Start(index.data_start + entry.offset))?;
    let mut limited = file.take(entry.size);
    let mut out = File::create(target)?;
    std::io::copy(&mut limited, &mut out)?;
    out.flush()?;
    Ok(())
}

fn nacp_names(path: &Path) -> Option<(String, String, String)> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 0x300 * 15 {
        return None;
    }
    let name = |i: usize| {
        let field = &data[i * 0x300..i * 0x300 + 0x200];
        let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
        String::from_utf8_lossy(&field[..end])
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
    };
    let en = if name(0).is_empty() { name(1) } else { name(0) };
    let ja = name(2);
    let ko = name(12);
    if en.is_empty() && ja.is_empty() && ko.is_empty() {
        None
    } else {
        Some((ko, en, ja))
    }
}

fn decrypt_nca(nca: &Path, work: &Path) -> Option<PathBuf> {
    static KEY_RE: OnceLock<Regex> = OnceLock::new();
    let key_re = KEY_RE.get_or_init(|| Regex::new(r"(?i)AES-CTR Key:\s*([0-9a-f]{32})").unwrap());
    let output = scanner::run_nstool(nca, &["--showkeys".into()])?;
    let key = key_re.captures(&output)?.get(1)?.as_str().to_owned();
    let folder = work.join(format!("part0_{}", nca.file_stem()?.to_string_lossy()));
    std::fs::create_dir_all(&folder).ok()?;
    let args = vec![
        "--contentkey".into(),
        key,
        "--part0".into(),
        folder.to_string_lossy().into_owned(),
    ];
    scanner::run_nstool(nca, &args)?;
    Some(folder)
}

fn cnmt_content(path: &Path) -> Option<Content> {
    let mut file = File::open(path).ok()?;
    let mut header = [0; 13];
    file.read_exact(&mut header).ok()?;
    let tid = format!("{:016X}", u64::from_le_bytes(header[..8].try_into().ok()?));
    if tid == "0000000000000000" {
        return None;
    }
    let version = u32::from_le_bytes(header[8..12].try_into().ok()?) as i64;
    let kind = match header[12] {
        0x80 => "base",
        0x81 => "update",
        0x82 | 0x84 => "dlc",
        0x83 => "delta",
        _ => scanner::content_type(&tid),
    };
    Some(Content {
        title_id: Some(tid),
        version,
        content_type: kind.into(),
        source: "header_cnmt".into(),
    })
}

fn save_icon(data: &[u8], target: &Path) -> bool {
    let Ok(image) = image::load_from_memory(data) else {
        return false;
    };
    if image.width() == 0 || image.height() == 0 {
        return false;
    }
    let image = image
        .resize_exact(120, 120, image::imageops::FilterType::Lanczos3)
        .to_rgb8();
    if let Some(parent) = target.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let temp = target.with_extension("png.tmp");
    if image
        .save_with_format(&temp, image::ImageFormat::Png)
        .is_err()
    {
        return false;
    }
    if target.exists() && std::fs::remove_file(target).is_err() {
        let _ = std::fs::remove_file(temp);
        return false;
    }
    std::fs::rename(temp, target).is_ok()
}

fn package_index(file: &mut File, path: &Path, size: u64) -> Option<Index> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "xci" {
        let root = root_index(file, size)?;
        let secure = root
            .entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case("secure"))?;
        return read_index(file, root.data_start.checked_add(secure.offset)?, size);
    }
    let offsets = [0, 0x200, 0x400, 0x1000, 0x4000, 0x8000];
    offsets
        .iter()
        .find_map(|offset| read_index(file, *offset, size))
}

pub fn extract_local_icon(package: &Path, target: &Path) -> bool {
    let Ok(mut file) = File::open(package) else {
        return false;
    };
    let Ok(size) = file.metadata().map(|m| m.len()) else {
        return false;
    };
    let Some(index) = package_index(&mut file, package, size) else {
        return false;
    };
    let mut direct: Vec<Entry> = index
        .entries
        .iter()
        .filter(|e| {
            e.size > 1000
                && e.size <= 15 * 1024 * 1024
                && [".jpg", ".jpeg", ".png", ".bmp", ".dat"]
                    .iter()
                    .any(|ext| e.name.to_ascii_lowercase().ends_with(ext))
        })
        .cloned()
        .collect();
    direct.sort_by_key(|e| {
        let name = e.name.to_ascii_lowercase();
        if ["icon", "logo", "banner", "picture", "splash"]
            .iter()
            .any(|k| name.contains(k))
        {
            0
        } else {
            1
        }
    });
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    for (i, e) in direct.iter().enumerate() {
        let temp_file = temp.path().join(format!("direct_{i}"));
        if extract(&mut file, &index, e, &temp_file).is_ok() {
            if let Ok(data) = std::fs::read(&temp_file) {
                if save_icon(&data, target) {
                    return true;
                }
            }
        }
    }
    let mut controls: Vec<Entry> = index
        .entries
        .iter()
        .filter(|e| {
            e.name.to_ascii_lowercase().ends_with(".nca")
                && !e.name.to_ascii_lowercase().ends_with(".cnmt.nca")
                && e.size > 10 * 1024
                && e.size < 15 * 1024 * 1024
        })
        .cloned()
        .collect();
    controls.sort_by_key(|e| e.size);
    for (i, e) in controls.iter().enumerate() {
        let nca = temp.path().join(format!("control_{i}.nca"));
        if extract(&mut file, &index, e, &nca).is_err() {
            continue;
        }
        let part0 = decrypt_nca(&nca, temp.path()).or_else(|| {
            let section0 = temp.path().join(format!("hactool_control_{i}"));
            scanner::extract_nca_section0(&nca, &section0).then_some(section0)
        });
        let Some(part0) = part0 else {
            continue;
        };
        let files: Vec<_> = walkdir::WalkDir::new(&part0)
            .into_iter()
            .filter_map(|x| x.ok())
            .filter(|x| x.file_type().is_file())
            .collect();
        if !files.iter().any(|x| {
            x.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("nacp"))
                .unwrap_or(false)
        }) {
            continue;
        }
        let mut images: Vec<_> = files
            .into_iter()
            .filter(|x| {
                let n = x.file_name().to_string_lossy().to_ascii_lowercase();
                [".jpg", ".jpeg", ".png", ".bmp", ".dat"]
                    .iter()
                    .any(|e| n.ends_with(e))
                    && std::fs::metadata(x.path())
                        .map(|m| m.len() > 1000)
                        .unwrap_or(false)
            })
            .collect();
        images.sort_by_key(|x| {
            let n = x.file_name().to_string_lossy().to_ascii_lowercase();
            if ["icon", "logo", "banner", "picture", "splash"]
                .iter()
                .any(|k| n.contains(k))
            {
                0
            } else {
                1
            }
        });
        for image in images {
            if let Ok(data) = std::fs::read(image.path()) {
                if save_icon(&data, target) {
                    return true;
                }
            }
        }
    }
    false
}

pub fn scan(path: &Path) -> (Vec<Content>, (String, String, String)) {
    let empty = (Vec::new(), (String::new(), String::new(), String::new()));
    let Ok(mut file) = File::open(path) else {
        return empty;
    };
    let Ok(file_size) = file.metadata().map(|m| m.len()) else {
        return empty;
    };
    let Some(root) = root_index(&mut file, file_size) else {
        return empty;
    };
    let Some(secure) = root
        .entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case("secure"))
    else {
        return empty;
    };
    let Some(secure_base) = root.data_start.checked_add(secure.offset) else {
        return empty;
    };
    let Some(secure_index) = read_index(&mut file, secure_base, file_size) else {
        return empty;
    };
    let mut metadata: Vec<Entry> = secure_index
        .entries
        .iter()
        .filter(|e| e.size > 0 && e.name.to_ascii_lowercase().ends_with(".cnmt.nca"))
        .cloned()
        .collect();
    if metadata.is_empty() {
        if let Some(smallest) = secure_index
            .entries
            .iter()
            .filter(|e| e.size > 0 && e.name.to_ascii_lowercase().ends_with(".nca"))
            .min_by_key(|e| e.size)
        {
            metadata.push(smallest.clone());
        }
    }
    let controls: Vec<Entry> = secure_index
        .entries
        .iter()
        .filter(|e| {
            let name = e.name.to_ascii_lowercase();
            name.ends_with(".nca")
                && !name.ends_with(".cnmt.nca")
                && e.size > 10 * 1024
                && e.size < 15 * 1024 * 1024
        })
        .cloned()
        .collect();
    if metadata.is_empty() && controls.is_empty() {
        return empty;
    }
    let Ok(temp) = tempfile::tempdir() else {
        return empty;
    };
    let mut contents = Vec::new();
    let mut names = (String::new(), String::new(), String::new());
    for (i, entry) in metadata.iter().enumerate() {
        let nca = temp.path().join(format!("meta_{i}.nca"));
        if extract(&mut file, &secure_index, entry, &nca).is_err() {
            continue;
        }
        if let Some(part0) = decrypt_nca(&nca, temp.path()) {
            for file_entry in walkdir::WalkDir::new(&part0)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file())
            {
                if file_entry
                    .path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("cnmt"))
                    .unwrap_or(false)
                {
                    if let Some(item) = cnmt_content(file_entry.path()) {
                        if !contents
                            .iter()
                            .any(|c: &Content| c.title_id == item.title_id)
                        {
                            contents.push(item);
                        }
                    }
                }
            }
        }
    }
    for (i, entry) in controls.iter().enumerate() {
        let nca = temp.path().join(format!("control_{i}.nca"));
        if extract(&mut file, &secure_index, entry, &nca).is_err() {
            continue;
        }
        if let Some(part0) = decrypt_nca(&nca, temp.path()) {
            for file_entry in walkdir::WalkDir::new(&part0)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file())
            {
                if file_entry
                    .path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("nacp"))
                    .unwrap_or(false)
                {
                    if let Some(parsed) = nacp_names(file_entry.path()) {
                        names = parsed;
                        break;
                    }
                }
            }
            if !names.0.is_empty() || !names.1.is_empty() || !names.2.is_empty() {
                break;
            }
        }
    }
    (contents, names)
}
