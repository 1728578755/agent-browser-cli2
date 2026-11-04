use super::*;

/// 合并两份日志：页面 hook 记的（fetch/XHR）与扩展 CDP 侧记的（网络层拦截）。
///
/// 两边字段不完全一致，这里只做「打 source 标签 + 按 at 排序 + 统一过滤/截断」，
/// 条目本身的字段原样保留，方便排查。
// 参数数量确实偏多，但每一个都是独立的过滤/合并维度（会话、配置、两份日志、
// 条数、是否清空、关键字），打包成结构体只会把"这里传什么"藏到结构体定义里，
// 反而更难读。保留签名，显式豁免。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn merge_net_log(
    state: &AppState,
    session_key: &str,
    config: &Value,
    page_payload: Value,
    limit: usize,
    clear: Option<bool>,
    needle: Option<&str>,
    log_source: &str,
    timeout: Duration,
) -> Value {
    let mut items: Vec<Value> = Vec::new();
    let mut page_count = 0usize;
    let mut cdp_count = 0usize;
    // CDP 侧取日志失败时不能静默：`sources.cdp=0` 与「确实没有日志」无法区分。
    let mut cdp_error: Option<String> = None;
    let include_page = log_source != "cdp";
    let include_cdp = log_source != "page";
    if include_page {
        if let Some(list) = page_payload.get("items").and_then(Value::as_array) {
            for item in list {
                let mut copy = item.clone();
                if let Some(object) = copy.as_object_mut() {
                    object.insert("source".to_string(), json!("page"));
                }
                items.push(copy);
                page_count += 1;
            }
        }
    }
    let cdp_enabled =
        include_cdp && config.get("cdpEnabled").and_then(Value::as_bool) == Some(true);
    // 扩展回报的 debugPass 开关：带出来才能确认 `--debug-pass` 真的生效，
    // 否则用户只能看到「没有 pass 条目」，分不清是没放行过还是开关没打开。
    let mut cdp_debug_pass: Option<Value> = None;
    if cdp_enabled {
        match extension_control(
            state,
            session_key,
            "netIntercept",
            json!({ "action": "status" }),
            timeout,
        )
        .await
        {
            Ok(status) => {
                if let Some(flag) = status.get("debugPass") {
                    cdp_debug_pass = Some(flag.clone());
                }
                if let Some(list) = status.get("log").and_then(Value::as_array) {
                    for item in list {
                        let mut copy = item.clone();
                        if let Some(object) = copy.as_object_mut() {
                            object.insert("source".to_string(), json!("cdp"));
                        }
                        items.push(copy);
                        cdp_count += 1;
                    }
                }
            }
            Err(err) => cdp_error = Some(humanize_extension_error(&err.to_string())),
        }
    }
    if let Some(needle) = needle
        .map(|value| value.to_lowercase())
        .filter(|value| !value.is_empty())
    {
        items.retain(|item| {
            item.get("url")
                .and_then(Value::as_str)
                .map(|url| url.to_lowercase().contains(&needle))
                .unwrap_or(false)
        });
    }
    // 两边的 at 都是 Date.now() 毫秒，可直接排序。
    items.sort_by_key(|item| {
        item.get("at")
            .or_else(|| item.get("time"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    });
    let total = items.len();
    let skip = total.saturating_sub(limit);
    let tail: Vec<Value> = items.into_iter().skip(skip).collect();
    // clearlog 失败不能静默：调用方以为日志清了，下一次 `net log` 又看到
    // 同一批旧条目，只会怀疑是不是没清干净。与 `cdp_error` 同形报出来，但不改 `ok`。
    let mut cdp_clear_error: Option<String> = None;
    if clear == Some(true) && cdp_enabled {
        if let Err(err) = extension_control(
            state,
            session_key,
            "netIntercept",
            json!({ "action": "clearlog" }),
            timeout,
        )
        .await
        {
            cdp_clear_error = Some(humanize_extension_error(&err.to_string()));
        }
    }
    let mut body = json!({
        "ok": true,
        "action": "log",
        "installed": page_payload.get("installed").cloned().unwrap_or(json!(false)),
        "count": total,
        "sources": { "page": page_count, "cdp": cdp_count },
        "items": tail,
    });
    if let Some(err) = cdp_error {
        body["sources"]["cdp_error"] = json!(err);
    }
    if let Some(err) = cdp_clear_error {
        body["sources"]["cdp_clear_error"] = json!(err);
    }
    // 只在 CDP 侧参与查询时出现；页面侧没有这个概念。
    if let Some(flag) = cdp_debug_pass {
        body["debug_pass"] = flag;
    }
    body
}
