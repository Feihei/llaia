fn main() {
    println!("cargo:rustc-env=GIT_HASH={}", git_hash());
    println!("cargo:rerun-if-changed=.git/HEAD");
}

fn git_hash() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
