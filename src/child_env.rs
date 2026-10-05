//! 子进程环境变量密钥剔除（ADR-0033 L3/A1，P9 Phase 2 首刀，学 codex）。
//!
//! dotenvy 把 `.env` 灌进进程 env，terminal/MCP 子进程默认继承完整环境——
//! 被骗模型一条 `env` 即可倒出全部 API key。剔除规则按变量名形态：含
//! `KEY` / `SECRET` / `TOKEN`（大小写不敏感）即视为凭据。配置显式提供给
//! 子进程的变量（mcp.toml 的 server `env` 段）在白名单内不剔除。

use std::collections::HashSet;

const SENSITIVE_SUBSTRINGS: [&str; 3] = ["KEY", "SECRET", "TOKEN"];

pub fn is_sensitive_key(name: &str) -> bool {
    let upper = name.to_uppercase();
    SENSITIVE_SUBSTRINGS.iter().any(|s| upper.contains(s))
}

/// 纯函数核心：从候选变量名中挑出需要移除的（命中敏感形态且不在 keep 白名单）。
fn filter_sensitive<'a>(names: impl Iterator<Item = &'a str>, keep: &HashSet<&str>) -> Vec<String> {
    names
        .filter(|n| is_sensitive_key(n) && !keep.contains(n))
        .map(|n| n.to_string())
        .collect()
}

/// 对子进程 Command 应用剔除。`extra_keys` 是配置显式提供给该子进程的变量名
/// （如 mcp.toml server env 段的键），即使命中敏感形态也保留。
///
/// 无视调用顺序安全：`Command::env_remove` 与 `envs` 同键后写覆盖先写，
/// 而 extra_keys 已从移除清单里排除，两边操作键集不相交。
pub fn scrub_command_env(cmd: &mut tokio::process::Command, extra_keys: &[String]) {
    let keep: HashSet<&str> = extra_keys.iter().map(|s| s.as_str()).collect();
    let inherited: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .collect();
    for name in filter_sensitive(inherited.iter().map(|s| s.as_str()), &keep) {
        cmd.env_remove(&name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_sensitive_key_matches_codex_rule() {
        assert!(is_sensitive_key("OPENAI_API_KEY"));
        assert!(is_sensitive_key("api_key"));
        assert!(is_sensitive_key("AWS_SECRET_ACCESS_KEY"));
        assert!(is_sensitive_key("TG_BOT_TOKEN"));
        assert!(is_sensitive_key("APPSECRET"));
        // 大小写不敏感
        assert!(is_sensitive_key("My-Api-Token"));
        // 常规系统变量不受影响
        assert!(!is_sensitive_key("PATH"));
        assert!(!is_sensitive_key("HOME"));
        assert!(!is_sensitive_key("SYSTEMROOT"));
        assert!(!is_sensitive_key("TERM"));
        assert!(!is_sensitive_key("SSH_AUTH_SOCK"));
    }

    #[test]
    fn test_filter_sensitive_keeps_whitelist() {
        let keep: HashSet<&str> = ["LLAMA_API_KEY", "PATH"].into_iter().collect();
        let names = [
            "OPENAI_API_KEY",
            "LLAMA_API_KEY", // 配置显式提供 → 保留
            "PATH",
            "TG_BOT_TOKEN",
        ];
        let removed = filter_sensitive(names.into_iter(), &keep);
        assert_eq!(removed, vec!["OPENAI_API_KEY", "TG_BOT_TOKEN"]);
    }
}
