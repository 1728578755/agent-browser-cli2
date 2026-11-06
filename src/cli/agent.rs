use super::*;
#[allow(unused_imports)]
use clap::{Args, Parser, Subcommand};

/// 解析调用方身份。
///
/// 优先级：`--agent` → 环境变量 → 终端会话标识 → `default`。
/// 终端标识这一层是为了「同一台机器开两个终端各跑一个智能体」时自动隔离——
/// 不配置也能分开；配置了就以显式值为准。
pub(crate) fn resolve_agent_id(explicit: Option<&str>) -> String {
    if let Some(value) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return sanitize_agent(value);
    }
    if let Some(value) = crate::settings::agent_from_env() {
        return sanitize_agent(&value);
    }
    for key in [
        "WT_SESSION",
        "TERM_SESSION_ID",
        "VSCODE_IPC_HOOK_CLI",
        "TMUX_PANE",
        "SSH_TTY",
    ] {
        if let Ok(value) = env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return sanitize_agent(&format!("{key}:{trimmed}"));
            }
        }
    }
    "default".to_string()
}

pub(crate) fn sanitize_agent(value: &str) -> String {
    // 与 daemon 的 `is_valid_agent_id` 同一个上限：1..=80 个 `[A-Za-z0-9._:-]`。
    // 追加 `-<8 位哈希>` 需要预留 9 个字符，否则超长身份会被 daemon 直接忽略并回落
    // `default`（比塌缩更隐蔽）。
    const MAX_LEN: usize = 80;
    const SUFFIX_LEN: usize = 9; // `-` + 8 位十六进制
    let cleaned: String = value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
        .take(MAX_LEN - SUFFIX_LEN)
        .collect();
    if cleaned.is_empty() {
        // 全是非 ASCII（`ATB_AGENT=客服` / `运维`）：以前直接塌缩成 `default`，两个智能体
        // 共用默认标签页并互相 cleanup 掉对方的页面。用稳定短哈希保留区分度。
        return format!("agent-{}", short_hash(value));
    }
    if cleaned == value {
        return cleaned;
    }
    // 清洗丢掉了区分度：`客服A`/`运维A` 都会变成 `A`，`a/b` 与 `ab` 都会变成 `ab`——
    // 两个智能体于是共用同一个标签页命名空间，互相 cleanup 掉对方的页面。
    // 在保留 ASCII 安全字符集不变的前提下，追加原值的 8 位短哈希：`A-1a2b3c4d`。
    format!("{cleaned}-{}", short_hash(value))
}

/// 原值的 8 位短哈希（`DefaultHasher`，只需在本机/同一次构建内稳定）。
///
/// DefaultHasher 不保证跨 Rust 版本稳定，但 CLI 与 daemon 永远来自同一次构建，
/// 而且这个 id 只在进程内存与本地 json 里用，不参与任何持久标识。
fn short_hash(value: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

pub(crate) fn agent_id() -> &'static str {
    AGENT_ID.get().map(String::as_str).unwrap_or("default")
}
