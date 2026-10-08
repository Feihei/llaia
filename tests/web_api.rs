use axum::http::HeaderMap;
use llaia::config::Config;
use llaia::web::{
    check_token, extract_token, generate_token, mask_sensitive, merge_masked, resolve_within,
    upload_name_or_err,
};

// H5（2026-10-08）：uploads 手动清理——文件名校验拒绝一切穿越形态
#[test]
fn test_upload_name_rejects_traversal() {
    assert!(upload_name_or_err("../secret.txt").is_err());
    assert!(upload_name_or_err("a/b.png").is_err());
    assert!(upload_name_or_err("a\\b.png").is_err());
    assert!(upload_name_or_err("..").is_err());
    assert!(upload_name_or_err(".").is_err());
    assert!(upload_name_or_err("").is_err());
    assert!(upload_name_or_err("bad\nname.txt").is_err());
}

#[test]
fn test_upload_name_accepts_plain_filenames() {
    assert!(upload_name_or_err("1749020000_report.pdf").is_ok());
    assert!(upload_name_or_err("img photo (1).png").is_ok());
    assert!(upload_name_or_err("中文名.jpg").is_ok());
}

#[test]
fn test_resolve_within_rejects_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(resolve_within(tmp.path(), "../../etc/passwd").is_err());
}

#[test]
fn test_resolve_within_accepts_inside() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("uploads")).unwrap();
    std::fs::write(tmp.path().join("uploads/a.png"), b"x").unwrap();
    assert!(resolve_within(tmp.path(), "uploads/a.png").is_ok());
}

#[test]
fn test_check_token() {
    let t = generate_token();
    assert!(check_token(&t, &t));
    assert!(!check_token("wrong", &t));
}

#[test]
fn test_extract_token_bearer() {
    let mut h = HeaderMap::new();
    h.insert("authorization", "Bearer abc".parse().unwrap());
    assert_eq!(extract_token(&h, "", None).as_deref(), Some("abc"));
}

#[test]
fn test_mask_sensitive_redacts() {
    let mut c = Config::default_for_workspace("/tmp/x");
    c.provider.get_mut("default").unwrap().api_key = "sk-x".into();
    let m = mask_sensitive(c);
    assert_eq!(m.provider.get("default").unwrap().api_key, "••••");
}

#[test]
fn test_merge_masked_preserves_secret() {
    let mut old = Config::default_for_workspace("/tmp/x");
    old.provider.get_mut("default").unwrap().api_key = "sk-orig".into();
    let mut new = old.clone();
    new.provider.get_mut("default").unwrap().api_key = "••••".into();
    let merged = merge_masked(&old, &new);
    assert_eq!(merged.provider.get("default").unwrap().api_key, "sk-orig");
}

#[test]
fn test_web_config_round_trip() {
    let mut c = Config::default_for_workspace("/tmp/x");
    c.webui.host = "0.0.0.0".into();
    c.webui.port = 9999;
    let s = toml::to_string(&c).unwrap();
    let parsed: Config = toml::from_str(&s).unwrap();
    assert_eq!(parsed.webui.host, "0.0.0.0");
    assert_eq!(parsed.webui.port, 9999);
}
