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
    if executable_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("dist-rust"))
    {
        if let Some(workspace_dir) = executable_dir.parent() {
            if workspace_dir.join("library.db").is_file()
                && workspace_dir.join("config.json").is_file()
            {
                return workspace_dir.to_path_buf();
            }
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
}
