//! 敏感信息 .env 自动化（P5 S1）。
//!
//! 保存配置时把明文敏感字段自动转存到 `<config_dir>/.env`，config.toml 只保留
//! `${VAR}` 引用（`expand_string` 已有展开机制，见 `src/config.rs`）。
//!
//! 流程（`PUT /api/config` 集成处）：
//! 1. `collect_plaintext_secrets` 收集所有明文敏感字段 → `(var, value)` 候选
//! 2. 先 `upsert_env` 写入 .env（幂等，保留注释与无关行）——**成功才应用引用**
//! 3. `apply_refs` 把对应字段替换为 `${VAR}`，config.toml 不再落明文
//!
//! 降级：.env 写入失败 → 保留明文 + warn（不阻断配置保存，保证服务可用）。

use crate::config::Config;
use anyhow::{anyhow, Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// 敏感字段位置标识（provider 字段带动态 id）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretField {
    ProviderApiKey,
    QqAppSecret,
    TelegramBotToken,
    DingtalkClientSecret,
    MailImapPass,
    MailSmtpPass,
    FeishuAppSecret,
    TavilyApiKey,
    BaiduApiKey,
    BraveApiKey,
    WebuiToken,
}

/// 一条待转存的敏感字段。
#[derive(Debug, Clone)]
pub struct SecretEntry {
    pub field: SecretField,
    /// provider 类字段的 provider id（其余为 None）。
    pub provider_id: Option<String>,
    /// .env 变量名（`LLAIA_<...>`）。
    pub var: String,
    /// 明文值。
    pub value: String,
}

/// 是否已是 `${VAR}` 引用（引用则跳过转存）。
fn is_plaintext(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let re = regex::Regex::new(r"^\$\{[A-Z_][A-Z0-9_]*\}$").unwrap();
    !re.is_match(s)
}

/// provider id → .env 变量名（大写 + 非字母数字转 `_`）。
fn provider_var(id: &str) -> String {
    let stem: String = id
        .to_uppercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("LLAIA_PROVIDER_{}_API_KEY", stem)
}

/// 收集 config 中所有明文敏感字段（非空且非 `${VAR}` 引用）。
pub fn collect_plaintext_secrets(cfg: &Config) -> Vec<SecretEntry> {
    let mut out = Vec::new();
    for (id, p) in &cfg.provider {
        if is_plaintext(&p.api_key) {
            let var = provider_var(id);
            out.push(SecretEntry {
                field: SecretField::ProviderApiKey,
                provider_id: Some(id.clone()),
                var,
                value: p.api_key.clone(),
            });
        }
    }
    macro_rules! push {
        ($field:expr, $var:expr, $value:expr) => {
            if is_plaintext($value) {
                out.push(SecretEntry {
                    field: $field,
                    provider_id: None,
                    var: $var.into(),
                    value: $value.clone(),
                });
            }
        };
    }
    push!(
        SecretField::QqAppSecret,
        "LLAIA_QQ_APP_SECRET",
        &cfg.channels.qq.app_secret
    );
    push!(
        SecretField::TelegramBotToken,
        "LLAIA_TELEGRAM_BOT_TOKEN",
        &cfg.channels.telegram.bot_token
    );
    push!(
        SecretField::DingtalkClientSecret,
        "LLAIA_DINGTALK_CLIENT_SECRET",
        &cfg.channels.dingtalk.client_secret
    );
    push!(
        SecretField::MailImapPass,
        "LLAIA_MAIL_IMAP_PASS",
        &cfg.channels.mail.imap_pass
    );
    push!(
        SecretField::MailSmtpPass,
        "LLAIA_MAIL_SMTP_PASS",
        &cfg.channels.mail.smtp_pass
    );
    push!(
        SecretField::FeishuAppSecret,
        "LLAIA_FEISHU_APP_SECRET",
        &cfg.channels.feishu.app_secret
    );
    push!(
        SecretField::TavilyApiKey,
        "LLAIA_TAVILY_API_KEY",
        &cfg.tools.tavily.api_key
    );
    push!(
        SecretField::BaiduApiKey,
        "LLAIA_BAIDU_API_KEY",
        &cfg.tools.baidu.api_key
    );
    push!(
        SecretField::BraveApiKey,
        "LLAIA_BRAVE_API_KEY",
        &cfg.tools.brave.api_key
    );
    push!(
        SecretField::WebuiToken,
        "LLAIA_WEBUI_TOKEN",
        &cfg.webui.token
    );
    out
}

/// 把 entries 中对应的敏感字段替换为 `${VAR}` 引用（原地修改）。
pub fn apply_refs(cfg: &mut Config, entries: &[SecretEntry]) {
    for e in entries {
        let var_ref = format!("${{{}}}", e.var);
        match e.field {
            SecretField::ProviderApiKey => {
                if let Some(id) = &e.provider_id {
                    if let Some(p) = cfg.provider.get_mut(id) {
                        p.api_key = var_ref;
                    }
                }
            }
            SecretField::QqAppSecret => cfg.channels.qq.app_secret = var_ref,
            SecretField::TelegramBotToken => cfg.channels.telegram.bot_token = var_ref,
            SecretField::DingtalkClientSecret => cfg.channels.dingtalk.client_secret = var_ref,
            SecretField::MailImapPass => cfg.channels.mail.imap_pass = var_ref,
            SecretField::MailSmtpPass => cfg.channels.mail.smtp_pass = var_ref,
            SecretField::FeishuAppSecret => cfg.channels.feishu.app_secret = var_ref,
            SecretField::TavilyApiKey => cfg.tools.tavily.api_key = var_ref,
            SecretField::BaiduApiKey => cfg.tools.baidu.api_key = var_ref,
            SecretField::BraveApiKey => cfg.tools.brave.api_key = var_ref,
            SecretField::WebuiToken => cfg.webui.token = var_ref,
        }
    }
}

/// 幂等写入 .env：保留注释/空行与无关行，更新已有 key，追加新 key。
/// 值为 KEY=VALUE 原样写入（dotenvy 启动时负责解析展开）。
pub fn upsert_env(path: &Path, updates: &[(String, String)]) -> Result<()> {
    let mut lines: Vec<String> = Vec::new();
    if let Ok(content) = std::fs::read_to_string(path) {
        lines = content.lines().map(|l| l.to_string()).collect();
    }
    let mut updated: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in updates {
        updated.insert(k.clone(), v.clone());
    }
    let mut written = BTreeMap::new();
    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(eq) = line.find('=') {
            let key = line[..eq].trim().to_string();
            if let Some(v) = updated.get(&key) {
                *line = format!("{}={}", key, v);
                written.insert(key, ());
            }
        }
    }
    for (k, v) in &updated {
        if !written.contains_key(k) {
            lines.push(format!("{}={}", k, v));
        }
    }
    let parent = path.parent();
    if let Some(p) = parent {
        std::fs::create_dir_all(p).ok();
    }
    std::fs::write(path, format!("{}\n", lines.join("\n")))
        .with_context(|| format!("write .env {:?}", path))?;
    // Unix 下收紧权限（Windows 无 POSIX 权限位）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 把 config 中 `${VAR}` 引用的敏感字段展开为环境变量值（内存态使用）。
///
/// 写盘保留 `${VAR}` 引用（下次启动 `Config::load` → `expand_paths` 再展开）；
/// 仅热加载 / 内存 config 需要展开后的明文（`build_provider_from_config` 直接用
/// `api_key` 字符串，不认 `${VAR}`）。非引用字符串原样返回（`expand_string` 行为）。
pub fn expand_config_secrets(cfg: &mut Config) {
    let expand = |s: &str| crate::config::expand_string(s).unwrap_or_else(|_| s.to_string());
    for p in cfg.provider.values_mut() {
        p.api_key = expand(&p.api_key);
    }
    cfg.channels.qq.app_secret = expand(&cfg.channels.qq.app_secret);
    cfg.channels.telegram.bot_token = expand(&cfg.channels.telegram.bot_token);
    cfg.channels.dingtalk.client_secret = expand(&cfg.channels.dingtalk.client_secret);
    cfg.channels.mail.imap_pass = expand(&cfg.channels.mail.imap_pass);
    cfg.channels.mail.smtp_pass = expand(&cfg.channels.mail.smtp_pass);
    cfg.channels.feishu.app_secret = expand(&cfg.channels.feishu.app_secret);
    cfg.tools.tavily.api_key = expand(&cfg.tools.tavily.api_key);
    cfg.tools.baidu.api_key = expand(&cfg.tools.baidu.api_key);
    cfg.tools.brave.api_key = expand(&cfg.tools.brave.api_key);
    cfg.webui.token = expand(&cfg.webui.token);
}

/// 扫描 config.toml 明文敏感字段数量（读原始 TOML，不做 ${VAR} 展开）。
/// 供启动提示用——内存 config 已被 `expand_paths` 展开为明文，不能用它判断。
pub fn count_plaintext_secrets(config_path: &Path) -> Result<usize> {
    let disk = std::fs::read_to_string(config_path)?;
    let parsed: Config = toml::from_str(&disk)?;
    Ok(collect_plaintext_secrets(&parsed).len())
}

/// 在 toml_edit 文档中按路径下钻并设置为标量值；路径不存在返回 false。
fn set_nested(doc: &mut toml_edit::DocumentMut, path: &[&str], value: &str) -> bool {
    let mut cur = doc.as_table_mut().get_mut(path[0]);
    for k in &path[1..] {
        cur = cur
            .and_then(|i| i.as_table_mut())
            .and_then(|t| t.get_mut(k));
    }
    match cur {
        Some(item) => {
            *item = toml_edit::value(value);
            true
        }
        None => false,
    }
}

/// 扫描 config.toml 明文敏感字段 → 转存 .env → config.toml 改为 `${VAR}` 引用。
/// 用 toml_edit 定点替换（保留注释）。返回迁移条目数（0 = 无明文，无需迁移）。
///
/// 供 `/migrate-secrets` 斜杠命令与启动扫描提示使用。
pub fn migrate_config_secrets(config_path: &Path) -> Result<usize> {
    let disk = std::fs::read_to_string(config_path)?;
    let parsed: Config = toml::from_str(&disk)?;
    let secrets = collect_plaintext_secrets(&parsed);
    if secrets.is_empty() {
        return Ok(0);
    }
    let env_path = config_path
        .parent()
        .map(|p| p.join(".env"))
        .ok_or_else(|| anyhow!("config path has no parent dir"))?;
    let updates: Vec<(String, String)> = secrets
        .iter()
        .map(|e| (e.var.clone(), e.value.clone()))
        .collect();
    upsert_env(&env_path, &updates)?;

    let mut doc: toml_edit::DocumentMut = disk
        .parse()
        .with_context(|| format!("parse config.toml {:?}", config_path))?;
    for e in &secrets {
        let var_ref = format!("${{{}}}", e.var);
        let ok = match e.field {
            SecretField::ProviderApiKey => match &e.provider_id {
                Some(id) => set_nested(&mut doc, &["provider", id, "api_key"], &var_ref),
                None => false,
            },
            SecretField::QqAppSecret => {
                set_nested(&mut doc, &["channels", "qq", "app_secret"], &var_ref)
            }
            SecretField::TelegramBotToken => {
                set_nested(&mut doc, &["channels", "telegram", "bot_token"], &var_ref)
            }
            SecretField::DingtalkClientSecret => set_nested(
                &mut doc,
                &["channels", "dingtalk", "client_secret"],
                &var_ref,
            ),
            SecretField::MailImapPass => {
                set_nested(&mut doc, &["channels", "mail", "imap_pass"], &var_ref)
            }
            SecretField::MailSmtpPass => {
                set_nested(&mut doc, &["channels", "mail", "smtp_pass"], &var_ref)
            }
            SecretField::FeishuAppSecret => {
                set_nested(&mut doc, &["channels", "feishu", "app_secret"], &var_ref)
            }
            SecretField::TavilyApiKey => {
                set_nested(&mut doc, &["tools", "tavily", "api_key"], &var_ref)
            }
            SecretField::BaiduApiKey => {
                set_nested(&mut doc, &["tools", "baidu", "api_key"], &var_ref)
            }
            SecretField::BraveApiKey => {
                set_nested(&mut doc, &["tools", "brave", "api_key"], &var_ref)
            }
            SecretField::WebuiToken => set_nested(&mut doc, &["webui", "token"], &var_ref),
        };
        if !ok {
            tracing::warn!(var = %e.var, "config key not found during migrate; skipped");
        }
    }
    std::fs::write(config_path, doc.to_string())?;
    Ok(secrets.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tempfile::tempdir;

    fn base_config() -> Config {
        // 用默认骨架（provider.api_key 默认空 → 不参与收集）
        Config::default_for_workspace("~/.llaia")
    }

    #[test]
    fn is_plaintext_detects_refs() {
        assert!(!is_plaintext(""));
        assert!(!is_plaintext("${LLAIA_TEST_KEY}"));
        assert!(is_plaintext("sk-plain-123"));
        assert!(is_plaintext("${lowercase}"));
        assert!(is_plaintext("${MIXED_case}"));
    }

    #[test]
    fn provider_var_sanitizes_id() {
        assert_eq!(provider_var("default"), "LLAIA_PROVIDER_DEFAULT_API_KEY");
        assert_eq!(
            provider_var("my-provider"),
            "LLAIA_PROVIDER_MY_PROVIDER_API_KEY"
        );
        assert_eq!(provider_var("ollama2"), "LLAIA_PROVIDER_OLLAMA2_API_KEY");
    }

    #[test]
    fn collect_skips_refs_and_empty() {
        let mut cfg = base_config();
        cfg.provider.insert(
            "default".into(),
            crate::config::ProviderConfig {
                provider_type: "openai_compatible".into(),
                base_url: "http://localhost:11434/v1".into(),
                api_key: "${LLAIA_EXISTING_KEY}".into(),
                compat: None,
                model: Default::default(),
            },
        );
        cfg.channels.qq.app_secret = "".into();
        cfg.webui.token = "".into();
        let secrets = collect_plaintext_secrets(&cfg);
        assert!(secrets.is_empty());
    }

    #[test]
    fn collect_and_apply_roundtrip() {
        let mut cfg = base_config();
        cfg.provider.insert(
            "default".into(),
            crate::config::ProviderConfig {
                provider_type: "openai_compatible".into(),
                base_url: "http://localhost:11434/v1".into(),
                api_key: "sk-plain".into(),
                compat: None,
                model: Default::default(),
            },
        );
        cfg.channels.qq.app_secret = "qq-secret".into();
        cfg.channels.telegram.bot_token = "tg-token".into();
        cfg.channels.feishu.app_secret = "fs-secret".into();
        cfg.tools.tavily.api_key = "tvly-x".into();
        cfg.webui.token = "ui-token".into();

        let secrets = collect_plaintext_secrets(&cfg);
        assert_eq!(secrets.len(), 6);
        // provider var 名正确
        assert!(secrets
            .iter()
            .any(|e| e.var == "LLAIA_PROVIDER_DEFAULT_API_KEY"));
        assert!(secrets.iter().any(|e| e.var == "LLAIA_QQ_APP_SECRET"));

        apply_refs(&mut cfg, &secrets);
        assert_eq!(
            cfg.provider["default"].api_key,
            "${LLAIA_PROVIDER_DEFAULT_API_KEY}"
        );
        assert_eq!(cfg.channels.qq.app_secret, "${LLAIA_QQ_APP_SECRET}");
        assert_eq!(
            cfg.channels.telegram.bot_token,
            "${LLAIA_TELEGRAM_BOT_TOKEN}"
        );
        assert_eq!(cfg.channels.feishu.app_secret, "${LLAIA_FEISHU_APP_SECRET}");
        assert_eq!(cfg.tools.tavily.api_key, "${LLAIA_TAVILY_API_KEY}");
        assert_eq!(cfg.webui.token, "${LLAIA_WEBUI_TOKEN}");
        // 再收集 → 全为引用 → 空
        assert!(collect_plaintext_secrets(&cfg).is_empty());
    }

    #[test]
    fn upsert_env_preserves_comments_and_updates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "# comment\nKEEP=1\nOLD=2\n").unwrap();

        upsert_env(
            &path,
            &[("OLD".into(), "2-new".into()), ("NEW".into(), "3".into())],
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# comment"));
        assert!(content.contains("KEEP=1"));
        assert!(content.contains("OLD=2-new"));
        assert!(content.contains("NEW=3"));
        // 幂等：再跑一次不产生重复行
        upsert_env(&path, &[("NEW".into(), "3".into())]).unwrap();
        let content2 = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content2.matches("NEW=3").count(), 1);
        assert_eq!(content, content2);
    }

    #[test]
    fn upsert_env_creates_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join(".env");
        upsert_env(&path, &[("A".into(), "1".into())]).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("A=1"));
    }

    #[test]
    fn mail_fields_collected() {
        let mut cfg = base_config();
        cfg.channels.mail = crate::config::MailConfig {
            imap_pass: "imap-pass".into(),
            smtp_pass: "smtp-pass".into(),
            imap_user: "u@example.com".into(),
            smtp_user: "u@example.com".into(),
            ..Default::default()
        };
        let secrets = collect_plaintext_secrets(&cfg);
        assert!(secrets
            .iter()
            .any(|e| e.var == "LLAIA_MAIL_IMAP_PASS" && e.value == "imap-pass"));
        assert!(secrets
            .iter()
            .any(|e| e.var == "LLAIA_MAIL_SMTP_PASS" && e.value == "smtp-pass"));
        // imap_user / smtp_user 非密钥 → 不收集
        assert!(!secrets.iter().any(|e| e.value == "u@example.com"));
    }

    #[test]
    fn migrate_moves_secrets_and_preserves_comments() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"# keep this comment
[runtime]
context_threshold = 0.7

[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"
api_key = "sk-plain-123"

[webui]
token = "ui-token-456"
"#,
        )
        .unwrap();

        let n = migrate_config_secrets(&config_path).unwrap();
        assert_eq!(n, 2);

        let config_text = std::fs::read_to_string(&config_path).unwrap();
        assert!(config_text.contains("# keep this comment"));
        assert!(config_text.contains("context_threshold = 0.7"));
        assert!(config_text.contains("api_key = \"${LLAIA_PROVIDER_DEFAULT_API_KEY}\""));
        assert!(config_text.contains("token = \"${LLAIA_WEBUI_TOKEN}\""));
        assert!(!config_text.contains("sk-plain-123"));

        // .env 落盘
        let env_path = dir.path().join(".env");
        let env_text = std::fs::read_to_string(&env_path).unwrap();
        assert!(env_text.contains("LLAIA_PROVIDER_DEFAULT_API_KEY=sk-plain-123"));
        assert!(env_text.contains("LLAIA_WEBUI_TOKEN=ui-token-456"));

        // 再跑 → 0（全为引用）
        assert_eq!(migrate_config_secrets(&config_path).unwrap(), 0);
    }

    #[test]
    fn count_plaintext_reads_raw_toml() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"
api_key = "sk-plain"

[webui]
token = "${LLAIA_REF}"
"#,
        )
        .unwrap();
        assert_eq!(count_plaintext_secrets(&config_path).unwrap(), 1);
    }
}
