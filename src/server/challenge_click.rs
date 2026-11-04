use super::*;

/// 找出页面上所有验证码 iframe 及其屏幕几何。
///
/// 走 DOM 查而不是 `Page.getFrameTree`：跨源 iframe 是独立进程（OOPIF），
/// 主目标的 frame tree 里看不到它们（实测只返回主框架 + about:blank 两个），
/// 而验证码组件恰恰全是跨源的。`DOM.querySelectorAll('iframe')` 拿的是元素本身，
/// 跨不跨源都一样；盒模型还自带 CSS transform 后的四角坐标。
///
/// 第二个返回值是排障诊断：定位要过 文档 → iframe 元素 → 盒模型 三关，
/// 任何一关失败都表现为「没找到组件」，没有诊断就只能瞎猜。
///
/// 第三个返回值 `query_failed` 是判据：**查询本身失败**（CDP 附不上 /
/// DOM 域不可用 / 回包缺失）与「页面上真的没有 iframe」必须分开。旧实现把两种
/// 情况都压成空集合，求解决策里「组件消失」被当成「点击被接收」（widget-removed），
/// 于是一次 attach 失败就让整轮求解提前停手等结论。
pub(crate) async fn challenge_frames(
    state: &AppState,
    session_key: &str,
    timeout: Duration,
) -> (Vec<ChallengeFrame>, Vec<Value>, bool) {
    let mut diag: Vec<Value> = Vec::new();
    // DOM 域没启用时 getAttributes / getBoxModel 会报错；启用本身对页面不可见。
    let enable = cdp_call(state, session_key, "DOM.enable", json!({}), timeout).await;
    // 页面里挂着**别的扩展**注入的 iframe 时，Chrome 会直接拒绝 chrome.debugger.attach
    // （"Cannot access a chrome-extension:// URL of different extension"）。
    // 这种 iframe 对页面内容没有贡献，却能让整条验证码链路失效（定位、点击、截图全废）。
    // 先摘掉再 attach 一次；扩展会在下次导航时自己重新注入，不做持久化。
    let attach_error = match &enable {
        Err(err) => Some(err.to_string()),
        Ok(result) if !result.ok => Some(result.error.clone().unwrap_or_default()),
        Ok(_) => None,
    };
    if let Some(message) = attach_error {
        if is_foreign_extension_frame_error(&message) {
            // C1-07：摘除失败必须单独记一条诊断。以前失败被压成 stripped:0，
            // 与「摘了 0 个」完全同形 —— attach 依然失败时根本看不出原因。
            let mut entry = json!({ "step": "attach", "error": "foreign_extension_iframe" });
            match strip_foreign_extension_frames(state, session_key).await {
                Ok(stripped) => entry["stripped"] = json!(stripped),
                Err(err) => diag.push(json!({
                    "step": "strip-foreign-frames",
                    "error": humanize_extension_error(&err.to_string()),
                })),
            }
            diag.push(entry);
            let _ = cdp_call(state, session_key, "DOM.enable", json!({}), timeout).await;
        }
    }
    let (iframe_ids, query_failed) = collect_iframe_ids(state, session_key, timeout).await;
    if iframe_ids.is_empty() {
        if query_failed {
            diag.push(json!({ "step": "query", "error": "frames_query_failed" }));
            return (Vec::new(), diag, true);
        }
        diag.push(json!({ "step": "query", "error": "no iframe element in document" }));
        return (Vec::new(), diag, false);
    }

    let mut out = Vec::new();
    let mut seen: Vec<Value> = Vec::new();
    for node_id in iframe_ids {
        let url = iframe_doc_url(state, session_key, node_id, timeout).await;
        if seen.len() < 8 {
            seen.push(json!({ "node": node_id, "url": url.chars().take(120).collect::<String>() }));
        }
        if !CHALLENGE_FRAME_HINTS.iter().any(|hint| url.contains(hint)) {
            continue;
        }
        let model = match cdp_call(
            state,
            session_key,
            "DOM.getBoxModel",
            json!({ "nodeId": node_id }),
            timeout,
        )
        .await
        {
            Ok(result) if result.ok => result.data.unwrap_or(Value::Null),
            Ok(result) => {
                diag.push(json!({ "step": "getBoxModel", "node": node_id, "error": result.error }));
                continue;
            }
            Err(err) => {
                diag.push(
                    json!({ "step": "getBoxModel", "node": node_id, "error": err.to_string() }),
                );
                continue;
            }
        };
        let content = model
            .pointer("/model/content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if content.len() < 8 {
            diag.push(json!({ "step": "geometry", "node": node_id, "error": "no content quad" }));
            continue;
        }
        let n = |idx: usize| content.get(idx).and_then(Value::as_f64).unwrap_or_default();
        let quad = [n(0), n(1), n(2), n(3), n(4), n(5), n(6), n(7)];
        let width = ((quad[2] - quad[0]).powi(2) + (quad[3] - quad[1]).powi(2)).sqrt();
        let height = ((quad[6] - quad[0]).powi(2) + (quad[7] - quad[1]).powi(2)).sqrt();
        if width < 4.0 || height < 4.0 {
            diag.push(json!({ "step": "geometry", "node": node_id, "error": "zero-size", "w": width, "h": height }));
            continue;
        }
        out.push(ChallengeFrame {
            url,
            quad,
            width,
            height,
        });
    }
    if out.is_empty() {
        diag.push(json!({ "step": "match", "error": "no challenge iframe", "iframes": seen }));
    }
    // 面积大的排前面：页面里常有一个「小尺寸 / 隐形」的挑战 iframe（站点自己的检测框、
    // CF 的 hidden 探针），DOM 顺序上它可能排第一。调用方取 `frames.first()` 时就会
    // 永远点到那个点不动的组件。`width/height` 已经在上面按 >=4.0 校验过。
    out.sort_by(|left, right| {
        (right.width * right.height)
            .partial_cmp(&(left.width * left.height))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    (out, diag, false)
}

/// 用可信鼠标事件点一个页面视口坐标。
///
/// 这是给「智能体自己点」的兜底通道（`challenge click --at x,y`）：组件已经在
/// 页面上、坐标也拿到了，但它需要一次真实鼠标事件。事件之间**不插入 sleep**——
/// 间距由 CDP 往返自然产生；从左侧 24px 滑入只是为了让 Turnstile 看到
/// mouseenter → mousemove → mousedown 的完整序列。
///
/// 每一次 `Input.*` 都走 `into_ok` 检查扩展的 `ok` 字段。旧实现只看
/// 传输层 `?`，`{ok:false}`（未 attach / 参数非法 / 目标已销毁）被当成「已发出」。
pub(crate) async fn challenge_click_at(
    state: &AppState,
    session_key: &str,
    x: f64,
    y: f64,
    timeout: Duration,
) -> Result<()> {
    const MOUSE: &str = "Input.dispatchMouseEvent";
    for step in 1..=4 {
        let t = f64::from(step) / 4.0;
        let px = x - 24.0 * (1.0 - t);
        let py = y + if step % 2 == 0 { 1.0 } else { -1.0 };
        cdp_call(
            state,
            session_key,
            MOUSE,
            json!({ "type": "mouseMoved", "x": px, "y": py, "button": "none", "buttons": 0, "pointerType": "mouse" }),
            timeout,
        )
        .await?
        .into_ok(MOUSE)?;
    }
    cdp_call(
        state,
        session_key,
        MOUSE,
        json!({ "type": "mouseMoved", "x": x, "y": y, "button": "none", "buttons": 0, "pointerType": "mouse" }),
        timeout,
    )
    .await?
    .into_ok(MOUSE)?;
    // 按下成功之后**必须**抬起：页面会一直停在 mousedown 状态，
    // 后续 click 事件永远不触发，组件卡在「按下」态。所以按下与抬起之间不放任何
    // 可能提前返回的步骤，唯一可能失败的是 release 自己。
    cdp_call(
        state,
        session_key,
        MOUSE,
        json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "buttons": 1, "clickCount": 1, "pointerType": "mouse" }),
        timeout,
    )
    .await?
    .into_ok(MOUSE)?;
    cdp_call(
        state,
        session_key,
        MOUSE,
        json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "buttons": 0, "clickCount": 1, "pointerType": "mouse" }),
        timeout,
    )
    .await?
    .into_ok(MOUSE)?;
    Ok(())
}

/// 点击后等结论。
///
/// CF 点完勾选框不会立刻放行，而是切到「Verifying you are human. This may take
/// a few seconds.」再要 3-10 秒。这段窗口里重复点击会把流程打回原点，
/// 所以这里持续轮询到出结论或窗口用尽，并把 verifying 状态回传给调用方。
pub(crate) async fn challenge_wait_result(
    state: &AppState,
    session_key: &str,
    max_wait: Duration,
    budget_end: Instant,
) -> (bool, Value, bool) {
    let deadline = (Instant::now() + max_wait).min(budget_end);
    let mut last = Value::Null;
    let mut verifying = false;
    // 失败诊断（C1-08）：只有发生失败时才会往里放，返回结构在成功路径上不变。
    let mut diag: Vec<Value> = Vec::new();
    // 只在第一次发现「窗口不可见」时恢复一次：反复切标签会把正在跑的验证打断，
    // 而已经跑废的挑战也不会因为窗口恢复就复活（那需要调用方 reload）。
    let mut env_restored = false;
    // 「已经拦了多久」由 daemon 记并下发（页面脚本刻意不写 window.*，见）。
    let mut first_blocking_at: Option<Instant> = None;
    loop {
        tokio::time::sleep(Duration::from_millis(350)).await;
        // C1-01（同一预算约束链）：单次 status 查询只能用**本函数剩余**里的量。
        // 固定 8s 时，扩展挂死会让「被动等待」在 deadline 之后再多耗 8s，
        // `--challenge-timeout 15` 就会被顶成 23s。
        let left = deadline.saturating_duration_since(Instant::now());
        if left < Duration::from_millis(50) {
            return (false, attach_wait_diag(last, &diag), verifying);
        }
        // 只有「真的探测过」的 status 才能参与判定：空载荷（Value::Null）上
        // detected / blocking 全是 false，下面的 `!detected` 会立刻判 cleared:true，
        // 这正是假通过路径。拿不到结论就按「还没结论」继续等/超时。
        let Some(status) = challenge_page_call(
            state,
            session_key,
            json!({
                "action": "status",
                "stalledMs": first_blocking_at
                    .map(|at| at.elapsed().as_millis() as u64)
                    .unwrap_or(0),
            }),
            None,
            left.min(Duration::from_secs(8)),
        )
        .await
        .ok()
        .filter(challenge_status_usable) else {
            if Instant::now() >= deadline {
                return (false, attach_wait_diag(last, &diag), verifying);
            }
            continue;
        };
        let flag = |key: &str| status.get(key).and_then(Value::as_bool).unwrap_or(false);
        let token_ready = flag("token_ready");
        let detected = flag("detected");
        let blocking = flag("blocking");
        let interactive = flag("interactive");
        verifying = flag("verifying") || verifying;
        if blocking || interactive {
            first_blocking_at.get_or_insert_with(Instant::now);
        } else if !detected {
            first_blocking_at = None;
        }

        // 窗口最小化时 `outerWidth=0`、`visibilityState='hidden'`，CF 的 Turnstile
        // 在这种页面里**不会执行**（实测连 iframe 都不创建）。`--background` 打开时
        // 窗口往往就是最小化的，所以先恢复一次窗口，再继续等。
        if !env_restored {
            let env_bad = status
                .get("env")
                .map(|env| {
                    let hidden = env.get("hidden").and_then(Value::as_bool).unwrap_or(false);
                    let outer_w = env
                        .get("outer")
                        .and_then(Value::as_array)
                        .and_then(|items| items.first())
                        .and_then(Value::as_f64)
                        .unwrap_or(1.0);
                    hidden || outer_w <= 0.0
                })
                .unwrap_or(false);
            if env_bad {
                env_restored = true;
                // C1-08：切不过去就别静默失败 —— 后面只会看到「等超时」，
                // 而真正原因是窗口没被激活（最小化窗口里 Turnstile 根本不跑）。
                if let Err(err) = extension_control(
                    state,
                    session_key,
                    "tabs",
                    json!({ "method": "switch", "allowFocus": false }),
                    DEFAULT_EXTENSION_CONTROL_TIMEOUT,
                )
                .await
                {
                    diag.push(json!({
                        "step": "activate-tab",
                        "error": humanize_extension_error(&err.to_string()),
                    }));
                }
            }
        }

        last = status.clone();
        if token_ready || !detected || (!blocking && !interactive) {
            return (true, attach_wait_diag(status, &diag), verifying);
        }
        // 静默挂死：页面侧已确认「Turnstile 没渲染」并超过阈值 —— 继续等只会把
        // `--challenge-timeout` 耗完，早点把结论和原因交出去，比一个 need_human 有用。
        if flag("stalled") {
            return (false, attach_wait_diag(status, &diag), verifying);
        }
        if Instant::now() >= deadline {
            return (false, attach_wait_diag(last, &diag), verifying);
        }
    }
}

/// 把失败诊断并入返回给调用方的 status 载荷（C1-08）。
///
/// 没有诊断时不加任何字段：成功路径的输出结构与旧版完全一致，只在真的
/// 发生过「切标签失败」时才多一个 `diag` 数组。
fn attach_wait_diag(status: Value, diag: &[Value]) -> Value {
    if diag.is_empty() {
        return status;
    }
    match status {
        Value::Object(mut map) => {
            map.insert("diag".to_string(), json!(diag));
            Value::Object(map)
        }
        // 非对象（通常只有 `Value::Null`）时整体换成一个只带诊断的对象。
        _ => json!({ "diag": diag }),
    }
}
