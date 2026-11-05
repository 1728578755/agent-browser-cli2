use super::*;

/// CDP 规则的体积上限。
///
/// `body` / `response.body` 会被整份落进 `net-hooks.json`，再随每条规则广播给**每个**
/// 标签；一条 20MB 的 mock body 会让 daemon 落盘 + N 次 WS 消息同时爆掉。
/// 返回 `(单条字节上限, 单 profile 规则条数上限)`，可用环境变量调整。
fn net_rule_limits() -> (usize, usize) {
    (
        crate::settings::env_usize_clamped(
            "ATB_NET_RULE_MAX_BYTES",
            2 * 1024 * 1024,
            1024,
            16 * 1024 * 1024,
        ),
        crate::settings::env_usize_clamped("ATB_NET_RULE_MAX_RULES", 64, 1, 500),
    )
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// 校验一条拦截规则的可下发性：body 类型、字节数。
///
/// 页面引擎与 CDP 引擎共用同一份规则形状，校验放在 `net_value` 解析完 rule 之后
/// 统一做一次，避免只有 `--engine cdp` 受保护。
pub(crate) fn validate_net_rule(action: &str, rule: &Value) -> Result<()> {
    let (max_bytes, _) = net_rule_limits();
    for key in ["body", "requestBody"] {
        let Some(value) = rule.get(key).filter(|value| !value.is_null()) else {
            continue;
        };
        if !(value.is_string() || value.is_object() || value.is_array()) {
            return Err(anyhow!(
                "net {action}: rule.{key} 必须是字符串或 JSON 对象/数组（收到 {}）",
                json_kind(value)
            ));
        }
        let bytes = value
            .as_str()
            .map(str::len)
            .unwrap_or_else(|| value.to_string().len());
        if bytes > max_bytes {
            return Err(anyhow!(
                "net_rule_too_large: rule.{key} 有 {bytes} 字节，超过上限 {max_bytes} 字节\
                 （可用 ATB_NET_RULE_MAX_BYTES 调整，值域 1KiB..16MiB）；\
                 建议改为短 stub，或让页面自己请求大文件（clamped: 未下发，规则未写入）"
            ));
        }
    }
    if let Some(response_body) = rule
        .get("response")
        .and_then(|response| response.get("body"))
        .filter(|value| !value.is_null())
    {
        if !(response_body.is_string() || response_body.is_object() || response_body.is_array()) {
            return Err(anyhow!(
                "net {action}: rule.response.body 必须是字符串或 JSON 对象/数组（收到 {}）",
                json_kind(response_body)
            ));
        }
        let bytes = response_body
            .as_str()
            .map(str::len)
            .unwrap_or_else(|| response_body.to_string().len());
        if bytes > max_bytes {
            return Err(anyhow!(
                "net_rule_too_large: rule.response.body 有 {bytes} 字节，超过上限 {max_bytes} 字节\
                 （可用 ATB_NET_RULE_MAX_BYTES 调整）；clamped: 未下发，规则未写入"
            ));
        }
    }
    Ok(())
}

/// 网络层增删改：只改 profile 上的规则，然后逐标签下发到扩展。
///
/// 返回体里的 `applied` 已经说明下发明细；旧签名多带一个恒为 `true` 的 bool，
/// 调用方全部丢弃，属于会误导人的死信号，已收成单个 `Value`。
pub(crate) async fn net_cdp_action(
    state: &AppState,
    profile_key: &str,
    action: &str,
    opts: &Value,
    config: &mut Value,
    timeout: Duration,
) -> Result<Value> {
    let mut rules = config
        .get("cdpRules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let enabled = config.get("cdpEnabled").and_then(Value::as_bool) == Some(true);
    // 扩展重启过：Fetch 域已经没了，只发 `rules` 不会重新 enable，
    // 必须用 `start` 整体重建。`start` 会清掉扩展侧日志/统计，所以只在真重启后用。
    let rebuild = config.get("cdpRebuildRequired").and_then(Value::as_bool) == Some(true);
    let apply_verb = if enabled && !rebuild {
        "rules"
    } else {
        "start"
    };
    match action {
        "route" | "mock" => {
            let rule = opts
                .get("rule")
                .cloned()
                .filter(Value::is_object)
                .ok_or_else(|| anyhow!("net {action}: 需要 rule"))?;
            let pattern = rule
                .get("match")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if pattern.is_empty() {
                return Err(anyhow!("net {action}: rule.match 不能为空"));
            }
            validate_net_rule(action, &rule)?;
            let method = rule
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("*")
                .to_uppercase();
            // 原位替换：旧写法 `retain` + `push` 会把更新过的规则挪到队尾，
            // 改变 first-match 优先级；页面侧 `page_net.js` 已改成原位替换，这里必须一致。
            let existing = rules.iter().position(|item| {
                let same_match =
                    item.get("match").and_then(Value::as_str) == Some(pattern.as_str());
                let same_method = item
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("*")
                    .to_uppercase()
                    == method;
                same_match && same_method
            });
            // 条数上限只对「新增」生效：更新已有规则不该被上限挡住。
            if existing.is_none() {
                let (_, max_rules) = net_rule_limits();
                if rules.len() >= max_rules {
                    return Err(anyhow!(
                        "net_rule_too_large: 当前 profile 已有 {} 条 CDP 规则，达到上限 {max_rules}\
                         （可用 ATB_NET_RULE_MAX_RULES 调整）；请先 `net unroute` 掉不用的规则",
                        rules.len()
                    ));
                }
            }
            match existing {
                Some(index) => rules[index] = rule.clone(),
                None => rules.push(rule.clone()),
            }
            let apply =
                net_cdp_broadcast(state, profile_key, apply_verb, &json!(rules), timeout).await?;
            config["cdpRules"] = json!(rules);
            config["cdpEnabled"] = json!(true);
            // 只有所有标签都确认才清重建标记，否则下一次 route 继续用 `start` 补齐。
            if apply
                .get("failed")
                .and_then(Value::as_array)
                .map(|failed| failed.is_empty())
                .unwrap_or(true)
            {
                config["cdpRebuildRequired"] = json!(false);
            }
            Ok(json!({
                "ok": true, "action": "route", "engine": "cdp",
                "routes": rules.len(), "rule": rule, "all": rules, "applied": apply
            }))
        }
        "unroute" => {
            let needle = opts
                .get("match")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if needle.is_empty() {
                return Err(anyhow!("net unroute: 需要 match"));
            }
            let before = rules.len();
            rules.retain(|item| item.get("match").and_then(Value::as_str) != Some(needle.as_str()));
            let removed = before - rules.len();
            // 删光规则后必须 `stop` 关掉 Fetch 域：只发一个空 `rules` 会让
            // Fetch 仍处于 enabled 状态，浏览器侧继续暂停/转发每个请求，
            // 而 daemon 侧 `cdpEnabled` 还是 true —— 用户再没有任何命令能把它关掉。
            let cleared = rules.is_empty();
            let verb = if cleared { "stop" } else { apply_verb };
            let payload_rules = if cleared { json!([]) } else { json!(rules) };
            // 重建待办时即使 cdpEnabled 为 false 也要下发：否则重启后的首个
            // unroute 只改配置，浏览器侧的旧拦截还在。
            let apply = if cleared || enabled || rebuild {
                net_cdp_broadcast(state, profile_key, verb, &payload_rules, timeout).await?
            } else {
                Value::Null
            };
            config["cdpRules"] = json!(rules);
            let fully_applied = apply.get("ok").and_then(Value::as_bool) == Some(true)
                && apply
                    .get("failed")
                    .and_then(Value::as_array)
                    .map(|failed| failed.is_empty())
                    .unwrap_or(true);
            let mut warning: Option<String> = None;
            if cleared {
                if fully_applied {
                    config["cdpEnabled"] = json!(false);
                    config["cdpRebuildRequired"] = json!(false);
                } else if apply.is_null() {
                    // 没有可下发的标签（配置里本来就没启用）：同步复位即可。
                    config["cdpEnabled"] = json!(false);
                } else {
                    // 部分标签没停下来：保留 cdpEnabled=true，让下一次 unroute / reset
                    // 还能再命中它们（否则配置已复位，这些标签的拦截再也停不掉）。
                    warning = Some(
                        "部分标签未能停止 CDP 拦截，已保留 cdpEnabled 以便重试（见 applied.failed）"
                            .to_string(),
                    );
                }
            } else if enabled || rebuild {
                config["cdpEnabled"] = json!(true);
                if rebuild && fully_applied {
                    config["cdpRebuildRequired"] = json!(false);
                }
            }
            let mut payload = json!({
                "ok": true, "action": "unroute", "engine": "cdp",
                "removed": removed, "routes": rules.len(), "all": rules, "applied": apply,
                // cleared=true 表示规则已删光且已下发 stop；cdp_enabled 是写回后的真实状态。
                "cleared": cleared,
                "cdp_enabled": config.get("cdpEnabled").cloned().unwrap_or(json!(false)),
            });
            if let Some(warning) = warning {
                payload["warning"] = json!(warning);
            }
            Ok(payload)
        }
        _ => Err(anyhow!("net: engine=cdp 不支持 action {action}")),
    }
}

/// 该 profile 下所有活跃标签的会话 key。
pub(crate) async fn profile_sessions(state: &AppState, profile_key: &str) -> Vec<String> {
    let driver = state.driver.read().await;
    driver
        .sessions
        .values()
        .filter(|session| session.is_active())
        .filter(|session| format!("{}:{}", session.browser_id, session.profile_id) == profile_key)
        .map(|session| session.session_key.clone())
        .collect()
}

/// 把 `debugPass` 开关同步到扩展侧的 per-tab 状态，让 CDP 的 pass 日志真正开始记录。
///
/// 走 `rules` 而不是 `start`：`start` 会清空已有日志与统计。`enabled` 是**每次都要
/// 下发**的最终值：旧实现只在打开时下发 `debugPass: true`，于是
/// `net log`（不带 `--debug-pass`）永远关不掉上一次打开的开关，pass 日志只增不减，
/// 也无从判断当前到底是开还是关。
pub(crate) async fn net_cdp_set_debug_pass(
    state: &AppState,
    profile_key: &str,
    config: &Value,
    timeout: Duration,
    enabled: bool,
) -> Value {
    let rules = config.get("cdpRules").cloned().unwrap_or_else(|| json!([]));
    let sessions = profile_sessions(state, profile_key).await;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut set = tokio::task::JoinSet::new();
    for session_key in sessions {
        let task_state = state.clone();
        let rules = rules.clone();
        set.spawn(async move {
            let tab_id = tab_id_of_session(&session_key);
            let outcome = extension_control(
                &task_state,
                &session_key,
                "netIntercept",
                json!({ "action": "rules", "rules": rules, "debugPass": enabled }),
                timeout,
            )
            .await;
            (tab_id, outcome)
        });
    }
    let mut applied = 0usize;
    let mut failed: Vec<Value> = Vec::new();
    loop {
        let joined = match tokio::time::timeout_at(deadline, set.join_next()).await {
            Ok(Some(joined)) => joined,
            Ok(None) | Err(_) => break,
        };
        match joined {
            Ok((_tab_id, Ok(_))) => applied += 1,
            Ok((tab_id, Err(err))) => failed.push(json!({
                "tab_id": tab_id,
                "error": humanize_extension_error(&err.to_string()),
            })),
            Err(err) => eprintln!("net: debugPass 同步任务异常退出: {err}"),
        }
    }
    json!({ "ok": failed.is_empty(), "enabled": enabled, "applied": applied, "failed": failed })
}

/// 把规则下发到该 profile 下的每个标签（Fetch 域是 per-tab 的）。
///
/// 并发 + 统一 deadline：旧实现串行等待每个标签，N 个标签各带一个
/// `timeout`，总耗时可达 N×timeout；调用方此时正持有 `net_persist_lock`，
/// 后续所有 net 命令都会被一起堵住（首请求慢 → 之后 150s 全线超时）。
pub(crate) async fn net_cdp_broadcast(
    state: &AppState,
    profile_key: &str,
    action: &str,
    rules: &Value,
    timeout: Duration,
) -> Result<Value> {
    let sessions: Vec<String> = profile_sessions(state, profile_key).await;
    if sessions.is_empty() {
        return Err(anyhow!("该 profile 没有可用的标签页"));
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut set = tokio::task::JoinSet::new();
    for session_key in sessions.iter().cloned() {
        let task_state = state.clone();
        let action = action.to_string();
        let rules = rules.clone();
        set.spawn(async move {
            let tab_id = tab_id_of_session(&session_key);
            let outcome = extension_control(
                &task_state,
                &session_key,
                "netIntercept",
                json!({ "action": action, "rules": rules }),
                timeout,
            )
            .await;
            (session_key, tab_id, outcome)
        });
    }
    let mut applied = 0usize;
    let mut failures = Vec::new();
    // 扩展侧把「无法收窄 urlPattern、只能退化成 `*`（全量拦截）」的 match 放在
    // `fallbackMatches`（status / start / rules 三种回包都有）。合并多标签后去重，
    // 让 routes / route --engine cdp 的输出能看出是哪条规则把范围放大成了全量。
    let mut fallback_matches: Vec<String> = Vec::new();
    let mut pending: Vec<String> = sessions.clone();
    loop {
        let joined = match tokio::time::timeout_at(deadline, set.join_next()).await {
            Ok(Some(joined)) => joined,
            Ok(None) | Err(_) => break,
        };
        match joined {
            Ok((session_key, _tab_id, Ok(value))) => {
                pending.retain(|key| key != &session_key);
                applied += 1;
                if let Some(items) = value.get("fallbackMatches").and_then(Value::as_array) {
                    for item in items {
                        let Some(text) = item.as_str() else { continue };
                        if !fallback_matches.iter().any(|seen| seen == text) {
                            fallback_matches.push(text.to_string());
                        }
                    }
                }
            }
            Ok((session_key, tab_id, Err(err))) => {
                pending.retain(|key| key != &session_key);
                failures.push(json!({
                    "tab_id": tab_id,
                    "error": humanize_extension_error(&err.to_string())
                }));
            }
            Err(err) => eprintln!("net: 网络层规则下发任务异常退出: {err}"),
        }
    }
    // 预算用完后仍在跑（或被 abort）的标签：状态未知，按超时如实上报，
    // 别让「没回来」看起来像「已经生效」。
    for session_key in pending {
        failures.push(json!({
            "tab_id": tab_id_of_session(&session_key),
            "error": format!("timeout: {}s 内未收到该标签的确认", timeout.as_secs()),
        }));
    }
    if applied == 0 {
        let first = failures
            .first()
            .and_then(|item| item.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(anyhow!("网络层拦截失败（CDP debugger 被占用？）：{first}"));
    }
    Ok(json!({
        "ok": true,
        "applied": applied,
        "failed": failures,
        "fallback_matches": fallback_matches,
    }))
}
