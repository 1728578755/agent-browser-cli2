use super::*;

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
            // 重建待办时即使 cdpEnabled 为 false 也要下发：否则重启后的首个
            // unroute 只改配置，浏览器侧的旧拦截还在。
            let apply = if enabled || rebuild {
                net_cdp_broadcast(state, profile_key, apply_verb, &json!(rules), timeout).await?
            } else {
                Value::Null
            };
            config["cdpRules"] = json!(rules);
            if rebuild
                && apply
                    .get("failed")
                    .and_then(Value::as_array)
                    .map(|failed| failed.is_empty())
                    .unwrap_or(true)
            {
                config["cdpRebuildRequired"] = json!(false);
            }
            Ok(json!({
                "ok": true, "action": "unroute", "engine": "cdp",
                "removed": removed, "routes": rules.len(), "all": rules, "applied": apply
            }))
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
/// 走 `rules` 而不是 `start`：`start` 会清空已有日志与统计。失败不致命，
/// 只是这次 `net log --debug-pass` 拿不到 pass 条目。
pub(crate) async fn net_cdp_set_debug_pass(
    state: &AppState,
    profile_key: &str,
    config: &Value,
    timeout: Duration,
) {
    let rules = config.get("cdpRules").cloned().unwrap_or_else(|| json!([]));
    for session_key in profile_sessions(state, profile_key).await {
        let _ = extension_control(
            state,
            &session_key,
            "netIntercept",
            json!({ "action": "rules", "rules": rules, "debugPass": true }),
            timeout,
        )
        .await;
    }
}

/// 把规则下发到该 profile 下的每个标签（Fetch 域是 per-tab 的）。
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
    let mut applied = 0usize;
    let mut failures = Vec::new();
    // 扩展侧把「无法收窄 urlPattern、只能退化成 `*`（全量拦截）」的 match 放在
    // `fallbackMatches`（status / start / rules 三种回包都有）。合并多标签后去重，
    // 让 routes / route --engine cdp 的输出能看出是哪条规则把范围放大成了全量。
    let mut fallback_matches: Vec<String> = Vec::new();
    for session_key in &sessions {
        match extension_control(
            state,
            session_key,
            "netIntercept",
            json!({ "action": action, "rules": rules }),
            timeout,
        )
        .await
        {
            Ok(value) => {
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
            Err(err) => failures.push(json!({
                "tab_id": tab_id_of_session(session_key),
                "error": humanize_extension_error(&err.to_string())
            })),
        }
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
