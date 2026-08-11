//! P3-e Skill 框架集成测试：loader 种子 + system prompt 注入 + WebUI Skills API。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use llaia::web::{build_system_routes, AppState};
use std::path::PathBuf;
use std::sync::Arc;
use tower::util::ServiceExt;

// ───────────────────────── loader / prompt ─────────────────────────

#[test]
fn test_load_skills_seeds_examples_once() {
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    // 首次加载：种子 3 个内置示例
    let skills = llaia::skill::loader::load_skills(&skills_dir);
    assert_eq!(skills.len(), 3);
    // skills.json 生成且全部 active
    let json_path = skills_dir.with_file_name("skills.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(json_path).unwrap()).unwrap();
    assert_eq!(json["skills"]["news-digest"]["active"], true);
    // 目录已存在时不再种子：手动删掉一个示例，再 load 不会恢复
    std::fs::remove_dir_all(skills_dir.join("todoist")).unwrap();
    let skills = llaia::skill::loader::load_skills(&skills_dir);
    assert_eq!(skills.len(), 2);
}

#[test]
fn test_skills_prompt_injected_with_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    let skills = llaia::skill::loader::load_skills(&skills_dir);
    let prompt = llaia::skill::prompt::build_skills_prompt(&skills);
    assert!(prompt.contains("## Skills"));
    assert!(prompt.contains("code-review"));
    // 路径为 SKILL.md 绝对路径，供 LLM file_read
    let expected = skills_dir.join("code-review").join("SKILL.md");
    assert!(prompt.contains(&expected.display().to_string()));
}

// ───────────────────────── Skills Web API ─────────────────────────

const TOKEN: &str = "test-token";

/// 构建最小 AppState：降级模式 agent（无 provider）+ 临时 config_dir
async fn build_state(tmp: &std::path::Path) -> AppState {
    use llaia::agent::runner::ToolRegistry;
    use llaia::agent::{Agent, AgentRegistry};
    use llaia::memory::sqlite::SessionStore;

    let config_dir = tmp.to_path_buf();
    let workspace = config_dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let store = SessionStore::open_in_memory().unwrap();
    let sid = store.create_session("test", "test").unwrap();
    let config = llaia::config::Config::default_for_workspace(&workspace.display().to_string());
    let agent = Agent::new(
        &config,
        None,
        None,
        Arc::new(ToolRegistry::new()),
        Arc::new(store),
        sid,
        "sys".into(),
        8192,
        workspace.clone(),
        config_dir.clone(),
        true,
        "main".into(),
        None,
    )
    .await;
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(tokio::sync::Mutex::new(agent)),
        workspace.clone(),
    ));

    AppState {
        registry,
        config: Arc::new(tokio::sync::RwLock::new(config)),
        config_path: config_dir.join("config.toml"),
        workspace,
        token: Arc::new(TOKEN.to_string()),
        shutdown_signal: Arc::new(tokio::sync::Notify::new()),
        active_ws: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        next_ws_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        cron_path: config_dir.join("cron.toml"),
        cron_scheduler: None,
        mcp_path: config_dir.join("mcp.toml"),
        mcp_registry: None,
        skills_dir: config_dir.join("skills"),
    }
}

fn app(state: AppState) -> axum::Router {
    build_system_routes().with_state(state)
}

async fn body_json(res: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn test_skills_api_full_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path()).await;
    let skills_dir: PathBuf = state.skills_dir.clone();

    // 1. 初始列表为空（skills 目录不存在，scan 不种子）
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/skills?token={}", TOKEN))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res).await;
    assert_eq!(json["skills"].as_array().unwrap().len(), 0);

    // 2. 无 token 拒绝
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/skills")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 3. 创建 skill（默认模板）
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/skills?token={}", TOKEN))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name": "demo"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(skills_dir.join("demo").join("SKILL.md").exists());

    // 4. 非法 name 拒绝（路径穿越）
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/skills?token={}", TOKEN))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name": "../evil"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 5. 列表出现 demo 且 active=true
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/skills?token={}", TOKEN))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(res).await;
    let skills = json["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0]["name"], "demo");
    assert_eq!(skills[0]["active"], true);

    // 6. 切换 active=false → skills.json 落盘
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/skills/demo/active?token={}", TOKEN))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"active": false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json_path = skills_dir.with_file_name("skills.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
    assert_eq!(json["skills"]["demo"]["active"], false);

    // 7. 读 content
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/skills/demo/content?token={}", TOKEN))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res).await;
    assert!(json["content"].as_str().unwrap().contains("name: demo"));

    // 8. 写 content：非法（无 frontmatter）→ 400
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/skills/demo/content?token={}", TOKEN))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"content": "plain text"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 9. 写 content：合法 → 200 且落盘
    let new_content = "---\nname: demo\ndescription: updated\n---\n\n# Demo\n";
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/skills/demo/content?token={}", TOKEN))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"content": new_content}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        std::fs::read_to_string(skills_dir.join("demo").join("SKILL.md"))
            .unwrap()
            .contains("description: updated")
    );

    // 10. 删除 skill
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/skills/demo?token={}", TOKEN))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(!skills_dir.join("demo").exists());
    // skills.json 条目一并清理
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
    assert!(json["skills"].get("demo").is_none());

    // 11. 删除不存在的 skill → 404
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/skills/demo?token={}", TOKEN))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
