use llaia::cron::{CronConfig, CronMode};
use serde_json::json;

#[test]
fn test_parse_agent_mode_task() {
    let toml = r#"
[[task]]
id = "morning_news"
schedule = "0 8 * * *"
mode = "agent"
channel = "qq"
enabled = true
prompt = "查今天的 AI 新闻"
"#;
    let cfg: CronConfig = toml::from_str(toml).unwrap();
    assert_eq!(cfg.task.len(), 1);
    let t = &cfg.task[0];
    assert_eq!(t.id, "morning_news");
    assert_eq!(t.schedule, "0 8 * * *");
    assert!(matches!(t.mode, CronMode::Agent));
    assert_eq!(t.channel, "qq");
    assert!(t.enabled);
    assert_eq!(t.prompt.as_deref(), Some("查今天的 AI 新闻"));
    assert!(t.steps.is_none());
}

#[test]
fn test_parse_tools_mode_task() {
    let toml = r#"
[[task]]
id = "health_check"
schedule = "*/30 * * * *"
mode = "tools"
channel = "web"
enabled = true
steps = [
  { tool = "tavily_search", args = { query = "llaia" } },
  { tool = "memory_write", args = { text = "checked at {{now}}" } },
]
"#;
    let cfg: CronConfig = toml::from_str(toml).unwrap();
    let t = &cfg.task[0];
    assert!(matches!(t.mode, CronMode::Tools));
    let steps = t.steps.as_ref().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].tool, "tavily_search");
    assert_eq!(steps[0].args, json!({"query": "llaia"}));
}

#[test]
fn test_parse_empty_config() {
    let cfg: CronConfig = toml::from_str("").unwrap();
    assert!(cfg.task.is_empty());
}

#[test]
fn test_parse_disabled_task() {
    let toml = r#"
[[task]]
id = "disabled_task"
schedule = "0 0 * * *"
mode = "agent"
channel = "qq"
enabled = false
prompt = "test"
"#;
    let cfg: CronConfig = toml::from_str(toml).unwrap();
    assert!(!cfg.task[0].enabled);
}

#[test]
fn test_default_enabled_is_true() {
    let toml = r#"
[[task]]
id = "no_enabled_field"
schedule = "0 0 * * *"
mode = "agent"
channel = "qq"
prompt = "x"
"#;
    let cfg: CronConfig = toml::from_str(toml).unwrap();
    assert!(cfg.task[0].enabled);
}

#[test]
fn test_load_missing_file_returns_empty() {
    let cfg = CronConfig::load(std::path::Path::new("/nonexistent/path/cron.toml")).unwrap();
    assert!(cfg.task.is_empty());
}

#[test]
fn test_step_args_default_empty_when_omitted() {
    let toml = r#"
[[task]]
id = "t"
schedule = "0 0 * * *"
mode = "tools"
channel = "web"
steps = [
  { tool = "terminal" },
]
"#;
    let cfg: CronConfig = toml::from_str(toml).unwrap();
    let steps = cfg.task[0].steps.as_ref().unwrap();
    assert_eq!(steps[0].tool, "terminal");
    assert_eq!(steps[0].args, json!({}));
}

#[test]
fn test_to_toml_roundtrip() {
    let toml = r#"
[[task]]
id = "rt"
schedule = "0 8 * * *"
mode = "tools"
channel = "web"
enabled = true
steps = [{ tool = "memory_write", args = { text = "hi" } }]
"#;
    let cfg: CronConfig = toml::from_str(toml).unwrap();
    let out = cfg.to_toml().unwrap();
    let cfg2: CronConfig = toml::from_str(&out).unwrap();
    assert_eq!(cfg2.task.len(), 1);
    assert_eq!(cfg2.task[0].id, "rt");
    assert!(matches!(cfg2.task[0].mode, CronMode::Tools));
}
