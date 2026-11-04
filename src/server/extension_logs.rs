use super::*;

/// 扩展控制命令的默认超时。
///
/// 只有「调用方没有自己的超时语义」的简单控制命令（cookies / frames /
/// network 启停）才用它兜底；pageOp 等长命令一律传调用方的 timeout，
/// 否则 `snapshot --timeout 300` 落到 CSP 回退通道上会被硬编码的 30s 截断。
pub(crate) const DEFAULT_EXTENSION_CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) async fn extension_control(
    state: &AppState,
    session_key: &str,
    command: &str,
    payload: Value,
    timeout: Duration,
) -> Result<Value> {
    let mut body = serde_json::Map::new();
    body.insert("cmd".to_string(), json!(command));
    let tab_id = session_tab_id(state, session_key).await?;
    body.insert("tabId".to_string(), json!(parse_tab_id(&tab_id)?));
    if let Value::Object(map) = payload {
        for (key, value) in map {
            body.insert(key, value);
        }
    }
    let response = execute_raw_js_on_tab(
        state,
        &Value::Object(body).to_string(),
        session_key,
        timeout,
    )
    .await
    .map_err(|err| anyhow!(humanize_extension_error(&err.to_string())))?;
    let value = response.data.or(response.result).unwrap_or(Value::Null);
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        let raw = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("extension command failed");
        return Err(anyhow!(humanize_extension_error(raw)));
    }
    Ok(value)
}

/// 扩展有时把错误再包一层 JSON（`{"error":"..."}` 或 `{"message":"..."}`），
/// 直接拼到命令错误里会出现双重转义的引号，这里剥到最内层文本。
pub(crate) fn humanize_extension_error(raw: &str) -> String {
    let mut text = raw.trim().to_string();
    for _ in 0..3 {
        if !(text.starts_with('{') && text.ends_with('}')) {
            break;
        }
        let parsed = match serde_json::from_str::<Value>(&text) {
            Ok(value) => value,
            Err(_) => break,
        };
        let inner = parsed
            .get("error")
            .and_then(Value::as_str)
            .or_else(|| parsed.get("message").and_then(Value::as_str));
        match inner {
            Some(next) if next.trim() != text => text = next.trim().to_string(),
            _ => break,
        }
    }
    text
}

pub(crate) async fn network_start(
    State(state): State<AppState>,
    Json(req): Json<TabRequest>,
) -> Json<Value> {
    touch(&state).await;
    let result = async {
        let tab_id = select_tab(
            &state,
            SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        )
        .await?;
        extension_control(
            &state,
            &tab_id,
            "networkStart",
            json!({}),
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await
    }
    .await;
    Json(match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

pub(crate) async fn network_list(
    State(state): State<AppState>,
    Json(req): Json<NetworkListRequest>,
) -> Json<Value> {
    touch(&state).await;
    let result = async {
        let tab_id = select_tab(
            &state,
            SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        )
        .await?;
        extension_control(
            &state,
            &tab_id,
            "networkList",
            json!({ "filter": req.filter, "limit": req.limit }),
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await
    }
    .await;
    Json(match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

pub(crate) async fn network_detail(
    State(state): State<AppState>,
    Json(req): Json<NetworkDetailRequest>,
) -> Json<Value> {
    touch(&state).await;
    let result = async {
        let tab_id = select_tab(
            &state,
            SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        )
        .await?;
        extension_control(
            &state,
            &tab_id,
            "networkDetail",
            json!({ "requestId": req.request_id }),
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await
    }
    .await;
    Json(match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

pub(crate) async fn network_clear(
    State(state): State<AppState>,
    Json(req): Json<TabRequest>,
) -> Json<Value> {
    touch(&state).await;
    let result = async {
        let tab_id = select_tab(
            &state,
            SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        )
        .await?;
        extension_control(
            &state,
            &tab_id,
            "networkClear",
            json!({}),
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await
    }
    .await;
    Json(match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

pub(crate) async fn network_stop(
    State(state): State<AppState>,
    Json(req): Json<TabRequest>,
) -> Json<Value> {
    touch(&state).await;
    let result = async {
        let tab_id = select_tab(
            &state,
            SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        )
        .await?;
        extension_control(
            &state,
            &tab_id,
            "networkStop",
            json!({}),
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await
    }
    .await;
    Json(match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

pub(crate) async fn console_start(
    State(state): State<AppState>,
    Json(req): Json<TabRequest>,
) -> Json<Value> {
    touch(&state).await;
    let result = async {
        let tab_id = select_tab(
            &state,
            SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        )
        .await?;
        extension_control(
            &state,
            &tab_id,
            "consoleStart",
            json!({}),
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await
    }
    .await;
    Json(match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

pub(crate) async fn console_list(
    State(state): State<AppState>,
    Json(req): Json<ConsoleListRequest>,
) -> Json<Value> {
    touch(&state).await;
    let result = async {
        let tab_id = select_tab(
            &state,
            SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        )
        .await?;
        extension_control(
            &state,
            &tab_id,
            "consoleList",
            json!({ "level": req.level, "limit": req.limit }),
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await
    }
    .await;
    Json(match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

pub(crate) async fn console_clear(
    State(state): State<AppState>,
    Json(req): Json<TabRequest>,
) -> Json<Value> {
    touch(&state).await;
    let result = async {
        let tab_id = select_tab(
            &state,
            SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        )
        .await?;
        extension_control(
            &state,
            &tab_id,
            "consoleClear",
            json!({}),
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await
    }
    .await;
    Json(match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

pub(crate) async fn console_stop(
    State(state): State<AppState>,
    Json(req): Json<TabRequest>,
) -> Json<Value> {
    touch(&state).await;
    let result = async {
        let tab_id = select_tab(
            &state,
            SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        )
        .await?;
        extension_control(
            &state,
            &tab_id,
            "consoleStop",
            json!({}),
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await
    }
    .await;
    Json(match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}
