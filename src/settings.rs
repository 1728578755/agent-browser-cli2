//! 用户可调参数（统一入口）。
//!
//! 设计原则：
//! - **默认值保持向后兼容**：不设置任何环境变量时，行为与本轮修复前一致或更安全；
//! - **用户可覆盖**：所有阈值、档位、开关都能用 `ATB_*` 环境变量显式调参；
//! - **越界即夹紧**：非法值/超出值域的值回落到缺省，并夹到合法区间，绝不 panic。
//!
//! 值域与语义写在每个访问器上方；完整清单同步在 `README` 的「环境变量」章节。

use std::env;

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// 读取字符串型环境变量；空串视为未设置。
pub(crate) fn env_string(name: &str, default: &str) -> String {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => default.to_string(),
    }
}

/// 读取布尔型环境变量；非法值回落缺省。
pub(crate) fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .and_then(|value| parse_bool(&value))
        .unwrap_or(default)
}

/// 读取浮点型环境变量并夹到 `[min, max]`；NaN/非法值回落缺省。
pub(crate) fn env_f64_clamped(name: &str, default: f64, min: f64, max: f64) -> f64 {
    match env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
    {
        Some(value) if value.is_finite() => value.clamp(min, max),
        _ => default.clamp(min, max),
    }
}

/// 读取无符号整数环境变量并夹到 `[min, max]`；非法值回落缺省。
pub(crate) fn env_u64_clamped(name: &str, default: u64, min: u64, max: u64) -> u64 {
    match env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        Some(value) => value.clamp(min, max),
        None => default.clamp(min, max),
    }
}

/// 读取 `usize` 环境变量并夹到 `[min, max]`。
pub(crate) fn env_usize_clamped(name: &str, default: usize, min: usize, max: usize) -> usize {
    env_u64_clamped(name, default as u64, min as u64, max as u64) as usize
}

/// 读取枚举型环境变量；不在白名单内回落缺省。
pub(crate) fn env_enum(name: &str, default: &str, allowed: &[&str]) -> String {
    match env::var(name) {
        Ok(value) => {
            let value = value.trim().to_ascii_lowercase();
            if allowed.iter().any(|candidate| *candidate == value) {
                value
            } else {
                default.to_string()
            }
        }
        Err(_) => default.to_string(),
    }
}

// ---------------------------------------------------------------------------
// 结果语义
// ---------------------------------------------------------------------------

/// `ATB_RESULT_STRICT`：组合命令（`--wait-js`、broadcast、scrape 汇总等）的严格度。
///
/// - `off`：子结论失败不改变顶层 `ok`（旧行为，不推荐）；
/// - `warn`（默认）：顶层 `ok:false` + `error_code`，但保留已产生的结果；
/// - `strict`：与 `warn` 相同，并让 CLI 退出码固定为非零（CI 门禁用）。
pub(crate) fn result_strict() -> String {
    env_enum("ATB_RESULT_STRICT", "warn", &["off", "warn", "strict"])
}

// ---------------------------------------------------------------------------
// 超时预算
// ---------------------------------------------------------------------------

/// `ATB_WAIT_MAX_TOTAL_SEC`：`wait` / `--wait-js` 单条命令的总预算上限（秒）。
/// 值域 `1..=3600`，默认 `600`（与动作类命令上限一致）。
pub(crate) fn wait_max_total_sec() -> f64 {
    env_f64_clamped("ATB_WAIT_MAX_TOTAL_SEC", 600.0, 1.0, 3600.0)
}

/// `ATB_WAIT_MIN_INTERVAL_MS`：`wait --interval` 的最小值（毫秒）。
/// 值域 `1..=60000`，默认 `50`；防止 `interval=0` 忙轮询。
pub(crate) fn wait_min_interval_ms() -> u64 {
    env_u64_clamped("ATB_WAIT_MIN_INTERVAL_MS", 50, 1, 60_000)
}

/// `ATB_CHALLENGE_MAX_IFRAMES`：单次挑战探测最多处理的 iframe 数。
/// 值域 `1..=64`，默认 `8`；超出按可视面积取前 K 个并写进 diag。
pub(crate) fn challenge_max_iframes() -> usize {
    env_usize_clamped("ATB_CHALLENGE_MAX_IFRAMES", 8, 1, 64)
}

/// `ATB_CHALLENGE_CLICK_TIMEOUT_SEC`：坐标点击整条链的硬上限（秒）。
/// 值域 `1..=120`，默认 `8`；与调用方 `--timeout` 取小。
pub(crate) fn challenge_click_timeout_sec() -> f64 {
    env_f64_clamped("ATB_CHALLENGE_CLICK_TIMEOUT_SEC", 8.0, 1.0, 120.0)
}

/// `ATB_HTTP_SLACK_SEC`：CLI 计算 HTTP 超时时在业务超时之上追加的余量（秒）。
/// 值域 `0..=60`，默认 `10`；统一各命令的「daemon 还在跑、CLI 先断」问题。
pub(crate) fn http_slack_sec() -> f64 {
    env_f64_clamped("ATB_HTTP_SLACK_SEC", 10.0, 0.0, 60.0)
}

/// `ATB_DIAG_DEPTH`：诊断信息里保留的底层错误条数。
/// 值域 `0..=20`，默认 `3`；`0` 表示只保留一句归因。
pub(crate) fn diag_depth() -> usize {
    env_usize_clamped("ATB_DIAG_DEPTH", 3, 0, 20)
}

// ---------------------------------------------------------------------------
// 文件写入与隐私
// ---------------------------------------------------------------------------

/// `ATB_IO_OVERWRITE`：`--out` 等文件输出的覆盖策略。
///
/// - `backup`（默认）：覆盖前把旧文件改名为 `<name>.<ts>.bak`；
/// - `force`：直接覆盖（旧行为，脚本化场景）；
/// - `ask`：存在即报 `output_exists`，要求显式 `--overwrite`；
/// - `append`：追加写（仅文本类输出支持）。
pub(crate) fn io_overwrite() -> String {
    env_enum(
        "ATB_IO_OVERWRITE",
        "backup",
        &["backup", "force", "ask", "append"],
    )
}

/// `ATB_IO_SANITIZE_FILENAME`：下载/落盘文件名净化档位。
/// `strict`（默认）取 basename 并拒绝 `..`、分隔符、Windows 保留名；`compat` 只告警。
pub(crate) fn io_sanitize_filename() -> String {
    env_enum("ATB_IO_SANITIZE_FILENAME", "strict", &["strict", "compat"])
}

/// `ATB_IO_MAX_OUTPUT_MB`：单个输出文件/内存缓冲的字节上限（MB）。
/// 值域 `1..=512`，默认 `50`（与 PDF 上限一致）。
pub(crate) fn io_max_output_bytes() -> usize {
    env_usize_clamped("ATB_IO_MAX_OUTPUT_MB", 50, 1, 512) * 1024 * 1024
}

/// `ATB_IDEMPOTENCY_PERSIST`：幂等缓存落盘策略。
///
/// - `redacted`（默认）：只落 `{key, scope, at, status}` 元数据，不落响应体；
/// - `full`：落完整响应（需用户显式知情，供审计/回放）；
/// - `off`：纯内存，不落盘。
pub(crate) fn idempotency_persist() -> String {
    env_enum(
        "ATB_IDEMPOTENCY_PERSIST",
        "redacted",
        &["redacted", "full", "off"],
    )
}

// ---------------------------------------------------------------------------
// 多智能体与会话
// ---------------------------------------------------------------------------

/// `ATB_SESSION_ISOLATION`：具名 agent 无自己的目标时是否允许回退全局默认。
/// `legacy`（默认，兼容旧 skill）/ `strict`（返回 `agent_no_default`）。
pub(crate) fn session_isolation() -> String {
    env_enum("ATB_SESSION_ISOLATION", "legacy", &["legacy", "strict"])
}

/// `ATB_MULTI_SCOPE`：`/multi` 的默认解析范围。
/// `agent`（默认，只解析本 agent 打开的标签）/ `all`（显式跨 agent）。
pub(crate) fn multi_scope_default() -> String {
    env_enum("ATB_MULTI_SCOPE", "agent", &["agent", "all"])
}

/// `ATB_DOWNLOAD_AUTO_CLAIM`：`download-wait` 无 `--id` 时是否自动认领最近下载。
/// 默认 `true`（兼容旧用法）；`false` 要求显式 `--id`。
pub(crate) fn download_auto_claim() -> bool {
    env_bool("ATB_DOWNLOAD_AUTO_CLAIM", true)
}

/// `ATB_DOWNLOAD_CLAIM_TTL_SEC`：自动认领的「近期完成」窗口（秒）。
/// 值域 `30..=900`，默认 `600`。
pub(crate) fn download_claim_ttl_sec() -> u64 {
    env_u64_clamped("ATB_DOWNLOAD_CLAIM_TTL_SEC", 600, 30, 900)
}

// ---------------------------------------------------------------------------
// WS 信任档位
// ---------------------------------------------------------------------------

/// `ATB_WS_TRUST_MODE`：扩展 WS 的握手信任档位。
///
/// - `compat`（默认）：沿用旧行为（Origin 前缀 + 可选 token），并在 `status` 里给出告警；
/// - `whitelist`：要求扩展 ID 在 `ws-trust.json` 白名单内；
/// - `token`：要求握手 token；
/// - `both`：白名单与 token 同时满足（共用机器的推荐档）。
pub(crate) fn ws_trust_mode() -> String {
    env_enum(
        "ATB_WS_TRUST_MODE",
        "compat",
        &["compat", "whitelist", "token", "both"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_falls_back_on_invalid_value() {
        // 不依赖进程环境（测试并行），只验证纯函数分支。
        assert_eq!(parse_bool(" YES "), Some(true));
        assert_eq!(parse_bool("0"), Some(false));
        assert_eq!(parse_bool("maybe"), None);
    }

    #[test]
    fn defaults_are_within_documented_ranges() {
        assert!((1.0..=3600.0).contains(&wait_max_total_sec()));
        assert!((1..=60_000).contains(&wait_min_interval_ms()));
        assert!((1..=64).contains(&challenge_max_iframes()));
        assert!((1..=512).contains(&(io_max_output_bytes() / (1024 * 1024))));
    }
}
