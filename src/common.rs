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

pub fn version_to_str(version: Option<i64>) -> String {
    let Some(v) = version else { return "-".into() };
    if v == 0 {
        return "v1.0.0".into();
    }
    let (major, minor, patch) = ((v >> 26) & 0x3f, (v >> 20) & 0x3f, (v >> 16) & 0xf);
    if major != 0 || minor != 0 || patch != 0 {
        format!("v{major}.{minor}.{patch}")
    } else if v >= 65536 {
        format!("v0.0.0+{v}")
    } else {
        format!("v? ({v})")
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
