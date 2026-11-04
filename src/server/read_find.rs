use super::*;

pub(crate) async fn read_page(
    State(state): State<AppState>,
    Json(req): Json<ReadRequest>,
) -> Json<Value> {
    touch(&state).await;
    Json(json_result(read_value(&state, req).await))
}

/// 结构化读取页面：一次注入返回正文、标题、元信息、表单与计数。
pub(crate) async fn read_value(state: &AppState, req: ReadRequest) -> Result<Value> {
    let include_hidden = req.include_hidden
        || req
            .mode
            .as_deref()
            .map(|mode| mode.eq_ignore_ascii_case("all") || mode.eq_ignore_ascii_case("hidden"))
            .unwrap_or(false);
    // clamp 只用来兜住极端值；实际值与请求值不同时必须显式告诉调用方。
    let applied_max_chars = req.max_chars.clamp(1_000, 400_000);
    let clamped_note = (applied_max_chars != req.max_chars).then(|| {
        json!({
            "field": "maxChars",
            "requested": req.max_chars,
            "applied": applied_max_chars,
            "min": 1_000,
            "max": 400_000,
        })
    });
    let mut opts = json!({
        "selector": req.selector,
        "maxChars": applied_max_chars,
        "links": req.links,
        "forms": req.forms,
        "includeHidden": include_hidden,
        "budgetMs": req.budget_ms,
        "maxNodes": req.max_nodes,
    });
    // `innerTextLimit` 是 page_read.js 的**节点数**阈值（页面侧默认 30000），
    // 不是字符数：节点数 <= 该值时用 innerText 精确抽取，超过则退化为近似可见
    // 文本（textMode:'visible-approx'）。此前 daemon 侧根本没有下发通道，
    // CLI 传什么都不会生效（read 侧）。
    // - None 不下发，保持页面默认；
    // - `0` 视为「未指定」而不是「最小上限」：页面脚本的约定就是 <=0 用默认
    //   （见 page_read.js 的参数约定注释），把它夹成 1000 会让「顺手写 0」的
    //   调用方在不知情的情况下切到激进抽取路径；
    // - 正数 clamp 到 [1000, 500000]：太小会让正常长页面直接走近似分支、
    //   太大则可能把几 MB 文本一次性塞进回包。
    if let Some(limit) = req.inner_text_limit.filter(|limit| *limit > 0) {
        opts["innerTextLimit"] = json!(limit.clamp(1_000, 500_000));
    }
    let script = format!("{}\nreturn __abcRead({});", html::js_page_read(), opts);
    let timeout = secs(req.timeout.unwrap_or(30.0).clamp(1.0, 120.0));
    let target = ExecTargetOptions {
        frame_id: req.frame_id,
        ..Default::default()
    };
    let selector = SessionSelector::new(
        req.switch_tab_id.clone(),
        req.browser.clone(),
        req.profile.clone(),
    );
    let executed =
        execute_page_js_with_target(state, &script, selector.clone(), true, timeout, &target).await;
    match executed {
        Ok(result) => {
            let mut payload = page_payload(result, "page");
            // A2：多浏览器并存且本次走隐式默认时，在顶层说明「默认落在哪个浏览器」。
            attach_multi_browser_hint(state, &mut payload, &selector).await;
            if let Some(note) = &clamped_note {
                payload["clamped"] = note.clone();
            }
            attach_challenge_if_blocked(state, &mut payload).await;
            Ok(payload)
        }
        Err(err) if is_csp_error(&err.to_string()) => {
            let session_key = select_tab(state, selector.clone()).await?;
            let page = page_op_via_extension(
                state,
                &session_key,
                "read",
                opts.clone(),
                req.frame_id,
                timeout,
            )
            .await?;
            let mut payload = json!({
                "tab_id": tab_id_of_session(&session_key),
                "session_key": session_key,
                "page": page,
                "via": "no-eval",
            });
            // A2：同一条 read 的降级路径也要给同样的提示（顶层附加字段）。
            attach_multi_browser_hint(state, &mut payload, &selector).await;
            if let Some(note) = &clamped_note {
                payload["clamped"] = note.clone();
            }
            attach_challenge_if_blocked(state, &mut payload).await;
            Ok(payload)
        }
        Err(err) => Err(err),
    }
}

pub(crate) async fn find_page(
    State(state): State<AppState>,
    Json(req): Json<FindRequest>,
) -> Json<Value> {
    touch(&state).await;
    Json(json_result(find_value(&state, req).await))
}

/// 元素查找：CSS / 文本 / XPath，返回可操作信息与简短 CSS 路径。
pub(crate) async fn find_value(state: &AppState, req: FindRequest) -> Result<Value> {
    if req.selector.is_none() && req.text.is_none() && req.xpath.is_none() {
        return Err(anyhow!("find: selector / text / xpath 至少需要一个"));
    }
    let spec = json!({
        "selector": req.selector,
        "text": req.text,
        "xpath": req.xpath,
        "limit": req.limit.clamp(1, 200),
        "visible": req.visible,
        "exact": req.exact,
    });
    let script = format!("{}\nreturn __abcFind({});", html::js_page_find(), spec);
    let timeout = secs(req.timeout.unwrap_or(20.0).clamp(1.0, 120.0));
    let target = ExecTargetOptions {
        frame_id: req.frame_id,
        ..Default::default()
    };
    let executed = execute_page_js_with_target(
        state,
        &script,
        SessionSelector::new(
            req.switch_tab_id.clone(),
            req.browser.clone(),
            req.profile.clone(),
        ),
        true,
        timeout,
        &target,
    )
    .await;
    match executed {
        Ok(result) => Ok(page_payload(result, "find")),
        Err(err) if is_csp_error(&err.to_string()) => {
            let session_key = select_tab(
                state,
                SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
            )
            .await?;
            let found = page_op_via_extension(
                state,
                &session_key,
                "find",
                spec.clone(),
                req.frame_id,
                timeout,
            )
            .await?;
            Ok(json!({
                "tab_id": tab_id_of_session(&session_key),
                "session_key": session_key,
                "find": found,
                "via": "no-eval",
            }))
        }
        Err(err) => Err(err),
    }
}

pub(crate) async fn action(
    State(state): State<AppState>,
    Json(req): Json<ActionRequest>,
) -> Json<Value> {
    touch(&state).await;
    Json(json_result(action_value(&state, req).await))
}

/// 单往返动作：hover / click / focus / select / check / uncheck / scroll / setValue。
pub(crate) async fn action_value(state: &AppState, req: ActionRequest) -> Result<Value> {
    let params = if req.params.is_null() {
        json!({})
    } else {
        req.params
    };
    let timeout = secs(req.timeout.unwrap_or(15.0).clamp(1.0, 120.0));
    let session_key = select_tab(
        state,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
    )
    .await?;
    let action = page_action_with_fallback(
        state,
        &session_key,
        &req.op,
        req.target.unwrap_or(Value::Null),
        params,
        timeout,
        req.frame_id,
    )
    .await?;
    ensure_page_action_ok(&action)?;
    Ok(json!({
        "tab_id": session_key.rsplit(':').next().unwrap_or_default(),
        "session_key": session_key,
        "action": action,
    }))
}

/// 在指定会话里执行一次页面动作，返回 `__abcAct` 的原始结果。
pub(crate) async fn run_page_action(
    state: &AppState,
    session_key: &str,
    op: &str,
    target: Value,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    run_page_action_in_frame(state, session_key, op, target, params, timeout, None).await
}

pub(crate) async fn run_page_action_in_frame(
    state: &AppState,
    session_key: &str,
    op: &str,
    target: Value,
    params: Value,
    timeout: Duration,
    frame_id: Option<i64>,
) -> Result<Value> {
    let script = format!(
        "{}\nreturn __abcAct({}, {}, {});",
        html::js_page_action(),
        serde_json::to_string(op)?,
        serde_json::to_string(&target)?,
        params
    );
    let target_options = ExecTargetOptions {
        frame_id,
        ..Default::default()
    };
    let result = execute_page_js_with_target(
        state,
        &script,
        SessionSelector::new(Some(session_key.to_string()), None, None),
        true,
        timeout,
        &target_options,
    )
    .await?;
    Ok(page_payload(result, "action")
        .get("action")
        .cloned()
        .unwrap_or(Value::Null))
}

pub(crate) async fn wait_page(
    State(state): State<AppState>,
    Json(req): Json<WaitRequest>,
) -> Json<Value> {
    touch(&state).await;
    // 等待超时的真实结论在内层 `ok` 上；用外层恒 true 的 `json_result` 会让
    // `atb wait --selector .never` 退出码 0。
    Json(json_result_with_inner_ok(
        wait_value(&state, req, None).await,
    ))
}

/// 等待条件成立：js / selector / text 三选一，轮询在 daemon 侧（后台标签页定时器会被节流）。
///
/// `cancel` 由异步任务传入：`--js` 轮询每轮开始前检查，selector/text 的观察器注入
/// 返回后再检查（页面里的观察器本身无法中断，只能等这一步做完）。
pub(crate) async fn wait_value(
    state: &AppState,
    req: WaitRequest,
    cancel: Option<&AtomicBool>,
) -> Result<Value> {
    let condition = if let Some(js) = req.js.as_deref().filter(|js| !js.trim().is_empty()) {
        js.to_string()
    } else if let Some(selector) = req.selector.as_deref().filter(|s| !s.trim().is_empty()) {
        if req.visible {
            format!(
                "const el = document.querySelector({}); if (!el) return false; const r = el.getBoundingClientRect(); return r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== 'hidden';",
                serde_json::to_string(selector)?
            )
        } else {
            format!(
                "return !!document.querySelector({});",
                serde_json::to_string(selector)?
            )
        }
    } else if let Some(text) = req.text.as_deref().filter(|s| !s.trim().is_empty()) {
        if req.visible {
            // `--text --visible` 以前被静默忽略（visible 只服务 selector 分支），
            // 隐藏文案也判命中。这里直接生成「按文本节点回溯到元素、
            // 再逐层判可见」的条件：不依赖页面脚本，也不改 assets/**。
            format!(
                concat!(
                    "const __t = {needle};",
                    "const __w = document.createTreeWalker(document.body || document.documentElement, NodeFilter.SHOW_TEXT);",
                    "while (__w.nextNode()) {{",
                    "  const __n = __w.currentNode;",
                    "  if (!__n.nodeValue || __n.nodeValue.indexOf(__t) < 0) continue;",
                    "  const __el = __n.parentElement; if (!__el) continue;",
                    "  const __r = __el.getBoundingClientRect();",
                    "  const __cs = getComputedStyle(__el);",
                    "  if (__r.width > 0 && __r.height > 0 && __cs.visibility !== 'hidden' && __cs.display !== 'none' && Number(__cs.opacity) > 0) return true;",
                    "}}",
                    "return false;"
                ),
                needle = serde_json::to_string(text)?
            )
        } else {
            format!(
                "return ((document.body && document.body.innerText) || '').includes({});",
                serde_json::to_string(text)?
            )
        }
    } else {
        return Err(anyhow!("wait: 需要 --js / --selector / --text 之一"));
    };

    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return Ok(json!({
            "condition": condition,
            "matched": false,
            "ok": false,
            "cancelled": true,
            "error": "cancelled",
            "mode": "cancelled",
        }));
    }

    let session_key = select_tab(
        state,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
    )
    .await?;
    let started = Instant::now();

    // selector / text 这类「等 DOM 出现」的条件：一次注入 + MutationObserver 完成，
    // 不再每 100ms 注入一次；后台标签页里 MutationObserver 不受定时器节流影响。
    if req.js.is_none() && (req.selector.is_some() || req.text.is_some()) {
        let opts = json!({
            "selector": req.selector,
            "text": req.text,
            "visible": req.visible,
            "timeoutMs": (req.timeout.max(0.1) * 1000.0) as u64,
            // 页面的巡检节流窗口：不传时 page_wait.js 会退回 50ms 默认值，
            // `wait --interval 1` 这种显式设置就被静默忽略了。
            "minIntervalMs": (req.interval * 1000.0).max(0.0) as u64,
        });
        let script = format!(
            "{}\nreturn await __abcWait({});",
            html::js_page_wait(),
            opts
        );
        let timeout = secs(req.timeout.max(0.1) + 5.0);
        let target = ExecTargetOptions {
            frame_id: req.frame_id,
            ..Default::default()
        };
        // 观察器是「一次注入、页面里等几十秒」——注入完就放锁。
        // 旧实现让整段等待都占着会话串行锁：一个 `wait --selector .never`
        // （默认 30s）能把同一标签上的 read / click 全部堵到超时，
        // 而等待本身并不需要串行。锁不在手上，超时/取消也能尽快返回。
        let executed = execute_page_js_with_target_releasing_lock(
            state,
            &script,
            SessionSelector::new(Some(session_key.clone()), None, None),
            true,
            timeout,
            &target,
        )
        .await;
        let outcome = match executed {
            Ok(result) => result.get("js_return").cloned().unwrap_or(Value::Null),
            Err(err) if is_csp_error(&err.to_string()) => {
                page_op_via_extension(
                    state,
                    &session_key,
                    "wait",
                    opts.clone(),
                    req.frame_id,
                    timeout,
                )
                .await?
            }
            Err(err) => return Err(err),
        };
        // 观察器注入是单次往返，取消只能在这一步返回后才生效。
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Ok(json!({
                "tab_id": tab_id_of_session(&session_key),
                "session_key": session_key,
                "condition": condition,
                "matched": false,
                "ok": false,
                "cancelled": true,
                "error": "cancelled",
                "mode": "observer",
                "elapsed_ms": started.elapsed().as_millis() as u64,
            }));
        }
        let mut body = json!({
            "tab_id": tab_id_of_session(&session_key),
            "session_key": session_key,
            "condition": condition,
            "matched": outcome.get("matched").cloned().unwrap_or(Value::Bool(false)),
            "ok": outcome.get("matched").cloned().unwrap_or(Value::Bool(false)),
            "polls": outcome.get("checks").cloned().unwrap_or(Value::Null),
            "value": outcome.get("value").cloned().unwrap_or(Value::Null),
            "error": outcome.get("error").cloned().unwrap_or(Value::Null),
            "mode": "observer",
            "elapsed_ms": started.elapsed().as_millis() as u64,
        });
        // `--text` 条件的取样方式（innerText / 超大 DOM 降级后的 textContent）。
        // 这里是白名单重建，漏掉就等于把 assets 侧的降级说明吞掉；
        // 页面没给该字段时保持「不出现」，不要用 null 占位。
        if let Some(mode) = outcome
            .get("text_match_mode")
            .filter(|value| !value.is_null())
        {
            body["text_match_mode"] = mode.clone();
        }
        return Ok(body);
    }

    let target = ExecTargetOptions {
        frame_id: req.frame_id,
        ..Default::default()
    };
    let outcome = wait_for_js_condition_with_target(
        state,
        Some(&session_key),
        &condition,
        req.timeout,
        req.interval,
        cancel,
        &target,
    )
    .await?;
    let mut body = json!({
        "tab_id": session_key.rsplit(':').next().unwrap_or_default(),
        "session_key": session_key,
        "condition": condition,
        "matched": outcome.get("matched").cloned().unwrap_or(Value::Bool(false)),
        "ok": outcome.get("ok").cloned().unwrap_or(Value::Bool(false)),
        "polls": outcome.get("polls").cloned().unwrap_or(Value::Null),
        "checks": outcome.get("checks").cloned().unwrap_or(Value::Null),
        "injections": outcome.get("injections").cloned().unwrap_or(Value::Null),
        "value": outcome.get("value").cloned().unwrap_or(Value::Null),
        "error": outcome.get("error").cloned().unwrap_or(Value::Null),
        "cancelled": outcome.get("cancelled").cloned().unwrap_or(Value::Bool(false)),
        "mode": outcome.get("mode").cloned().unwrap_or(json!("js-observer")),
        "elapsed_ms": started.elapsed().as_millis() as u64,
    });
    // 同一套白名单重建：页面对 --text 的降级说明不能在这里被吞掉。
    if let Some(mode) = outcome
        .get("text_match_mode")
        .filter(|value| !value.is_null())
    {
        body["text_match_mode"] = mode.clone();
    }
    Ok(body)
}
