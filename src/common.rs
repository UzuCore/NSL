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

/// Formats a Nintendo update number for display.
///
/// Nintendo numbers updates as `N << 16` (65536 = 1st update, 131072 = 2nd).
/// That integer is **not** a semantic version: 65536 decoded as major/minor/
/// patch yields "1.0.1", which no game ever shipped. The human-readable
/// version lives in the NACP display version, so anything here is an update
/// ordinal and is labelled as such.
pub fn version_to_str(version: Option<i64>) -> String {
    let Some(v) = version else { return "-".into() };
    if v <= 0 {
        return "v1.0.0".into();
    }
    let ordinal = v >> 16;
    if ordinal > 0 {
        format!("업데이트 {ordinal}")
    } else {
        // Not a plausible update number; show it verbatim rather than invent a
        // version out of its bits.
        format!("업데이트 ? ({v})")
    }
}

/// Formats the installed version of a package.
///
/// A combined base+update file carries both a real version string and an
/// update ordinal, so both are shown: "v1.0.0 · 업데이트 5". Showing only the
/// string would hide which patch is installed.
pub fn display_version(display: Option<&str>, update: Option<i64>) -> String {
    let text = display.map(str::trim).filter(|v| !v.is_empty());
    let ordinal = update.filter(|v| *v > 0).map(|v| v >> 16).filter(|v| *v > 0);
    match (text, ordinal) {
        (Some(text), Some(ordinal)) => format!("v{text} · 업데이트 {ordinal}"),
        (Some(text), None) => format!("v{text}"),
        (None, _) => version_to_str(update),
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
    fn update_numbers_are_not_decoded_as_semantic_versions() {
        // 65536 is Nintendo's 1st update. Decoding its bits as major.minor.patch
        // produced "v1.0.1", a version the game never shipped.
        assert_eq!(version_to_str(Some(65536)), "업데이트 1");
        assert_eq!(version_to_str(Some(131072)), "업데이트 2");
        assert_eq!(version_to_str(Some(458752)), "업데이트 7");
        assert_eq!(version_to_str(Some(0)), "v1.0.0");
        assert_eq!(version_to_str(None), "-");
        assert!(!version_to_str(Some(65536)).contains("1.0.1"));
    }

    #[test]
    fn display_version_prefers_the_nacp_string() {
        assert_eq!(display_version(Some("1.10"), Some(65536)), "v1.10 · 업데이트 1");
        assert_eq!(display_version(Some(" 1.0.3 "), None), "v1.0.3");
        assert_eq!(display_version(Some("   "), Some(131072)), "업데이트 2");
        assert_eq!(display_version(None, Some(65536)), "업데이트 1");
        // A base package (update 0) shows the version only.
        assert_eq!(display_version(Some("1.0.0"), Some(0)), "v1.0.0");
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
