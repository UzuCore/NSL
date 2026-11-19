use crate::{
    native_nca,
    scanner::{self, Content},
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
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
    let mut bytes = std::io::Cursor::new(Vec::new());
    if image::DynamicImage::ImageRgb8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .is_err()
    {
        return false;
    }
    crate::database::atomic_write(target, &bytes.into_inner()).is_ok()
}

fn package_index(file: &mut File, path: &Path, size: u64) -> Option<Index> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "xci" || ext == "xcz" {
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

/// Extract a cover directly from the package without external executables.
pub fn extract_local_icon(package: &Path, target: &Path) -> bool {
    let Ok(mut source) = File::open(package) else {
        return false;
    };
    let Ok(size) = source.metadata().map(|m| m.len()) else {
        return false;
    };
    let Some(index) = package_index(&mut source, package, size) else {
        return false;
    };
    // Homebrew archives occasionally embed a plain icon image.
    let mut direct: Vec<_> = index
        .entries
        .iter()
        .filter(|e| {
            e.size > 1000
                && e.size <= 8 * 1024 * 1024
                && [".jpg", ".jpeg", ".png", ".bmp", ".dat"]
                    .iter()
                    .any(|ext| e.name.to_ascii_lowercase().ends_with(ext))
        })
        .collect();
    direct.sort_by_key(|e| {
        let name = e.name.to_ascii_lowercase();
        if ["icon", "logo", "banner", "picture"]
            .iter()
            .any(|key| name.contains(key))
        {
            0
        } else {
            1
        }
    });
    for entry in direct {
        let Some(start) = index.data_start.checked_add(entry.offset) else {
            continue;
        };
        if source.seek(SeekFrom::Start(start)).is_err() {
            continue;
        }
        let mut buffer = vec![0u8; entry.size as usize];
        if source.read_exact(&mut buffer).is_ok() && save_icon(&buffer, target) {
            return true;
        }
    }
    let Some(keys) = scanner::prod_keys().and_then(|p| native_nca::cached_keys(&p)) else {
        return false;
    };
    let mut controls: Vec<_> = index
        .entries
        .iter()
        .filter(|e| {
            let name = e.name.to_ascii_lowercase();
            (name.ends_with(".nca") || name.ends_with(".ncz"))
                && !name.contains(".cnmt.")
                && (10 * 1024..=256 * 1024 * 1024).contains(&e.size)
        })
        .collect();
    controls.sort_by_key(|e| e.size);
    for entry in controls.into_iter().take(32) {
        let Some(start) = index.data_start.checked_add(entry.offset) else {
            continue;
        };
        if native_nca::content_kind(&mut source, start, entry.size, &keys) != Some(2) {
            continue;
        }
        if let Some(data) = native_nca::extract_icon_package(
            &mut source,
            start,
            entry.size,
            entry.name.to_ascii_lowercase().ends_with(".ncz"),
            &keys,
        ) {
            if save_icon(&data, target) {
                return true;
            }
        }
    }
    false
}

/// Read the CNMT metadata from every game-card partition, directly from the
/// original file. The old XCI reader only scanned `secure` and XCZ never
/// reached this module at all; merged games therefore lost updates and DLC.
/// Native NCA decryption reads only small headers and metadata from the
/// original stream; no multi-GiB NCA file is copied into a temporary folder.
fn scan_native_card(
    file: &mut File,
    root: &Index,
    file_size: u64,
) -> Option<(Vec<Content>, (String, String, String))> {
    let keys = scanner::prod_keys().and_then(|p| native_nca::cached_keys(&p))?;
    let mut contents = Vec::<Content>::new();
    let mut names = (String::new(), String::new(), String::new());
    let mut metadata_seen = 0usize;
    let mut metadata_failed = false;

    // Base game names come from `secure`. Updates and DLC can also be stored
    // in `update` and `normal`, particularly in merged XCZ game-card images.
    for partition_name in ["secure", "update", "normal"] {
        let Some(partition) = root
            .entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(partition_name))
        else {
            continue;
        };
        let partition_base = root.data_start.checked_add(partition.offset)?;
        let partition_end = partition_base.checked_add(partition.size)?;
        if partition_end > file_size {
            return None;
        }
        let Some(index) = read_index(file, partition_base, file_size) else {
            // XCI images commonly contain empty 512-byte `normal`/`update`
            // partitions. They are valid and do not invalidate `secure`.
            if partition.size <= 0x1000 {
                continue;
            }
            return None;
        };
        // CNMT content records identify their matching Control NCA directly.
        // Prefer linked archives when a combined image has multiple titles.
        let mut linked_controls = Vec::<String>::new();
        for entry in index.entries.iter().filter(|e| {
            let name = e.name.to_ascii_lowercase();
            (name.ends_with(".cnmt.nca") || name.ends_with(".cnmt.ncz")) && e.size >= 0xc00
        }) {
            let start = index.data_start.checked_add(entry.offset)?;
            if start.checked_add(entry.size)? > partition_end {
                return None;
            }
            metadata_seen += 1;
            match native_nca::extract_cnmt_controls_package(
                file,
                start,
                entry.size,
                entry.name.to_ascii_lowercase().ends_with(".ncz"),
                &keys,
            ) {
                Some(found) => {
                    for record in found {
                        let mut content = record.content;
                        if let Some(control_id) = record.control_nca_id {
                            if content.content_type == "base" {
                                linked_controls.insert(0, control_id);
                            } else if content.content_type == "update" {
                                linked_controls.push(control_id);
                            }
                        }
                        content.source = "header_native_card".into();
                        if let Some(saved) = contents
                            .iter_mut()
                            .find(|saved| saved.title_id == content.title_id)
                        {
                            if content.version > saved.version {
                                *saved = content;
                            }
                        } else {
                            contents.push(content);
                        }
                    }
                }
                None => metadata_failed = true,
            }
        }
        if names.0.is_empty() && names.1.is_empty() && names.2.is_empty() {
            let mut candidates = index
                .entries
                .iter()
                .filter(|e| {
                    e.size >= 0xc00
                        && (e.name.to_ascii_lowercase().ends_with(".nca")
                            || e.name.to_ascii_lowercase().ends_with(".ncz"))
                        && !e.name.to_ascii_lowercase().contains(".cnmt.")
                })
                .collect::<Vec<_>>();
            candidates.sort_by_key(|e| {
                let preferred = linked_controls.iter().position(|id| {
                    e.name
                        .get(..32)
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(id))
                });
                (preferred.unwrap_or(usize::MAX), e.size)
            });
            for entry in candidates {
                let start = index.data_start.checked_add(entry.offset)?;
                if start.checked_add(entry.size)? > partition_end {
                    continue;
                }
                if native_nca::content_kind(file, start, entry.size, &keys) != Some(2) {
                    continue;
                }
                if let Some(found) = native_nca::extract_nacp_package(
                    file,
                    start,
                    entry.size,
                    entry.name.to_ascii_lowercase().ends_with(".ncz"),
                    &keys,
                ) {
                    names = found;
                    break;
                }
            }
        }
    }
    // If even one metadata archive is unsupported, use the established
    // legacy analyzer instead of silently presenting a partial merged title.
    (metadata_seen != 0 && !metadata_failed && !contents.is_empty()).then_some((contents, names))
}

/// Read game-card metadata from the source stream without subprocesses.
/// Unsupported protected layouts return no guessed metadata; the enclosing
/// library scanner retains its filename-based fallback and flags the source.
pub fn scan(path: &Path) -> (Vec<Content>, (String, String, String)) {
    let empty = (Vec::new(), (String::new(), String::new(), String::new()));
    let Ok(mut file) = File::open(path) else {
        return empty;
    };
    let Ok(size) = file.metadata().map(|m| m.len()) else {
        return empty;
    };
    let Some(root) = root_index(&mut file, size) else {
        return empty;
    };
    scan_native_card(&mut file, &root, size).unwrap_or(empty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    #[ignore = "requires local read-only game corpus and prod.keys"]
    fn native_xci_xcz_samples_from_library_database() {
        use rusqlite::{Connection, OpenFlags};
        let db = Connection::open_with_flags(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("library.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        for ext in ["xci", "xcz"] {
            let paths = db
                .prepare(
                    "SELECT path FROM game_files WHERE lower(file_type)=?1 ORDER BY size LIMIT 2",
                )
                .unwrap()
                .query_map([ext], |row| row.get::<_, String>(0))
                .unwrap()
                .map(|row| row.unwrap())
                .collect::<Vec<_>>();
            assert!(!paths.is_empty());
            for path in paths {
                let original = Path::new(&path);
                let (contents, names) = scan(original);
                eprintln!(
                    "NATIVE_CARD_SCAN {ext}: contents={} ko={} en={} ja={}",
                    contents.len(),
                    !names.0.is_empty(),
                    !names.1.is_empty(),
                    !names.2.is_empty()
                );
                assert!(!contents.is_empty(), "{ext} metadata missing");
                assert!(contents
                    .iter()
                    .all(|record| record.title_id.as_ref().is_some_and(|id| id.len() == 16)));
                assert!(
                    contents.iter().any(|record| record.content_type == "base"),
                    "{ext} base metadata missing"
                );
            }
        }
    }

    #[test]
    #[ignore = "uses local read-only NSZ Control NCA and temporary icon folder"]
    fn native_icon_from_real_nsz_control_archive() {
        use rusqlite::{Connection, OpenFlags};
        let db = Connection::open_with_flags(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("library.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let path: String = db
            .query_row(
                "SELECT path FROM game_files WHERE lower(file_type)='nsz' ORDER BY size LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("icon.png");
        let succeeded = extract_local_icon(Path::new(&path), &target);
        eprintln!("NATIVE_LOCAL_ICON_SUCCESS={succeeded}");
        if succeeded {
            let image = image::open(target).unwrap();
            assert_eq!(image.width(), 120);
            assert_eq!(image.height(), 120);
        }
    }

    /// Optional regression test: point NSL_XCI_QA_DIR at a directory containing
    /// XCI/XCZ images. Input files are only opened read-only, never copied.
    #[test]
    #[ignore = "set NSL_XCI_QA_DIR to test local XCI/XCZ files"]
    fn real_xci_and_merged_xcz_keep_all_game_card_partitions() {
        let Some(folder) = std::env::var_os("NSL_XCI_QA_DIR") else {
            return;
        };
        let files = std::fs::read_dir(PathBuf::from(folder))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| {
                            ext.eq_ignore_ascii_case("xci") || ext.eq_ignore_ascii_case("xcz")
                        })
            })
            .collect::<Vec<_>>();
        assert!(
            !files.is_empty(),
            "QA directory does not contain game cards"
        );
        let mut xci_count = 0usize;
        let mut xcz_count = 0usize;
        let started = std::time::Instant::now();
        for path in &files {
            let ext = path
                .extension()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase();
            let before = std::fs::metadata(path).unwrap();
            let (contents, names) = scan(path);
            assert!(!contents.is_empty(), "{ext}: missing metadata");
            assert!(
                contents.iter().any(|c| c.content_type == "base"),
                "{ext}: missing base"
            );
            assert!(
                contents.iter().all(|c| c.source == "header_native_card"),
                "{ext}: fell back to filename instead of NCA metadata"
            );
            assert!(
                contents
                    .iter()
                    .all(|c| c.title_id.as_ref().is_some_and(|id| id.len() == 16)),
                "{ext}: invalid title ID"
            );
            assert!(
                !names.0.is_empty() || !names.1.is_empty() || !names.2.is_empty(),
                "{ext}: missing localized NACP name"
            );

            let filename = path.file_name().unwrap().to_string_lossy();
            let scanned =
                scanner::scan_file_hashed(path, &filename, scanner::fast_hash(path).unwrap())
                    .unwrap();
            let to_set = |entries: Vec<Content>| {
                entries
                    .into_iter()
                    .map(|c| (c.title_id, c.version, c.content_type))
                    .collect::<std::collections::HashSet<_>>()
            };
            assert_eq!(
                to_set(scanned.contents),
                to_set(contents.clone()),
                "{ext}: scanner discarded game-card metadata"
            );
            assert_eq!((scanned.nacp_ko, scanned.nacp_en, scanned.nacp_ja), names);
            if ext == "xcz" {
                xcz_count += 1;
                assert!(
                    contents
                        .iter()
                        .any(|c| c.content_type == "update" && c.version > 0),
                    "XCZ merged update partition missing"
                );
                if filename.contains("1DLC") {
                    assert!(
                        contents.iter().any(|c| c.content_type == "dlc"),
                        "XCZ merged DLC partition missing"
                    );
                }
            } else {
                xci_count += 1;
            }
            let after = std::fs::metadata(path).unwrap();
            assert_eq!(before.len(), after.len(), "input size changed");
            assert_eq!(
                before.modified().ok(),
                after.modified().ok(),
                "input mtime changed"
            );
            eprintln!(
                "READ_ONLY_GAME_CARD_QA ext={ext} contents={} en_name={} elapsed={:.2}s",
                contents.len(),
                !names.1.is_empty(),
                started.elapsed().as_secs_f64()
            );
        }
        assert!(xci_count > 0 && xcz_count > 0, "test both XCI and XCZ");
        eprintln!(
            "REAL_GAME_CARD_QA_PASS xci={xci_count} xcz={xcz_count} total_s={:.2}",
            started.elapsed().as_secs_f64()
        );
    }
}
