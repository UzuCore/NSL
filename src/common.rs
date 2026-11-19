use std::path::PathBuf;

pub fn base_dir() -> PathBuf {
    if cfg!(debug_assertions) {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    }
    let executable_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    resolve_data_dir(executable_dir)
}

/// Picks the folder that holds the app's data (`library.db`, `config.json`,
/// `titledb/`).
///
/// A packaged build keeps everything beside `nsl.exe`. When the binary sits in
/// `target/release` (or `dist`), the real data lives one or two levels up
/// in the project root, so look there first. Falling back to the executable
/// folder keeps true portable installs working.
fn resolve_data_dir(executable_dir: PathBuf) -> PathBuf {
    // Only known development build paths borrow the project-root database.
    // A portable installation must never adopt an unrelated parent library.
    let candidates: Vec<PathBuf> = match executable_dir.file_name().and_then(|v| v.to_str()) {
        Some("dist") | Some("dist-rust") => executable_dir
            .parent()
            .map(PathBuf::from)
            .into_iter()
            .collect(),
        Some("release")
            if executable_dir
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|v| v.to_str())
                == Some("target") =>
        {
            executable_dir
                .parent()
                .and_then(|p| p.parent())
                .map(PathBuf::from)
                .into_iter()
                .collect()
        }
        _ => Vec::new(),
    };
    for candidate in candidates {
        if candidate.join("library.db").is_file()
            && candidate.join("config.json").is_file()
            && candidate.join("titledb").is_dir()
        {
            return candidate;
        }
    }
    executable_dir
}

/// Shows a version exactly as stored: the raw Nintendo update number with a
/// `v` prefix (`524288` -> `v524288`).
///
/// The value is Nintendo's `N << 16` update ordinal, not a semantic version,
/// so it is displayed verbatim rather than decoded into fields that would read
/// as a version the game never shipped.
pub fn version_to_str(version: Option<i64>) -> String {
    match version {
        None => "-".into(),
        Some(v) => format!("v{v}"),
    }
}

pub fn format_size(n: u64) -> String {
    let mut value = n as f64;
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if value < 1024.0 {
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    format!("{value:.1} PB")
}

pub fn format_count(n: i64) -> String {
    let digits = n.max(0).to_string();
    let first_group = digits.len() % 3;
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    formatted.push_str(&digits[..first_group]);
    for (index, chunk) in digits.as_bytes()[first_group..].chunks(3).enumerate() {
        if first_group != 0 || index != 0 {
            formatted.push(',');
        }
        formatted.push_str(std::str::from_utf8(chunk).unwrap_or("000"));
    }
    formatted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_versions_as_the_raw_update_number() {
        // The library shows the stored Nintendo update number verbatim.
        assert_eq!(version_to_str(Some(524288)), "v524288");
        assert_eq!(version_to_str(Some(65536)), "v65536");
        assert_eq!(version_to_str(Some(0)), "v0");
        assert_eq!(version_to_str(None), "-");
    }

    #[test]
    fn formats_sizes_and_counts() {
        assert_eq!(format_size(0), "0.0 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(846), "846");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    /// Helper: build a fake data folder with the three markers.
    fn fake_data_dir(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("titledb")).unwrap();
        std::fs::write(root.join("library.db"), b"").unwrap();
        std::fs::write(root.join("config.json"), b"{}").unwrap();
    }

    #[test]
    fn release_binary_beside_target_uses_project_root_database() {
        // Regression: running `target/release/nsl.exe` used to resolve the data
        // folder to `target/release`, so the app scanned into a fresh empty
        // database and reported only the handful of titles it could parse.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fake_data_dir(root);
        let exe_dir = root.join("target").join("release");
        std::fs::create_dir_all(&exe_dir).unwrap();
        assert_eq!(resolve_data_dir(exe_dir), root);
    }

    #[test]
    fn dist_binary_uses_project_root_database() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fake_data_dir(root);
        let exe_dir = root.join("dist");
        std::fs::create_dir_all(&exe_dir).unwrap();
        assert_eq!(resolve_data_dir(exe_dir), root);
    }

    #[test]
    fn legacy_dist_path_still_resolves_existing_project_data() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fake_data_dir(root);
        let exe_dir = root.join("dist-rust");
        std::fs::create_dir_all(&exe_dir).unwrap();
        assert_eq!(resolve_data_dir(exe_dir), root);
    }

    #[test]
    fn portable_install_keeps_data_beside_the_executable() {
        // A shipped folder has no populated parent, so the executable folder
        // itself must be used.
        let dir = tempfile::tempdir().unwrap();
        let exe_dir = dir.path().join("NSL");
        std::fs::create_dir_all(&exe_dir).unwrap();
        fake_data_dir(&exe_dir);
        assert_eq!(resolve_data_dir(exe_dir.clone()), exe_dir);
    }

    #[test]
    fn nested_portable_install_does_not_take_parent_database() {
        let dir = tempfile::tempdir().unwrap();
        fake_data_dir(dir.path());
        let portable = dir.path().join("MyApp");
        std::fs::create_dir_all(&portable).unwrap();
        assert_eq!(resolve_data_dir(portable.clone()), portable);
    }
}
