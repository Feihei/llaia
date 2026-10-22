//! 统一的出站 HTTP client 构建入口与进程级代理模式（`[runtime].proxy`）。
//!
//! 背景：reqwest 默认继承系统代理（Windows 注册表 / 环境变量），系统代理开启时
//! 发往 127.0.0.1 本地端点（Ollama / LMStudio / sd-server）的请求也会被路由进
//! 代理而失败。本模块收口全部 `reqwest::Client` 的构建：默认 **强制直连**
//! （no_proxy），需要出墙的云端 API 可显式选 `system` 或填代理 URL。
//!
//! 模式值（`[runtime].proxy`，`Config::load` 校验）：
//! - 缺省 / `"none"`：强制直连（默认）
//! - `"system"`：跟随系统代理（reqwest 默认行为）
//! - 其他值：视为代理 URL（`http://`、`socks5://` 等，由 reqwest 解析）
//!
//! 全局模式在进程启动时由各命令入口（serve / chat / doctor）从配置注入，
//! reload loop 每轮重建子系统前重设，重建出的 client 自然跟随新值。

use std::sync::RwLock;

/// 出站代理模式，见模块文档。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyMode {
    /// 强制直连（默认）。
    None,
    /// 跟随系统代理（reqwest 默认行为）。
    System,
    /// 显式代理 URL。
    Url(String),
}

impl ProxyMode {
    /// 从 `[runtime].proxy` 配置值解析；`None`/`"none"`/`""` → 直连，
    /// `"system"` → 跟随系统，其他 → 代理 URL。
    pub fn from_config(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            None | Some("") | Some("none") => ProxyMode::None,
            Some("system") => ProxyMode::System,
            Some(url) => ProxyMode::Url(url.to_string()),
        }
    }
}

static PROXY_MODE: RwLock<ProxyMode> = RwLock::new(ProxyMode::None);

fn proxy_mode() -> ProxyMode {
    // 中毒锁仅在持锁线程 panic 时发生；这里持锁只做 clone，不会 panic，
    // 退回上次值比传播 poison 更合理。
    PROXY_MODE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// 注入进程级代理模式（serve / chat / doctor 入口在加载配置后调用；
/// reload loop 每轮重设，重建的 client 随新值）。
pub fn set_proxy(mode: ProxyMode) {
    // 同上：持锁只做赋值，不会 panic。
    *PROXY_MODE.write().unwrap_or_else(|e| e.into_inner()) = mode;
}

/// 统一 client 构建入口：调用方在此之上链自己的 timeout / UA / redirect 等。
/// 代理失败（URL 非法）防御性回退直连——`Config::load` 已校验，正常不可达。
pub fn client_builder() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder();
    match proxy_mode() {
        ProxyMode::None => builder.no_proxy(),
        ProxyMode::System => builder,
        ProxyMode::Url(url) => match reqwest::Proxy::all(&url) {
            Ok(proxy) => builder.proxy(proxy),
            Err(_) => builder.no_proxy(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_config_parses_three_modes() {
        assert_eq!(ProxyMode::from_config(None), ProxyMode::None);
        assert_eq!(ProxyMode::from_config(Some("")), ProxyMode::None);
        assert_eq!(ProxyMode::from_config(Some("  none ")), ProxyMode::None);
        assert_eq!(ProxyMode::from_config(Some("system")), ProxyMode::System);
        assert_eq!(
            ProxyMode::from_config(Some("http://127.0.0.1:7890")),
            ProxyMode::Url("http://127.0.0.1:7890".into())
        );
    }

    #[test]
    fn client_builder_applies_no_proxy_by_default() {
        set_proxy(ProxyMode::None);
        // 不 panic 且可构建即达成直连语义；reqwest 无 API 读取已设代理，
        // 行为由集成路径（系统代理环境下测试套件全绿）背书。
        assert!(client_builder().build().is_ok());
    }
}
