use std::{env, process::Command};

fn git(args: &[&str]) -> Option<String> {
    let result = Command::new("git").args(args).output().ok()?;
    if !result.status.success() {
        return None;
    }
    let output = String::from_utf8(result.stdout).ok()?;
    Some(output.trim().to_owned())
}

fn valid_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/logs/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/master");
    println!("cargo:rerun-if-changed=.git/refs/heads/main");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");

    let commit = env::var("GITHUB_SHA")
        .ok()
        .filter(|value| valid_commit(value))
        .or_else(|| git(&["rev-parse", "HEAD"]).filter(|value| valid_commit(value)))
        .unwrap_or_else(|| "unknown".to_owned());
    let timestamp = git(&["log", "-1", "--format=%ct"])
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    println!("cargo:rustc-env=NSL_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=NSL_BUILD_COMMIT_TIME={timestamp}");
}
