fn main() {
    println!("cargo:rustc-env=GIT_HASH={}", git_hash());
    println!("cargo:rerun-if-changed=.git/HEAD");
    // 重新嵌前沿：src/web/static 内的静态文件（index.html/app.js/theme.css 等）变更时
    // 必须触发重新编译，否则 rust-embed 会沿用增量缓存里的旧副本。
    println!("cargo:rerun-if-changed=src/web/static");
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
