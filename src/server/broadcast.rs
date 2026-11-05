use super::*;

#[derive(Debug, Deserialize)]
pub(crate) struct BroadcastRequest {
    pub(crate) op: String,
    #[serde(default)]
    pub(crate) args: Value,
    #[serde(default)]
    pub(crate) browsers: Vec<String>,
    #[serde(default)]
    pub(crate) profiles: Vec<String>,
    pub(crate) concurrency: Option<usize>,
}

pub(crate) async fn broadcast(
    State(state): State<AppState>,
    Json(req): Json<BroadcastRequest>,
) -> Json<Value> {
    touch(&state).await;
    // 广播的结论在 `failed == 0`（内层 ok）上，外层恒 true 会让「全部目标都失败」
    // 也退出 0（v5 C-14 复发点）。
    Json(json_result_with_inner_ok(
        broadcast_value(&state, req).await,
    ))
}

pub(crate) async fn broadcast_value(state: &AppState, req: BroadcastRequest) -> Result<Value> {
    if !JOB_OPS.contains(&req.op.as_str()) {
        return Err(anyhow!(
            "broadcast: 不支持 op {}，可选：{}",
            req.op,
            JOB_OPS.join(", ")
        ));
    }
    if !req.args.is_null() && !req.args.is_object() {
        // args 非对象时无法注入 switch_tab_id，每条子命令都会打到全局默认标签，必须显式报错。
        let kind = if req.args.is_array() {
            "array"
        } else if req.args.is_string() {
            "string"
        } else if req.args.is_number() {
            "number"
        } else if req.args.is_boolean() {
            "boolean"
        } else {
            "unknown"
        };
        return Err(anyhow!("broadcast: --args 必须是 JSON 对象，收到 {}", kind));
    }
    let mut targets = browser_targets(state).await;
    if !req.browsers.is_empty() {
        let wanted: Vec<String> = req.browsers.iter().map(|v| v.to_lowercase()).collect();
        // 只做单向匹配：`abc*` 是前缀，其余一律精确匹配。
        // 以前是双向前缀（id.starts_with(want) || want.starts_with(id)），
        // 短 id 会同时命中多个浏览器，把广播悄悄打到错误的目标上。
        targets.retain(|target| {
            let id = target.browser_id.to_lowercase();
            wanted.iter().any(|want| match want.strip_suffix('*') {
                Some(prefix) => id.starts_with(prefix),
                None => id == *want,
            })
        });
        if targets.is_empty() {
            return Err(anyhow!(
                "未找到浏览器 {}；可用 \"{}*\" 做前缀匹配，或用 atb browsers 查看完整 id",
                req.browsers.join(", "),
                req.browsers[0]
            ));
        }
    }
    if !req.profiles.is_empty() {
        let wanted: Vec<String> = req.profiles.iter().map(|v| v.to_lowercase()).collect();
        targets.retain(|target| {
            let mine = [
                target.profile_id.to_lowercase(),
                target
                    .profile_label
                    .clone()
                    .unwrap_or_default()
                    .to_lowercase(),
            ];
            wanted
                .iter()
                .any(|want| mine.iter().any(|value| value == want))
        });
    }
    if targets.is_empty() {
        return Err(anyhow!(
            "no_target: 没有匹配的 browser/profile（可用 browsers 命令查看）"
        ));
    }

    let started = Instant::now();
    let concurrency = req.concurrency.unwrap_or(targets.len()).clamp(1, 32);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut tasks = tokio::task::JoinSet::new();
    // 调用方身份必须在 spawn 之前取出来带进任务。
    //
    // task-local **不会**随 `tokio::spawn` 继承：裸 spawn 的任务里 `current_agent()`
    // 退回 "default"，广播出去的每条子命令都会落到全局默认标签上，并把别人的默认
    // 目标覆盖成自己的——表面成功，实际打错了页面。
    let agent = current_agent();
    for target in targets {
        let state = state.clone();
        let op = req.op.clone();
        let base_args = req.args.clone();
        let semaphore = semaphore.clone();
        let agent = agent.clone();
        tasks.spawn(with_agent(agent, async move {
            let _permit = semaphore.acquire().await;
            let mut args = if base_args.is_null() {
                json!({})
            } else {
                base_args
            };
            if let Some(object) = args.as_object_mut() {
                object.insert(
                    "switch_tab_id".to_string(),
                    json!(target.session_key.clone()),
                );
                object.remove("browser");
                object.remove("profile");
            }
            let step_started = Instant::now();
            let result = run_job(&state, &op, args, Arc::new(AtomicBool::new(false))).await;
            (target, result, step_started.elapsed().as_millis() as u64)
        }));
    }

    let mut results = Vec::new();
    let mut failed = 0usize;
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((target, result, elapsed_ms)) => {
                let (ok, payload) = match result {
                    // 请求成功 ≠ 业务成功：`wait` 超时这类会以 Ok(ok:false) 回来，
                    // 以前一律计成功，于是「广播了但没人等到」也报 ok。
                    Ok(value) => {
                        let inner_ok = value.get("ok").and_then(Value::as_bool).unwrap_or(true);
                        if !inner_ok {
                            failed += 1;
                        }
                        (inner_ok, json!({ "result": value }))
                    }
                    Err(err) => {
                        failed += 1;
                        (false, json!({ "error": err.to_string() }))
                    }
                };
                let mut entry = json!({
                    "browser_id": target.browser_id,
                    "profile_id": target.profile_id,
                    "profile_label": target.profile_label,
                    "tab_id": target.tab_id,
                    "session_key": target.session_key,
                    "ok": ok,
                    "elapsed_ms": elapsed_ms,
                });
                if let (Some(entry), Some(payload)) = (entry.as_object_mut(), payload.as_object()) {
                    for (key, value) in payload {
                        entry.insert(key.clone(), value.clone());
                    }
                }
                results.push(entry);
            }
            Err(err) => {
                failed += 1;
                results.push(json!({ "ok": false, "error": format!("task join error: {err}") }));
            }
        }
    }
    results.sort_by(|a, b| {
        let key = |value: &Value| {
            (
                value
                    .get("browser_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                value
                    .get("profile_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        };
        key(a).cmp(&key(b))
    });
    Ok(json!({
        "op": req.op,
        "count": results.len(),
        "failed": failed,
        "ok": failed == 0,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "results": results,
    }))
}

// ---------------------------------------------------------------- 页面网络
