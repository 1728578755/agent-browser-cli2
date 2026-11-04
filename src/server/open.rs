use super::*;

/// `/open` 请求体的扩展包装：`OpenRequest` 是共享 DTO（dto.rs），本轮新增
/// `wait_for` / `warm` 两个字段时不动共享结构，用 serde(flatten) 包一层读取；
/// 旧字段全部原样透传，旧调用方（不传新字段）解析行为与旧版完全一致。
#[derive(Debug, Deserialize)]
pub(crate) struct OpenRequestExt {
    #[serde(flatten)]
    pub(crate) inner: OpenRequest,
    /// 打开后继续等待的页面条件：dom-quiet | selector:<CSS> | text:<文本>。
    /// `networkidle` 仍接受，但实现是**近似**的「resource 计数 + DOM 稳定」采样，
    /// 不是 CDP 在途请求计数。
    #[serde(default)]
    pub(crate) wait_for: Option<String>,
    /// 打开前先 ping 一次扩展，唤醒 service worker。
    #[serde(default)]
    pub(crate) warm: bool,
}

pub(crate) async fn open_tab(
    State(state): State<AppState>,
    Json(req): Json<OpenRequestExt>,
) -> Json<Value> {
    touch(&state).await;
    let wait_for = req.wait_for.clone();
    if req.warm {
        warm_up_extension(&state).await;
    }
    let req = req.inner;
    let extra_targets = req.targets.clone();
    let started = Instant::now();
    if extra_targets.len() > 1 {
        let mut value = open_many(&state, req, wait_for).await;
        // 统一返回：多 URL 的旧字段（tabs/count/failed/elapsed_ms）原样保留，
        // 额外补一份顶层 targets（url/tab_id/ok/error/ready/elapsed_ms）。
        // 旧脚本继续读 tabs 不受影响；新调用方单/多 URL 只认 targets 一套解析。
        if let Some(object) = value.as_object_mut() {
            let targets: Vec<Value> = object
                .get("tabs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|entry| {
                    json!({
                        "url": entry.get("url").cloned().unwrap_or(Value::Null),
                        "tab_id": entry.get("tab_id").cloned().unwrap_or(Value::Null),
                        "ok": entry.get("ok").cloned().unwrap_or(Value::Null),
                        "error": entry.get("error").cloned().unwrap_or(Value::Null),
                        "ready": entry.get("ready").cloned().unwrap_or(Value::Null),
                        // S1-04：两条路径的 targets 形状保持一致（单 URL 那条也带这两个字段）。
                        "refresh_error": entry.get("refresh_error").cloned().unwrap_or(Value::Null),
                        "refresh_error_code": entry
                            .get("refresh_error_code")
                            .cloned()
                            .unwrap_or(Value::Null),
                        "elapsed_ms": entry.get("elapsed_ms").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            object.insert("targets".to_string(), json!(targets));
        }
        return Json(value);
    }
    let mut single = req;
    if let Some(target) = extra_targets.into_iter().next() {
        single.target = Some(target.clone());
        single.url = Some(target);
    }
    let result = open_single(&state, single, true, wait_for.as_deref()).await;
    // 统一返回：单 URL 旧形状 {"ok":true,"result":{...}} 的所有字段不动，
    // 顶层补 count=1 与 targets=[...]。以前单/多 URL 是两套 JSON 形状，调用方
    // 必须先数 targets 长度再选解析器；现在两边的 targets 字段结构一致。
    let entry = result.get("result").cloned().unwrap_or(Value::Null);
    let targets = json!([{
        "url": entry.get("url").cloned().unwrap_or(Value::Null),
        "tab_id": entry.get("opened_tab_id").cloned().unwrap_or(Value::Null),
        "ok": result.get("ok").cloned().unwrap_or(json!(false)),
        // 错误信息按优先级取：wait_for 的（写明是哪个条件）→ result 内的 →
        // 顶层的（open 本身失败时信息只挂在顶层，result 是 null）。
        "error": entry
            .pointer("/wait_for/error")
            .cloned()
            .or_else(|| entry.get("error").cloned())
            .or_else(|| result.get("error").cloned())
            .unwrap_or(Value::Null),
        "ready": entry.get("ready").cloned().unwrap_or(json!(false)),
        // S1-04：标签已开但会话没注册上时，`open_single` 会把刷新失败原因写进 result，
        // 这里透传到统一的 targets 形状（多 URL 走 open_many 的条目，两条路径字段一致）。
        "refresh_error": entry.get("refresh_error").cloned().unwrap_or(Value::Null),
        "refresh_error_code": entry
            .get("refresh_error_code")
            .cloned()
            .unwrap_or(Value::Null),
        "elapsed_ms": started.elapsed().as_millis() as u64,
    }]);
    let mut value = result;
    if let Some(object) = value.as_object_mut() {
        object.insert("count".to_string(), json!(1));
        object.insert("targets".to_string(), targets);
    }
    Json(value)
}

/// --warm：扩展 service worker 空闲约 30 秒就会被 Chrome 挂起，冷启动的
/// 第一条 open 要先等 SW 被消息唤醒，动辄多花几秒甚至撞超时。这里在真正打开
/// 之前先发一次轻量请求（拉 tabs 列表）把它叫醒；失败重试一次，仍失败也照常
/// 打开——warm 只是加速手段，不能因为 ping 失败把 open 拒之门外。
async fn warm_up_extension(state: &AppState) {
    for attempt in 0..2 {
        if refresh_tabs_via_extension(state, None, Duration::from_secs(3))
            .await
            .is_ok()
        {
            return;
        }
        if attempt == 0 {
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
}

/// 单个目标的打开逻辑。
///
/// `refresh_ready`：打开后是否立刻拉一次标签列表把会话注册上。单个打开时必须做，
/// 否则下一条命令找不到新标签；批量打开时交给 `open_many` 收尾统一拉一次，
/// 否则 N 个并发打开会各自排队刷新，把并发收益吃掉。
///
/// `wait_for`：`--wait-for` 条件（dom-quiet / selector: / text:；
/// `networkidle` 作为 dom-quiet 的兼容别名）。
/// 打开并注册会话成功后继续等条件满足；条件未满足时该目标 ok:false，
/// error_code 取真实病因（wait_for_timeout / invalid_wait_for / extension_unavailable），
/// 但标签页本身保留（不回滚、不影响其它目标）。
pub(crate) async fn open_single(
    state: &AppState,
    req: OpenRequest,
    refresh_ready: bool,
    wait_for: Option<&str>,
) -> Value {
    // 先把挑战参数取出来：后面的 `req.group_title.or(req.session)` 会部分移动 req。
    let challenge_mode = req
        .challenge
        .clone()
        .unwrap_or_else(|| "auto".to_string())
        .to_ascii_lowercase();
    let challenge_timeout = req.challenge_timeout.unwrap_or(15.0);
    // 后台标签的定时器会被 Chrome 节流到分钟级，挑战脚本根本走不完。
    // 只在前台打开时主动切到该标签（不抢系统焦点），--background 仍保持后台语义。
    let open_active = req.active;
    // 默认标签被关掉后 `open` 仍必须可用：先清掉本调用方已失效的默认目标，
    // 否则 select_tab 会按 报 target_lost（e2e 的 cleanup → open 序列会卡住）。
    forget_stale_agent_default(state, &current_agent()).await;
    let target = match resolve_request_target(req.target.as_deref(), req.url.as_deref()) {
        Ok(target) => target,
        Err(err) => return open_target_error_json(err),
    };
    let resolved = match resolve_open_target(target, req.cwd.as_deref()) {
        Ok(resolved) => resolved,
        Err(err) => return open_target_error_json(err),
    };
    let mut selector = SessionSelector::new(req.switch_tab_id, req.browser, req.profile);

    if resolved.kind == TargetKind::LocalResource
        && (!req.readiness_timeout.is_finite() || req.readiness_timeout <= 0.0)
    {
        return json!({
            "ok": false,
            "error": "File readiness timeout must be a finite number greater than zero",
            "error_code": "invalid_readiness_timeout",
            "details": { "readiness_timeout": req.readiness_timeout }
        });
    }

    if resolved.kind == TargetKind::LocalResource {
        let status_payload = json!({ "cmd": "status" }).to_string();
        let status_result = match execute_page_js(
            state,
            &status_payload,
            selector.clone(),
            true,
            Duration::from_secs(20),
        )
        .await
        {
            Ok(value) => value,
            Err(err) => {
                // 查询失败 ≠ 扩展太旧。超时/断连报成 `extension_upgrade_required`
                // 会让用户去重装扩展，而真正要做的是刷新标签或等扩展重连（P3 状态机组）。
                let message = err.to_string();
                let transient = [
                    "超时",
                    "timed out",
                    "timeout",
                    "未连接",
                    "not connected",
                    "未找到可操作",
                ]
                .iter()
                .any(|needle| message.to_lowercase().contains(needle));
                return json!({
                    "ok": false,
                    "error": format!("Could not query extension file capability: {err}"),
                    "error_code": if transient { "extension_unavailable" } else { "extension_upgrade_required" },
                    "hint": if transient {
                        "扩展暂时没响应：用 `atb status` 看连接状态，必要时 `atb tabs` 触发一次刷新；持续失败再考虑重装扩展"
                    } else {
                        "确认扩展版本 ≥ 2.1（chrome://extensions 查看），旧版本不支持本地文件"
                    },
                    "details": { "minimum_extension_version": "2.1" }
                });
            }
        };
        if let Some(executor_session_key) = status_result.get("session_key").and_then(Value::as_str)
        {
            selector = SessionSelector::new(Some(executor_session_key.to_string()), None, None);
        }
        let status = status_result
            .get("js_return")
            .cloned()
            .unwrap_or(Value::Null);
        let installed_version = status
            .get("extensionVersion")
            .and_then(Value::as_str)
            .unwrap_or("0");
        if !version_at_least(installed_version, "2.1") {
            return json!({
                "ok": false,
                "error": format!("Agent Browser CLI Bridge {installed_version} does not support local files; version 2.1 or newer is required"),
                "error_code": "extension_upgrade_required",
                "details": {
                    "installed_extension_version": installed_version,
                    "minimum_extension_version": "2.1"
                }
            });
        }
        if status.get("fileSchemeAccess").and_then(Value::as_bool) != Some(true) {
            return json!({
                "ok": false,
                "error": "Agent Browser CLI Bridge is not allowed to access file URLs",
                "error_code": "file_scheme_access_denied",
                "details": {
                    "extension_id": status.get("extensionId").cloned().unwrap_or(Value::Null),
                    "instructions": [
                        "Open chrome://extensions",
                        "Select Agent Browser CLI Bridge",
                        "Enable Allow access to file URLs"
                    ]
                }
            });
        }
    }

    let group_title = req.group_title.or(req.session);
    let payload = json!({
        "cmd": "openTab",
        "url": resolved.navigation_url.clone(),
        "active": req.active,
        "window": req.window,
        "allowFocus": req.allow_focus,
        "keepAlive": req.keep_alive,
        "groupTitle": group_title,
    })
    .to_string();
    let value =
        match execute_page_js(state, &payload, selector, true, Duration::from_secs(30)).await {
            Ok(value) => value,
            Err(err) => return json!({ "ok": false, "error": err.to_string() }),
        };
    let mut result = normalize_open_result(state, value.clone()).await;
    let opened_tab_id = result
        .get("opened_tab_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let expected_session_key = result
        .get("opened_session_key")
        .and_then(Value::as_str)
        .map(str::to_string);

    if resolved.kind == TargetKind::LocalResource {
        let ready = if let Some(key) = expected_session_key.as_deref() {
            wait_for_session_key(state, key, req.readiness_timeout).await
        } else {
            false
        };
        if !ready {
            return json!({
                "ok": false,
                "error": format!("File tab was created but did not become controllable within {} seconds", req.readiness_timeout),
                "error_code": "file_tab_readiness_timeout",
                "opened_tab_id": opened_tab_id,
                "opened_session_key": expected_session_key,
                "details": {
                    "timeout_seconds": req.readiness_timeout,
                    "tab_preserved": true,
                    "navigation_url": resolved.navigation_url
                }
            });
        }
    }

    // 新打开的标签页始终成为后续命令的默认目标，即使它是后台打开（不切前台）。
    //
    // 扩展创建标签页后，daemon 要等一次标签列表同步才会出现可操作会话。推送路径
    // 受 Service Worker 调度影响（实测可能延迟十几秒），这里改成主动拉一次列表，
    // 并在「新标签还没加载完、命令发不进去」时退到更老的会话重试。
    // 扩展已经把 tab id 返回了，会话不必再等一次列表同步才能用：
    // 先用执行者会话的元信息原地登记，后续命令立刻可寻址；等下一次标签快照
    // 到达时再用真实标题/URL 覆盖（apply_tabs_snapshot 会保留 connected_at）。
    let mut ready = false;
    if let (Some(executor_key), Some(key)) = (
        value.get("session_key").and_then(Value::as_str),
        expected_session_key.as_deref(),
    ) {
        if let Some(tab_id) = result
            .get("opened_tab_id")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            let url = result
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let title = result
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            ready = register_opened_session(state, executor_key, key, &tab_id, &url, &title).await;
        }
    }
    if !ready && refresh_ready {
        if let Some(key) = expected_session_key.as_deref() {
            // S1-04：刷新失败以前被 `let _ =` 整段吞掉，`ready:false` 没有任何原因可查
            // （调用方只能看到「标签打开了但不可操作」，分不清扩展断连还是刷新超时）。
            // 这里保留最后一次失败文本，三次都失败后挂到该 target 上；`ok` 语义不变。
            let mut last_refresh_error: Option<String> = None;
            for _ in 0..3 {
                match refresh_tabs_via_extension(state, Some(key), Duration::from_secs(5)).await {
                    Ok(_) => {
                        if wait_for_session_key(state, key, 0.2).await {
                            ready = true;
                            break;
                        }
                    }
                    Err(err) => last_refresh_error = Some(err.to_string()),
                }
                tokio::time::sleep(Duration::from_millis(60)).await;
            }
            if !ready {
                ready = wait_for_session_key(state, key, req.readiness_timeout.min(3.0)).await;
            }
            // 仍然没就绪：把最后一次刷新失败（如果有）连同归类一起带上。
            let refresh_message = if ready { None } else { last_refresh_error };
            if let Some(message) = refresh_message {
                if let Some(object) = result.as_object_mut() {
                    object.insert("refresh_error".to_string(), json!(message));
                    object.insert(
                        "refresh_error_code".to_string(),
                        json!(refresh_error_code(&message)),
                    );
                }
            }
        }
    }
    {
        let agent = current_agent();
        let active = {
            let driver = state.driver.read().await;
            expected_session_key
                .as_deref()
                .map(|key| driver.sessions.get(key).is_some_and(Session::is_active))
                .unwrap_or(false)
        };
        if let Some(key) = expected_session_key.clone() {
            if active {
                remember_agent_default(state, &agent, &key).await;
            } else {
                let mut defaults = state.agent_defaults.lock().await;
                let entry = defaults.entry(agent.clone()).or_default();
                entry.preferred_key = Some(key.clone());
                // 记下 owner：只有这个智能体能命中"待生效会话"，
                // 否则 A 打开后还没注册的标签会被 B 的第一条命令抢走并写成 B 的默认。
                entry.preferred_owner = Some(agent.clone());
            }
            if agent == "default" {
                let mut driver = state.driver.write().await;
                if active {
                    driver.default_session_key = Some(key);
                    driver.clear_preferred_default_session();
                } else {
                    // A2：记「待生效目标」必须同时写时间戳（6h TTL 的起点）。
                    driver.set_preferred_default_session(key);
                }
            }
        }
    }
    if let Some(object) = result.as_object_mut() {
        object.insert("ready".to_string(), json!(ready));
    }
    // 落在挑战页上时默认等站点自己放行；正常页面靠标题判别直接跳过，不付额外往返。
    if challenge_mode != "off" && ready {
        if let Some(key) = expected_session_key.as_deref() {
            let title = result
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let url = result
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if should_probe_challenge(&url, &title) {
                let outcome = auto_wait_challenge(state, key, challenge_timeout, open_active).await;
                if let Some(object) = result.as_object_mut() {
                    object.insert("challenge".to_string(), outcome);
                }
            }
        }
    }
    // --wait-for 条件等待。放在挑战处理之后：挑战页放行后站点往往还要
    // 跳转到真实页面，先等挑战再等条件才符合「打开后内容可用」的直觉。
    // 不带 --wait-for 时这段完全不执行，输出与旧版逐字节一致。
    let mut wait_outcome: Option<Value> = None;
    if let Some(cond) = wait_for {
        wait_outcome = Some(match expected_session_key.as_deref() {
            Some(key) if ready => run_wait_for(state, key, cond, req.readiness_timeout).await,
            _ => json!({
                "condition": cond,
                "matched": false,
                "error_code": "wait_for_timeout",
                "error": "标签页未能就绪，等待条件未执行（open 本身已失败或超时）",
            }),
        });
        let matched = wait_outcome
            .as_ref()
            .and_then(|w| w.get("matched"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(object) = result.as_object_mut() {
            object.insert(
                "wait_for".to_string(),
                wait_outcome.clone().unwrap_or(Value::Null),
            );
            // ready 语义升级：带 --wait-for 时 ready=true 表示「条件已满足」；
            // 不带时保持旧语义（仅标签可操作，不保证首屏有内容，README 已注明）。
            object.insert("ready".to_string(), json!(ready && matched));
        }
    }
    let ok = wait_outcome
        .as_ref()
        .map(|w| w.get("matched").and_then(Value::as_bool).unwrap_or(false))
        .unwrap_or(true);
    let mut response = json!({ "ok": ok, "result": result });
    if !ok {
        // 旧实现单 URL 打开失败也只在 result 里带信息；wait_for 未满足按同样的
        // 约定补顶层 error/error_code，智能体不用下钻一层就能判断失败原因。
        // error_code 必须是内层的真实病因（invalid_wait_for /
        // extension_unavailable / wait_for_timeout），不能一律写成超时 ——
        // 否则「条件拼错」会被当成「等得不够久」，调用方只会加大 --timeout 白等。
        response["error_code"] = wait_outcome
            .as_ref()
            .and_then(|w| w.get("error_code"))
            .filter(|code| !code.is_null())
            .cloned()
            .unwrap_or_else(|| json!("wait_for_timeout"));
        response["error"] = wait_outcome
            .as_ref()
            .and_then(|w| w.get("error"))
            .cloned()
            .unwrap_or(Value::Null);
    }
    response
}

/// --wait-for 条件的 JS 字符串字面量转义：选择器/文本是调用方给的任意字符串，
/// 不转义直接拼进 JS 会被引号/反斜杠打断，甚至注入出别的表达式。
fn js_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// `--wait-for` 一次等待的结论。
///
/// 旧实现把「条件写错」「扩展通道坏了」「真的等超时」全压成 `wait_for_timeout`：
/// 调用方看到超时会加大 --timeout 重试，而真正的原因（条件拼错 / CDP 附不上）
/// 永远不会因此好转。三类必须分开，各自带稳定 error_code。
enum WaitOutcome {
    Matched,
    /// 条件在预算内没有成立（唯一真正该叫 timeout 的一类）。
    Timeout(String),
    /// 条件表达式本身非法（daemon 侧也能直接判定）。
    Invalid(String),
    /// 等待通道故障：注入失败 / 扩展断连 / CSP 双通道都不可用。
    Extension(String),
}

/// --wait-for 的条件等待。
///
/// selector/text 复用 exec.rs 的 wait_for_js_condition_with_target（daemon 侧
/// 轮询注入，后台标签不受 Chrome 定时器节流影响），只调用不修改。
///
/// `networkidle` / `dom-quiet` 走**同一套近似实现**：
/// 页面内 Promise 等「readyState=complete 且 resource 条目数稳定 1s 以上、
/// 且连续 3 个 200ms 采样都没有变化」。它**不是** CDP 的「在途请求计数」，
/// 只是「看起来安静了」，所以对外只承诺 `dom-quiet` 这个语义；
/// `networkidle` 这个名字保留兼容，但不再是「真正意义上的 network idle」。
async fn run_wait_for(state: &AppState, session_key: &str, cond: &str, timeout: f64) -> Value {
    let started = Instant::now();
    // cond 在 CLI 侧已校验过前缀；这里仍按前缀分发，直接打 daemon API 的
    // 调用方拼错时明确报 invalid_wait_for，而不是装作「永远等不到」。
    let outcome = match cond {
        // `dom-quiet` 是这套实现的真实语义（DOM / resource 安静下来）。
        "dom-quiet" => wait_dom_quiet(state, session_key, timeout).await,
        // 兼容旧名字：实现已经是「稳定采样」，仍然只是近似，不代表 CDP 在途计数为 0。
        "networkidle" => wait_dom_quiet(state, session_key, timeout).await,
        _ => match cond.split_once(':') {
            Some((kind, needle)) => match kind {
                "selector" => {
                    let js = format!("!!document.querySelector({})", js_string_literal(needle));
                    wait_for_condition_js(state, session_key, &js, timeout).await
                }
                "text" => {
                    let js = format!(
                        "!!(document.body && document.body.innerText && document.body.innerText.includes({}))",
                        js_string_literal(needle)
                    );
                    wait_for_condition_js(state, session_key, &js, timeout).await
                }
                _ => WaitOutcome::Invalid(format!("未知的 --wait-for 条件类型 {kind:?}")),
            },
            None => WaitOutcome::Invalid(
                "--wait-for 缺少前缀（dom-quiet / selector:<CSS> / text:<文本>）".to_string(),
            ),
        },
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match outcome {
        WaitOutcome::Matched => json!({
            "condition": cond,
            "matched": true,
            "error_code": Value::Null,
            "error": Value::Null,
            "elapsed_ms": elapsed_ms,
        }),
        WaitOutcome::Timeout(message) => json!({
            "condition": cond,
            "matched": false,
            "error_code": "wait_for_timeout",
            "error": message,
            "wait_for_error": message.clone(),
            "elapsed_ms": elapsed_ms,
        }),
        // 条件写错：原样交回（CLI 侧映射为参数错误 / exit 2），不要伪装成超时。
        WaitOutcome::Invalid(message) => json!({
            "condition": cond,
            "matched": false,
            "error_code": "invalid_wait_for",
            "error": message,
            "wait_for_error": message.clone(),
            "elapsed_ms": elapsed_ms,
        }),
        // 通道故障：和「等不到」是两种病因，重试只会白等。
        WaitOutcome::Extension(message) => json!({
            "condition": cond,
            "matched": false,
            "error_code": "extension_unavailable",
            "error": message,
            "wait_for_error": message.clone(),
            "elapsed_ms": elapsed_ms,
        }),
    }
}

/// selector / text 条件：复用 daemon 侧轮询等待，并按错误形态分类。
async fn wait_for_condition_js(
    state: &AppState,
    session_key: &str,
    js: &str,
    timeout: f64,
) -> WaitOutcome {
    match wait_for_js_condition_with_target(
        state,
        Some(session_key),
        js,
        timeout,
        0.25,
        None,
        &ExecTargetOptions::default(),
    )
    .await
    {
        Ok(value) => {
            if value
                .get("matched")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                WaitOutcome::Matched
            } else {
                WaitOutcome::Timeout(format!("等待条件在 {timeout} 秒内未满足"))
            }
        }
        Err(err) => classify_wait_error(err.to_string(), timeout),
    }
}

/// 注入类错误分类：只有「时间用尽」才是 timeout，其余都算通道故障。
fn classify_wait_error(message: String, timeout: f64) -> WaitOutcome {
    let lower = message.to_lowercase();
    if lower.contains("exec_timeout") || lower.contains("timed out") || lower.contains("timeout") {
        WaitOutcome::Timeout(format!("等待条件在 {timeout} 秒内未满足（{message}）"))
    } else {
        WaitOutcome::Extension(message)
    }
}

/// 导航销毁执行上下文（页面正在跳转）—— 这种错误换一次注入就能好。
fn is_navigation_context_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "execution context was destroyed",
        "cannot find context",
        "context destroyed",
        "inspected target navigated",
        "target closed",
        "no frame",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// 近似 network idle：resource 计数稳定 + DOM 稳定 + 至少 1s 且连续 3 个
/// 200ms 采样无变化。
///
/// 明确它**不是**「所有请求都已完成」：`performance.getEntriesByType('resource')`
/// 只覆盖页面可观测的资源加载，WebSocket / 长轮询 / 在途 XHR 都不在计数里。
/// 因此对外只承诺 `dom-quiet` 语义；`networkidle` 仅作兼容别名。
async fn wait_dom_quiet(state: &AppState, session_key: &str, timeout: f64) -> WaitOutcome {
    let deadline = Instant::now() + secs(timeout.max(0.1));
    let mut retries = 0u32;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining < Duration::from_millis(50) {
            return WaitOutcome::Timeout(format!(
                "页面在 {timeout} 秒内没有安静下来（dom-quiet 是近似判定：只统计 resource 条目与 DOM 变化）"
            ));
        }
        match run_dom_quiet_once(state, session_key, remaining).await {
            Ok(true) => return WaitOutcome::Matched,
            // 页面自己放弃（readyState 一直不是 complete / 预算到）都算「没等到」。
            Ok(false) => {
                return WaitOutcome::Timeout(format!(
                    "页面在 {timeout} 秒内没有安静下来（dom-quiet 是近似判定：只统计 resource 条目与 DOM 变化）"
                ));
            }
            Err(err) => {
                let message = err.to_string();
                // 导航把执行上下文销毁了：换页后新页面可能立刻就是安静的，
                // 按剩余预算重试（最多 2 次）再判失败，不要直接把「正在跳转」报成「没安静」。
                if is_navigation_context_error(&message)
                    && retries < 2
                    && deadline.saturating_duration_since(Instant::now())
                        > Duration::from_millis(300)
                {
                    retries += 1;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }
                return classify_wait_error(message, timeout);
            }
        }
    }
}

/// 单次 dom-quiet 采样注入。返回 Ok(idle)。
async fn run_dom_quiet_once(state: &AppState, session_key: &str, budget: Duration) -> Result<bool> {
    let budget_ms = budget.as_millis().max(200) as u64;
    let js = format!(
        r#"(async () => {{
        const budget = {budget_ms};
        const start = performance.now();
        let lastCount = -1;
        let stableSamples = 0;
        let lastActivity = performance.now();
        let observer = null;
        try {{
            observer = new MutationObserver(() => {{ lastActivity = performance.now(); }});
            observer.observe(document.documentElement || document, {{ childList: true, subtree: true, attributes: true, characterData: true }});
        }} catch (e) {{}}
        const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
        try {{
            while (performance.now() - start < budget) {{
                if (document.readyState !== 'complete') {{
                    stableSamples = 0;
                    await sleep(200);
                    continue;
                }}
                const count = performance.getEntriesByType('resource').length;
                const quietMs = performance.now() - lastActivity;
                if (count === lastCount && quietMs >= 1000) {{
                    stableSamples += 1;
                }} else {{
                    stableSamples = 0;
                }}
                lastCount = count;
                // 连续 3 个 200ms 采样都没变、且 DOM 已安静 1s 以上。
                if (stableSamples >= 3) {{
                    return {{ idle: true, samples: stableSamples, resources: count }};
                }}
                await sleep(200);
            }}
            return {{ idle: false, samples: stableSamples, readyState: document.readyState }};
        }} finally {{
            if (observer) {{ try {{ observer.disconnect(); }} catch (e) {{}} }}
        }}
    }})()"#
    );
    let script = format!("return await ({js});");
    // 等待类注入发完消息就放会话锁：条件可能等几十秒，锁跟着等会把同一标签上的
    // 其它命令全部堵死（exec.rs 的 wait 路径就是这么做的）。
    let exec_timeout = budget + Duration::from_millis(3_000);
    let execution = execute_raw_js_releasing_lock(
        state,
        &script,
        exec_timeout,
        Some(session_key),
        &ExecTargetOptions::default(),
    )
    .await?;
    let payload = execution
        .result
        .data
        .or(execution.result.result)
        .unwrap_or(Value::Null);
    Ok(payload
        .get("idle")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

/// 一次请求打开多个目标：内部并发，失败按目标逐条返回。
///
/// 与「CLI 起 N 个线程各发一次请求」相比，这里只占一个 HTTP 往返，
/// 也避免 N 个请求各自去争同一把会话锁。
///
/// `wait_for`：`--wait-for` 条件，逐个目标在 `open_single` 里等待；
/// 某个目标等不到只把该目标标成 ok:false + wait_for_timeout，其余目标照常。
pub(crate) async fn open_many(
    state: &AppState,
    req: OpenRequest,
    wait_for: Option<String>,
) -> Value {
    let started = Instant::now();
    let targets = req.targets.clone();
    let concurrency = targets.len().clamp(1, 6);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut set = tokio::task::JoinSet::new();
    for (index, target) in targets.iter().cloned().enumerate() {
        let mut single = req.clone();
        single.targets.clear();
        single.target = Some(target.clone());
        single.url = Some(target.clone());
        let state = state.clone();
        let permits = semaphore.clone();
        // task-local 不随 `spawn` 继承：不套 `with_agent` 的话，这批并发打开的标签
        // 会全部记到 `"default"` 名下——默认目标写到别人的 agent 上，`cleanup` 也
        // 收不到自己开的页面。`multi.rs` / `run.rs` 都是这么包的。
        let agent = current_agent();
        let wait_for = wait_for.clone();
        set.spawn(with_agent(agent, async move {
            // S1-05：`acquire_owned().await.ok()` 会把 Err 吞成 None，然后**没有许可也照常
            // 打开**——并发上限直接失效（一批几十个目标同时压浏览器）。这里改成显式
            // 失败：该目标标记 `concurrency_closed`，绝不无许可继续执行。
            let _permit = match permits.acquire_owned().await {
                Ok(permit) => permit,
                Err(err) => {
                    eprintln!(
                        "open_many: 目标 {target} 并发许可获取失败（semaphore closed），该目标未执行: {err}"
                    );
                    return (
                        index,
                        target,
                        json!({
                            "ok": false,
                            "error": "concurrency_closed: 内部并发控制器已关闭，该目标未执行，可重试",
                            "error_code": "concurrency_closed"
                        }),
                        0u64,
                    );
                }
            };
            // 逐目标计时：顶层 targets 的 elapsed_ms 要能回答「哪一个慢」，旧实现
            // 只有整批 elapsed_ms，定位慢页面还得逐个再测。
            let started = Instant::now();
            let value = open_single(&state, single, false, wait_for.as_deref()).await;
            (index, target, value, started.elapsed().as_millis() as u64)
        }));
    }
    let mut slots: Vec<Option<(String, Value, u64)>> = vec![None; targets.len()];
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((index, target, value, elapsed_ms)) => {
                if let Some(slot) = slots.get_mut(index) {
                    *slot = Some((target, value, elapsed_ms));
                }
            }
            Err(err) => {
                eprintln!("open_many: 任务异常退出: {err}");
            }
        }
    }

    // 所有打开动作结束后统一刷新标签列表：并发打开时每条各自刷新会互相覆盖，
    // 最后这一步保证 daemon 的会话表和浏览器实际状态一致。
    // 扩展创建标签页与 chrome.tabs.query 之间存在毫秒级时序差，缺谁就再拉一次。
    let opened_keys: Vec<String> = slots
        .iter()
        .flatten()
        .filter_map(|(_, value, _)| {
            value
                .pointer("/result/opened_session_key")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let mut active_keys: HashSet<String> = HashSet::new();
    // S1-04：收尾刷新失败以前被 `let _ =` 吞掉，落单的目标只剩 `ready:false`，
    // 没有任何原因可查。这里保留最后一次失败文本/归类，挂到仍未就绪的条目与顶层。
    let mut last_refresh_error: Option<String> = None;
    for attempt in 0..3 {
        match refresh_tabs_via_extension(state, None, Duration::from_secs(5)).await {
            Ok(_) => last_refresh_error = None,
            Err(err) => last_refresh_error = Some(err.to_string()),
        }
        active_keys = {
            let driver = state.driver.read().await;
            driver
                .sessions
                .values()
                .filter(|session| session.is_active())
                .map(|session| session.session_key.clone())
                .collect()
        };
        if opened_keys.iter().all(|key| active_keys.contains(key)) {
            break;
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    }

    let mut entries = Vec::with_capacity(slots.len());
    let mut failed = 0usize;
    for (index, slot) in slots.into_iter().enumerate() {
        let (target, value, elapsed_ms) = match slot {
            Some(entry) => entry,
            None => {
                failed += 1;
                entries.push(json!({
                    "index": index,
                    "target": targets[index],
                    "ok": false,
                    "error": "打开任务异常退出",
                    "error_code": "task_panicked",
                    "elapsed_ms": Value::Null,
                }));
                continue;
            }
        };
        let opened_ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let result = value.get("result").cloned().unwrap_or(Value::Null);
        let session_key = result
            .get("opened_session_key")
            .and_then(Value::as_str)
            .map(str::to_string);
        let ready = session_key
            .as_deref()
            .map(|key| active_keys.contains(key))
            .unwrap_or(false);
        // --wait-for 的结果并入本条目。旧实现 ready 只代表「标签可操作」，
        // 带条件时必须以条件是否满足为准；等不到只挂这一条（ok:false +
        // wait_for_timeout），其它目标照常返回——不整体失败。
        let wait_info = result.get("wait_for").cloned().unwrap_or(Value::Null);
        let wait_matched = wait_info
            .get("matched")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let ok = opened_ok && wait_matched;
        let ready = ready && wait_matched;
        if !ok {
            failed += 1;
        }
        let mut entry = json!({
            "index": index,
            "target": target,
            "ok": ok,
            "tab_id": result.get("opened_tab_id").cloned().unwrap_or(Value::Null),
            "session_key": session_key,
            "url": result.get("url").cloned().unwrap_or(Value::Null),
            "title": result.get("title").cloned().unwrap_or(Value::Null),
            "ready": ready,
            "elapsed_ms": elapsed_ms,
        });
        if wait_info.is_object() {
            entry["wait_for"] = wait_info.clone();
        }
        if !ok {
            if !wait_matched {
                // 条件未满足：错误信息以 wait_for 的为准（里面写了是哪个条件、等多久），
                // error_code 也用内层的真实病因（：invalid_wait_for /
                // extension_unavailable / wait_for_timeout 不能一律压成超时）。
                entry["error"] = wait_info
                    .get("error")
                    .cloned()
                    .unwrap_or(json!("--wait-for 条件未满足"));
                entry["error_code"] = wait_info
                    .get("error_code")
                    .filter(|code| !code.is_null())
                    .cloned()
                    .unwrap_or_else(|| json!("wait_for_timeout"));
            } else {
                entry["error"] = value.get("error").cloned().unwrap_or(Value::Null);
                if let Some(code) = value.get("error_code") {
                    entry["error_code"] = code.clone();
                }
            }
        }
        // S1-04：这一条没就绪、并且收尾刷新确实失败过时，把失败原因挂到该条目上
        // （每条一个 `refresh_error`，`ok`/`error` 语义不变）。
        let refresh_message = if ready {
            None
        } else {
            last_refresh_error.as_deref()
        };
        if let Some(message) = refresh_message {
            entry["refresh_error"] = json!(message);
            entry["refresh_error_code"] = json!(refresh_error_code(message));
        }
        entries.push(entry);
    }

    // 先算归类再构造 JSON：同一个 json! 里既放原文又放它的派生结果，
    // 展开后容易踩借用顺序，分开算最直观。
    let top_refresh_code = last_refresh_error
        .as_deref()
        .map(refresh_error_code)
        .map(|code| json!(code))
        .unwrap_or(Value::Null);
    json!({
        "ok": failed == 0,
        "count": entries.len(),
        "failed": failed,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "tabs": entries,
        // 顶层也带一份（批量时便于一次看到「整批刷新都失败了」），无失败时为 null。
        "refresh_error": last_refresh_error,
        "refresh_error_code": top_refresh_code,
        "hint": if failed == 0 { Value::Null } else { json!("失败项带 error/error_code；成功的标签页已可用，失败的可重试该条") },
    })
}

/// 收尾刷新失败的归类（S1-04）：`ready:false` 必须能说明是「等超时」还是「扩展不可用」。
///
/// 不能靠错误类型区分——`refresh_tabs_via_extension` 的失败全部是 `anyhow` 文本
/// （超时分支自己拼的中文、扩展侧回包的英文……），所以只能按文本归类，
/// 拿不准就退到 `refresh_failed`，绝不假装成功。
pub(crate) fn refresh_error_code(message: &str) -> &'static str {
    let lower = message.to_lowercase();
    if ["超时", "timeout", "timed out"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return "refresh_timeout";
    }
    if [
        "未连接",
        "没有已连接",
        "not connected",
        "disconnected",
        "已断开",
        "extension_disconnected",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return "extension_unavailable";
    }
    "refresh_failed"
}

pub(crate) fn resolve_request_target<'a>(
    target: Option<&'a str>,
    url: Option<&'a str>,
) -> Result<&'a str, OpenTargetError> {
    match (target, url) {
        (Some(target), Some(url)) if target != url => Err(OpenTargetError {
            code: "ambiguous_open_target",
            message: "Open request contains conflicting target and url fields".to_string(),
            details: json!({ "target": target, "url": url }),
        }),
        (Some(target), _) => Ok(target),
        (_, Some(url)) => Ok(url),
        (None, None) => Err(OpenTargetError {
            code: "missing_open_target",
            message: "Open request requires target or url".to_string(),
            details: json!({}),
        }),
    }
}

pub(crate) fn open_target_error_json(err: OpenTargetError) -> Value {
    json!({
        "ok": false,
        "error": err.message,
        "error_code": err.code,
        "details": err.details,
    })
}

pub(crate) fn version_at_least(installed: &str, minimum: &str) -> bool {
    let parse = |value: &str| {
        value
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let mut installed = parse(installed);
    let mut minimum = parse(minimum);
    let len = installed.len().max(minimum.len());
    installed.resize(len, 0);
    minimum.resize(len, 0);
    installed >= minimum
}

/// 与 daemon 一起编译进二进制的扩展版本。
///
/// 浏览器里跑的是扩展目录的副本，改完文件不点「重新加载」Chrome 仍执行旧代码；
/// 这个版本号是判断「磁盘已更新但浏览器没生效」的唯一可靠依据。
pub(crate) fn bundled_extension_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        serde_json::from_str::<Value>(include_str!("../../assets/atb_bridge/manifest.json"))
            .ok()
            .and_then(|value| {
                value
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "0".to_string())
    })
}

/// 汇总「扩展是否需要重新加载」：连接中的扩展版本低于随包版本即视为过期。
pub(crate) fn extension_freshness(profiles: &[Value]) -> Value {
    let bundled = bundled_extension_version();
    let mut versions: Vec<String> = profiles
        .iter()
        .filter_map(|profile| profile.get("extension_version").and_then(Value::as_str))
        .filter(|version| !version.is_empty() && *version != "0")
        .map(str::to_string)
        .collect();
    versions.sort();
    versions.dedup();
    let stale_versions: Vec<&str> = versions
        .iter()
        .filter(|version| !version_at_least(version, bundled))
        .map(String::as_str)
        .collect();
    let stale = !stale_versions.is_empty();
    json!({
        "bundled_version": bundled,
        "connected_versions": versions,
        "stale": stale,
        "hint": if stale {
            json!(format!(
                "浏览器里的扩展仍是 {}，磁盘上已是 {}：打开 chrome://extensions 对本扩展点一次「重新加载」",
                stale_versions.join("/"),
                bundled
            ))
        } else {
            Value::Null
        },
    })
}

pub(crate) async fn wait_for_session_key(
    state: &AppState,
    session_key: &str,
    timeout: f64,
) -> bool {
    let deadline = Instant::now() + secs(timeout.max(0.1));
    loop {
        {
            let driver = state.driver.read().await;
            if driver
                .sessions
                .get(session_key)
                .is_some_and(Session::is_active)
            {
                return true;
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let remaining = deadline.saturating_duration_since(now);
        tokio::select! {
            _ = state.sessions_ready.notified() => {}
            _ = tokio::time::sleep(remaining.min(Duration::from_millis(100))) => {}
        }
    }
}

pub(crate) async fn normalize_open_result(state: &AppState, value: Value) -> Value {
    let opened = value.get("js_return").cloned().unwrap_or(Value::Null);
    let opened_tab_id = opened.get("id").and_then(|v| {
        v.as_i64()
            .map(|n| n.to_string())
            .or_else(|| v.as_str().map(str::to_string))
    });
    let executor_session_key = value
        .get("session_key")
        .and_then(Value::as_str)
        .map(str::to_string);
    let opened_session_key = if let Some(tab_id) = opened_tab_id.as_deref() {
        match executor_session_key.as_deref() {
            Some(executor_key) => derive_session_key(executor_key, tab_id),
            None => find_unique_session_key_by_tab_id(state, tab_id).await,
        }
    } else {
        None
    };
    json!({
        "status": "success",
        "opened_tab_id": opened_tab_id,
        "opened_session_key": opened_session_key,
        "window_id": opened.get("windowId").cloned().unwrap_or(Value::Null),
        "window": opened.get("window").and_then(Value::as_bool).unwrap_or(false),
        "url": opened.get("url").cloned().unwrap_or(Value::Null),
        "title": opened.get("title").cloned().unwrap_or(Value::Null),
        "active": opened.get("active").cloned().unwrap_or(Value::Null),
        // 只回调用方真正需要的东西：早期把扩展原始回包整体塞进 metadata.raw，
        // 一次开 6 个标签光这一项就多出上千 token，且与上面的字段完全重复。
        "group": opened.get("group").cloned().unwrap_or(Value::Null),
    })
}

pub(crate) async fn find_unique_session_key_by_tab_id(
    state: &AppState,
    tab_id: &str,
) -> Option<String> {
    let driver = state.driver.read().await;
    let mut matches = driver
        .sessions
        .values()
        .filter(|session| session.is_active() && session.tab_id == tab_id)
        .map(|session| session.session_key.clone());
    let session_key = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(session_key)
}

pub(crate) fn derive_session_key(
    executor_session_key: &str,
    opened_tab_id: &str,
) -> Option<String> {
    let mut parts = executor_session_key.splitn(3, ':');
    let browser_id = parts.next()?;
    let profile_id = parts.next()?;
    let executor_tab_id = parts.next()?;
    if browser_id.is_empty() || profile_id.is_empty() || executor_tab_id.is_empty() {
        return None;
    }
    Some(crate::protocol::make_session_key(
        browser_id,
        profile_id,
        opened_tab_id,
    ))
}
