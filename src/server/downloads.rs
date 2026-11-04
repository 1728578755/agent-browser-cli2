use super::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 每个调用方（agent）最近一次 `download` 启动的下载 id。
///
/// `download-wait` 不带 `--id` 时，旧逻辑只能靠「wait 启动前 10s 内开始的终态条目」
/// 推断归属：多 Agent 并发下载时，可能认领别人的文件。改为在 `download`（start）
/// 成功时按 agent 记下 id，wait 直接取自己那条；记录 10 分钟内有效，避免长期误导。
fn last_download_registry() -> &'static Mutex<HashMap<String, (i64, Instant)>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, (i64, Instant)>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remember_last_download(id: i64) {
    let mut registry = last_download_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry.insert(current_agent(), (id, Instant::now()));
}

fn last_download_of_current_agent() -> Option<i64> {
    let mut registry = last_download_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let agent = current_agent();
    match registry.get(&agent).copied() {
        Some((id, at)) if at.elapsed() < Duration::from_secs(600) => Some(id),
        Some(_) => {
            registry.remove(&agent);
            None
        }
        None => None,
    }
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct DownloadsRequest {
    /// list | start | wait | cancel | erase
    pub(crate) action: Option<String>,
    pub(crate) id: Option<i64>,
    pub(crate) url: Option<String>,
    pub(crate) filename: Option<String>,
    pub(crate) dir: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) limit: Option<usize>,
    /// wait 的超时（秒）与轮询间隔（秒）。
    pub(crate) wait_timeout: Option<f64>,
    pub(crate) interval: Option<f64>,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
}

pub(crate) async fn downloads(
    State(state): State<AppState>,
    Json(req): Json<DownloadsRequest>,
) -> Json<Value> {
    touch(&state).await;
    // wait 的超时 / interrupted 是业务失败，外层 ok 必须跟随内层。
    Json(json_result_with_inner_ok(
        downloads_value(&state, req).await,
    ))
}

pub(crate) async fn downloads_value(state: &AppState, req: DownloadsRequest) -> Result<Value> {
    let action = req
        .action
        .clone()
        .unwrap_or_else(|| "list".to_string())
        .to_lowercase();
    let session_key = select_tab(
        state,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
    )
    .await?;

    if action != "wait" {
        let mut payload = json!({ "action": action });
        if let Some(id) = req.id {
            payload["id"] = json!(id);
        }
        if let Some(url) = req.url {
            payload["url"] = json!(url);
        }
        if let Some(filename) = req.filename {
            payload["filename"] = json!(filename);
        }
        if let Some(dir) = req.dir {
            payload["dir"] = json!(dir);
        }
        if let Some(state_filter) = req.state {
            payload["state"] = json!(state_filter);
        }
        if let Some(limit) = req.limit {
            payload["limit"] = json!(limit.clamp(1, 200));
        }
        let value = extension_control(
            state,
            &session_key,
            "downloads",
            payload,
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await?;
        let data = value.get("data").cloned().unwrap_or(value);
        // 记住本次 start 的 id，供同一 agent 的 `download-wait` 精确认领。
        if action == "start" {
            let id = data
                .get("item")
                .and_then(|item| item.get("id"))
                .or_else(|| data.get("id"))
                .and_then(Value::as_i64);
            if let Some(id) = id {
                remember_last_download(id);
            }
        }
        return Ok(data);
    }

    // wait：在 daemon 侧轮询，避免扩展 Service Worker 在长下载期间被回收。
    let timeout = req.wait_timeout.unwrap_or(120.0).clamp(1.0, 3600.0);
    let interval = req.interval.unwrap_or(0.5).clamp(0.05, 5.0);
    let deadline = Instant::now() + secs(timeout);
    let mut polls: u64 = 0;
    let mut last: Value = Value::Null;
    // 不带 `--id` 时，「等最近一条」需要一个时间基线。
    //
    // 旧实现直接取 `items.first()`：只要列表里还有一条**历史的** complete 记录，
    // 第一次轮询就命中并返回 `ok:true`——等待类命令的假成功比超时更难发现，
    // 调用方会拿着一份旧文件当成本次下载的产物。
    // 现在的规则：进循环时记下列表里的最大 id，之后只认「id 比基线大」的记录；
    // 唯一例外是**最新一条仍在进行中**（`download` 紧接着 `download-wait` 的常见路径）。
    //
    // 上面这条规则修掉了假成功，却把主流路径变成了假超时：
    // `download && download-wait` 里目标条目在 wait 首轮就已存在（id == 基线），
    // 一旦完成就被 `id > 基线` 排除，兜底过滤器又把 complete/interrupted 排除，
    // 于是一路空转到 120s 超时。补两条通道：
    // 1) **认领**：看到「最新一条仍在进行中」就记下它的 id，之后按 id 跟踪到终态，
    //    不再依赖「它是否还比基线新」；
    // 2) **近期完成窗口**：首轮没有在跑条目时，接受「最新一条已终态且刚开始不久」
    //    的记录（10s 容差），覆盖 `download` 很快结束、wait 才启动的场景。
    // 原有 `id > baseline` 通道保留，三种情形互不覆盖。
    // 优先用同一 agent 刚才 `download` 启动的 id（真正的归属证明）；
    // 没有记录时才退回「认领/近期窗口」的推断逻辑（兼容旧调用方与手工 wait）。
    let effective_id = req.id.or_else(last_download_of_current_agent);
    let mut baseline_max_id: Option<i64> = None;
    let mut claimed_id: Option<i64> = None;
    let wait_started_ms = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0);
    loop {
        polls += 1;
        // 每轮控制调用与 sleep 都必须吃「剩余预算」，否则声明 1s 的等待
        // 可能因为一次扩展往返就拖到 30s 以上（deadline 只在调用后检查）。
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(json!({
                "ok": false,
                "matched": false,
                "state": "timeout",
                "error": format!("download_wait_timeout: {timeout}s 内未完成"),
                "item": last,
                "polls": polls
            }));
        }
        let call_timeout = remaining
            .min(DEFAULT_EXTENSION_CONTROL_TIMEOUT)
            .max(Duration::from_millis(50));
        let value = match extension_control(
            state,
            &session_key,
            "downloads",
            json!({ "action": "list", "limit": 50, "id": effective_id }),
            call_timeout,
        )
        .await
        {
            Ok(value) => value,
            // 单轮超时不算整体失败：预算还没用完就继续等下一轮；扩展真断了由
            // 后续轮次连续失败 + deadline 兜底，避免把一次抖动误报成下载失败。
            Err(err) if remaining > Duration::from_millis(100) => {
                last = json!({ "error": humanize_extension_error(&err.to_string()) });
                tokio::time::sleep(remaining.min(secs(interval))).await;
                continue;
            }
            Err(err) => return Err(err),
        };
        let data = value.get("data").cloned().unwrap_or(value);
        let items = data
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let max_id = items
            .iter()
            .filter_map(|item| item.get("id").and_then(Value::as_i64))
            .max();
        if baseline_max_id.is_none() {
            baseline_max_id = max_id;
        }
        let latest = items
            .iter()
            .max_by_key(|item| item.get("id").and_then(Value::as_i64).unwrap_or(0));
        // 认领：最新一条还在跑就锁定它。只在当前轮还没认领时做一次，
        // 避免中途新开的下载把等待目标换掉。
        if effective_id.is_none() && claimed_id.is_none() {
            if let Some(item) = latest {
                if download_is_running(download_state_of(item)) {
                    claimed_id = item.get("id").and_then(Value::as_i64);
                }
            }
        }
        let mut matched_recently = false;
        let item = if let Some(id) = effective_id {
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_i64) == Some(id))
                .cloned()
        } else if let Some(claimed) = claimed_id {
            // 认领后只按 id 跟踪：列表被 limit 截断或扩展重启时这一轮可能读不到，
            // 继续等下一轮即可（不要退回去重新认领，否则目标会被换掉）。
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_i64) == Some(claimed))
                .cloned()
        } else {
            let newer = items
                .iter()
                .filter(
                    |item| match (item.get("id").and_then(Value::as_i64), baseline_max_id) {
                        (Some(id), Some(base)) => id > base,
                        _ => false,
                    },
                )
                .max_by_key(|item| item.get("id").and_then(Value::as_i64).unwrap_or(0));
            if let Some(item) = newer {
                // 原有通道：wait 期间新出现的记录（id 比基线大）。
                Some(item.clone())
            } else if let Some(item) = latest
                .filter(|item| download_is_running(download_state_of(item)))
                .cloned()
            {
                // 没有更新的记录：最新一条还在跑就认它。
                Some(item)
            } else {
                // 近期完成窗口：wait 启动前 10s 内开始的终态条目算「刚完成」，
                // 起步时间读不到（扩展没带 startTimeMs / startTime）时跳过该判据，
                // 宁可继续等到超时，也不拿历史记录冒充本次产物。
                let recent = latest.cloned().filter(|item| {
                    download_is_terminal(download_state_of(item))
                        && download_start_ms(item)
                            .map(|start| start >= wait_started_ms.saturating_sub(10_000))
                            .unwrap_or(false)
                });
                matched_recently = recent.is_some();
                recent
            }
        };
        if let Some(item) = item {
            let download_state = download_state_of(&item).to_string();
            if download_is_terminal(&download_state) {
                let mut out = json!({
                    "ok": download_state == "complete",
                    "matched": download_state == "complete",
                    "state": download_state,
                    "item": item,
                    "polls": polls,
                    "waited_ms": (polls.saturating_sub(1) as f64 * interval * 1000.0).round() as u64
                });
                // 只有走「近期完成窗口」才有这个字段：调用方据此知道命中的是
                // wait 之前就已完成的那条，而不是实时观察到的完成。
                if matched_recently {
                    out["matched_recently"] = json!(true);
                }
                return Ok(out);
            }
            last = item;
        }
        if Instant::now() >= deadline {
            return Ok(json!({
                "ok": false,
                "matched": false,
                "state": "timeout",
                "error": format!("download_wait_timeout: {timeout}s 内未完成"),
                "item": last,
                "polls": polls
            }));
        }
        // sleep 同样不能越过 deadline：`interval` 默认 0.5s，但用户可以给 5s。
        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(secs(interval).min(remaining)).await;
    }
}

// ---------------------------------------------------------------- wait 判据辅助

fn download_state_of(item: &Value) -> &str {
    item.get("state")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// `complete` / `interrupted` 是下载的终态，其余（in_progress / paused 等）都还在跑。
fn download_is_terminal(state: &str) -> bool {
    matches!(state, "complete" | "interrupted")
}

/// 「仍在进行中」：state 缺失的空串不算——没读到状态就认领，只会空等到超时。
fn download_is_running(state: &str) -> bool {
    !state.is_empty() && !download_is_terminal(state)
}

/// 条目开始时间（epoch 毫秒）。
///
/// 优先读毫秒字段 `startTimeMs`（扩展可能要补），缺失时退回 ISO8601 字符串：
/// 扩展的 `summarizeDownload` 目前给的是 `start_time: item.startTime`。
/// 全都读不到时返回 `None`——调用方会把「近期完成」这条判据跳过，
/// 绝不用 0 或当前时间兜底（那等于把所有历史记录都当成刚完成）。
fn download_start_ms(item: &Value) -> Option<i64> {
    for key in ["startTimeMs", "startTime", "start_time"] {
        match item.get(key) {
            Some(Value::Number(number)) => {
                if let Some(value) = number.as_i64() {
                    // 秒级时间戳也能用（1e10 ms 约等于 2286 年，足够区分）。
                    return Some(if value < 10_000_000_000 {
                        value.saturating_mul(1000)
                    } else {
                        value
                    });
                }
            }
            Some(Value::String(text)) => {
                if let Some(value) = parse_iso8601_ms(text) {
                    return Some(value);
                }
            }
            _ => {}
        }
    }
    None
}

/// 解析 `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]`，返回 epoch 毫秒。
///
/// 只覆盖浏览器时间戳的实际形态（Chrome 的 `downloads.startTime` 是 UTC ISO8601），
/// 不引时间库：解析失败返回 `None`，由调用方跳过判据。
fn parse_iso8601_ms(text: &str) -> Option<i64> {
    let text = text.trim();
    let part =
        |range: std::ops::Range<usize>| -> Option<i64> { text.get(range)?.parse::<i64>().ok() };
    if text.len() < 19 {
        return None;
    }
    let year = part(0..4)?;
    let month = part(5..7)?;
    let day = part(8..10)?;
    let hour = part(11..13)?;
    let minute = part(14..16)?;
    let second = part(17..19)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut rest = text.get(19..)?;
    let mut millis: i64 = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits: String = fraction
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        if digits.is_empty() {
            return None;
        }
        let mut frac = digits.clone();
        frac.truncate(3);
        while frac.len() < 3 {
            frac.push('0');
        }
        millis = frac.parse::<i64>().ok()?;
        rest = fraction.get(digits.len()..)?;
    }
    let offset_seconds = match rest {
        "" | "Z" | "z" => 0,
        other => {
            let sign = match other.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let body = other.get(1..)?;
            let (offset_hour, offset_minute) = match body.split_once(':') {
                Some((hh, mm)) => (hh.parse::<i64>().ok()?, mm.parse::<i64>().ok()?),
                None if body.len() == 4 => (
                    body.get(0..2)?.parse::<i64>().ok()?,
                    body.get(2..4)?.parse::<i64>().ok()?,
                ),
                None => return None,
            };
            sign * (offset_hour * 3600 + offset_minute * 60)
        }
    };
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_seconds;
    Some(seconds.saturating_mul(1000) + millis)
}

/// 公历日期 → 1970-01-01 起的天数（Howard Hinnant 的 days_from_civil）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

// ---------------------------------------------------------------- 多浏览器编排
