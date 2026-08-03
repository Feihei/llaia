use axum::http::HeaderMap;
use llaia::channels::web::{check_token, extract_token, generate_token, resolve_within};

#[test]
fn test_resolve_within_rejects_traversal_integration() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(resolve_within(tmp.path(), "../../etc/passwd").is_err());
}

#[test]
fn test_check_token_integration() {
    let t = generate_token();
    assert!(check_token(&t, &t));
    assert!(!check_token("wrong", &t));
}

#[test]
fn test_extract_token_priority() {
    let mut h = HeaderMap::new();
    h.insert("authorization", "Bearer from-header".parse().unwrap());
    assert_eq!(extract_token(&h, "", None).as_deref(), Some("from-header"));
}
