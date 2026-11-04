use super::*;

/// 只在「看起来像挑战页」时才多注入一次做精确识别。
///
/// 正常页面不该为验证码检测多付一次往返；而 CF 拦截页的标题/正文特征很稳定，
/// 用几十字节的文本判断就够，命中后再让页面脚本给结构化结果。
pub(crate) async fn attach_challenge_if_blocked(state: &AppState, payload: &mut Value) {
    let page = payload.get("page").cloned().unwrap_or(Value::Null);
    let title = page
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let text = page
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // 这里的列表只决定「要不要多花一次往返去问页面脚本」，不用来下结论。
    // "access denied" 已移除：它是无数 403 页的通用标题，页面脚本侧也只当 suspected
    // （见 page_challenge.js 的 STRONG/WEAK 分档），留着只会让每个 403 页白付一次注入。
    let title_hit = [
        "just a moment",
        "attention required",
        "checking your browser",
        "verifying you are human",
        "请稍候",
        "安全验证",
        "人机验证",
    ]
    .iter()
    .any(|needle| title.contains(needle));
    let text_hit = text.chars().count() < 600 && {
        let lower = text.to_lowercase();
        [
            "verify you are human",
            "checking if the site connection",
            "enable javascript and cookies",
            "确认您是真人",
            "请完成安全验证",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    };
    if !title_hit && !text_hit {
        return;
    }
    let session_key = payload
        .get("session_key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if session_key.is_empty() {
        return;
    }
    if let Ok(challenge) = challenge_page_call(
        state,
        &session_key,
        json!({ "action": "status" }),
        None,
        Duration::from_secs(8),
    )
    .await
    {
        if let Some(object) = payload.as_object_mut() {
            object.insert("challenge".to_string(), challenge);
        }
    }
}

/// 值不值得为这个刚打开的标签页花一次往返去问「你是不是挑战页」。
///
/// 标题是最便宜的判据：CF 拦截页标题稳定为 Just a moment / Attention required 等。
/// 标题为空的也探一次——挑战页在拿到响应头之前标题就是空的，而正常页面几毫秒内
/// 就会有标题，这点开销（一次 page op）远小于「漏判导致后续命令全打在拦截页上」。
pub(crate) fn should_probe_challenge(url: &str, title: &str) -> bool {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return false;
    }
    let title = title.trim().to_lowercase();
    if title.is_empty() {
        return true;
    }
    const HINTS: [&str; 11] = [
        "just a moment",
        "attention required",
        "checking your browser",
        "verifying you are human",
        "one more step",
        "请稍候",
        "正在检查",
        "正在验证",
        "安全验证",
        "人机验证",
        "access denied",
    ];
    HINTS.iter().any(|hint| title.contains(hint))
}

/// open 之后的自动等待：识别一次，确认被拦才进入等待。
///
/// 复用 `challenge_value` 的 wait 分支，保证 `open --challenge` 与 `challenge wait`
/// 的判定口径、超时语义、need_human 行为完全一致——两条独立实现迟早会分叉。
pub(crate) async fn auto_wait_challenge(
    state: &AppState,
    session_key: &str,
    timeout_secs: f64,
    activate: bool,
) -> Value {
    // 识别要重试：刚 open 完的标签页可能还在导航中，页面脚本注入会撞上
    // 「执行上下文被销毁」，表现为 exec_timeout（ACK 收到、结果丢失）。
    // 这种失败跟「不是挑战页」是两回事，直接报错会让 open 的输出里多一条假故障；
    // 等页面落定后再探一次基本都能拿到结果。
    //
    // 另外文档还在 loading 时必须重探：CF 拦截页刚提交时标题是空的、DOM 还没渲染，
    // 此刻「检测不到挑战」是假阴性，直接返回 cleared:true 会让调用方以为已经进去了。
    let mut last_err = String::new();
    let mut got: Option<Value> = None;
    // `--challenge-timeout` 是**总预算**，不是每个阶段各给一份。
    // 旧实现：solve_budget=0.8T、reload 再等最多 8s、被动等待又拿完整 T，
    // 最坏 ≈1.8T+8s —— `--challenge-timeout 15` 实测能等 35~40s。
    // 从现在起所有阶段都只从 `remaining()` 里取，绝不重新起算。
    let overall_deadline = Instant::now() + secs(timeout_secs.clamp(1.0, 600.0));
    let budget_left = || overall_deadline.saturating_duration_since(Instant::now());
    for attempt in 0..12 {
        // C1-01：重试次数不能压过总预算。旧实现只在循环外算一次 deadline，
        // 循环里每次固定 8s，`--challenge-timeout 15` + 扩展挂死时这一小段
        // 最坏能等 12×8s —— 「识别」就把整条链路的预算吃光。
        let left = budget_left();
        if left.is_zero() {
            break;
        }
        // 剩余不足一次往返（50ms）时同样收手：用 0 超时发请求只能得到一次
        // 必然失败的调用，把「预算用完」误报成「扩展错误」。
        if left < Duration::from_millis(50) {
            break;
        }
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(250).min(left)).await;
        }
        match challenge_page_call(
            state,
            session_key,
            json!({ "action": "status" }),
            None,
            left.min(STATUS_CALL_TIMEOUT),
        )
        .await
        {
            // `Ok(Value::Null)`（页面脚本没返回值）不能当成「探测过了」：Null 上
            // blocking / interactive 全是 false，下面会直接判 cleared:true。
            Ok(status) if !challenge_status_usable(&status) => {
                last_err = "页面 status 返回空载荷（扩展未注入 / 页面脚本未执行）".to_string();
            }
            Ok(status) => {
                let pending = status
                    .get("pending")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                got = Some(status);
                // 最多为「还在加载」等 1.5s：再久就是慢站点的正常加载，
                // 不该让每次 open 都陪着等。
                if !pending || attempt >= 6 {
                    break;
                }
            }
            Err(err) => last_err = err.to_string(),
        }
    }
    let status = match got {
        Some(status) => status,
        // 识别失败不能算 open 失败：页面已经打开了，把原因带回去让调用方决定。
        None => {
            return json!({
                "detected": Value::Null,
                "waited": false,
                "error": humanize_extension_error(&last_err),
            })
        }
    };
    let blocking = status
        .get("blocking")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let interactive = status
        .get("interactive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let token_ready = status
        .get("token_ready")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // 文档还在 loading 时**不能**下结论。上面的重试最多只等 6×250ms≈1.5s，慢站点
    // （CF 边缘拥塞、大页面）超过这个时间还在 pending 很正常；而 pending 期间的
    // 「检测不到挑战」是假阴性，此时返回 cleared:true 会让 `open --challenge` 的
    // 调用方以为已经进去了（，与函数内注释直接矛盾）。
    // 如实回 `cleared:false + reason:"still_loading"`，让调用方自己决定要不要再等。
    if status
        .get("pending")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return json!({
            "detected": status.get("detected").cloned().unwrap_or(json!(false)),
            "waited": false,
            "cleared": false,
            "reason": "still_loading",
            "vendor": status.get("vendor").cloned().unwrap_or(Value::Null),
            "kind": status.get("kind").cloned().unwrap_or(Value::Null),
        });
    }
    // 「没被拦」不等于「已经放行」：页面上挂着一个没勾的 Turnstile 组件时，
    // blocking 是 false（能浏览），但提交前必须过它。旧实现只看 blocking，
    // 于是 open 在这种页面上返回 cleared:true —— 调用方以为进去了，
    // 后续命令全打在挑战页上。判定改成「有组件且没 token」也算未完成。
    if !blocking && (!interactive || token_ready) {
        return json!({
            "detected": status.get("detected").cloned().unwrap_or(json!(false)),
            "waited": false,
            "cleared": true,
            "vendor": status.get("vendor").cloned().unwrap_or(Value::Null),
            "kind": status.get("kind").cloned().unwrap_or(Value::Null),
        });
    }
    // 硬拦截（CF 的 "Sorry, you have been blocked"、error 1020/1015）：点也点不了、
    // 等也不会放行。直接把结论带回去，别让 open 白耗十几秒再报超时。
    if status.get("kind").and_then(Value::as_str) == Some("blocked") {
        return json!({
            "detected": true,
            "waited": false,
            "cleared": false,
            "blocked": true,
            "vendor": status.get("vendor").cloned().unwrap_or(Value::Null),
            "kind": "blocked",
            "hints": status.get("hints").cloned().unwrap_or_else(|| json!([])),
        });
    }
    // 后台标签的定时器会被 Chrome 节流，CF/DataDome 的挑战脚本跑不完 —— 实测同一个
    // 挑战页前台 1~3s 放行、后台 20~30s 仍在转圈。所以**只要判定为拦截，就必须把这个
    // 标签切成 active**，`--background` 也不例外：那个参数的本意是「开页面别打断我」，
    // 而不是「停在挑战页上永远过不去」。allowFocus:false 只切标签、不抢系统焦点。
    // 切不过去时挑战一定过不了（定时器被节流），必须把原因带回给调用方：
    // 否则 open 只会报一句「等了 N 秒没清」，看不出是标签根本没激活（C1-08）。
    let mut diag: Vec<Value> = Vec::new();
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
    let _ = activate;
    // 先主动点一次验证码组件（带勾选框的 Turnstile 等只会等待是过不去的）。
    // 预算取总超时的 80%（上限 25s）：点完还要等它自己验证，Turnstile 常常要 5-20s
    // 才写出 token，旧的 12s 上限会在「马上要过」的时候掐断，随后 open 报 need_human，
    // 用户看到的就是「工具有时候能过、有时候不能」。剩下的时间留给被动等待。
    //
    // 一律走一次主动求解，不再拿「页面脚本说有没有可点组件」当门。
    //
    // 原因：CF 的组件是**异步**渲染进 closed shadow root 的，open 完成的那一刻
    // 它往往还没建出来，页面脚本如实报 interactive=false；等组件出现时，
    // 旧代码已经进了被动等待分支，再也不会去点。求解器本身是元素驱动的
    // （每轮先看 token/是否离开挑战页，再看组件在不在），没有组件时它会很快
    // 返回 no-widget-frame，不会白耗预算；而要点的页面只有它能过。
    let solve_cap = Duration::from_millis((timeout_secs * 800.0) as u64)
        .min(Duration::from_secs(25))
        .max(Duration::from_millis(2000));
    // 从总预算里扣，而不是重新给 0.8T。
    let solve_budget = budget_left().min(solve_cap);
    let mut solved = challenge_auto_solve(state, session_key, solve_budget, None, None).await;
    // 一轮没解掉时整页 reload 再解一次：CF 每次下发的挑战不是同一个 ray，
    // 卡住的那一轮换个 ray 往往就过了。只在「预算还剩得下第二轮」时才做，
    // 并且 reload 后用元素确认页面已经落定（不是睡固定时间）。
    if solved.get("solved").and_then(Value::as_bool) != Some(true) {
        let reason = solved.get("reason").and_then(Value::as_str).unwrap_or("");
        let worth_retry = matches!(reason, "verifying_not_completed" | "click-no-effect");
        let remaining = solve_budget.saturating_sub(Duration::from_millis(
            solved
                .get("elapsed_ms")
                .and_then(Value::as_u64)
                .unwrap_or(default_solve_ms(solve_budget)),
        ));
        // 第二轮只能花「总预算里真正剩下的」：reload 与等待都算在内，至少留 4s。
        if worth_retry
            && remaining > Duration::from_secs(4)
            && budget_left() > Duration::from_secs(4)
        {
            let _ = execute_page_js_with_target(
                state,
                "location.reload(); return 'reload'",
                SessionSelector::new(Some(session_key.to_string()), None, None),
                true,
                Duration::from_secs(5),
                &ExecTargetOptions::default(),
            )
            .await;
            // 等页面重新落定：status 不再是 pending 才算新挑战页就绪。
            // 单次查询的超时同样取「本循环剩余」与「总预算剩余」的较小值（C1-01）：
            // 旧实现固定 6s，reload 窗口只有 8s 时最后那次调用能把总预算顶穿。
            let reload_deadline = Instant::now() + Duration::from_secs(8).min(budget_left());
            while Instant::now() < reload_deadline {
                let left = reload_deadline
                    .saturating_duration_since(Instant::now())
                    .min(budget_left());
                if left < Duration::from_millis(50) {
                    break;
                }
                if let Ok(status) = challenge_page_call(
                    state,
                    session_key,
                    json!({ "action": "status" }),
                    None,
                    left.min(Duration::from_secs(6)),
                )
                .await
                {
                    let pending = status
                        .get("pending")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if !pending {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            solved =
                challenge_auto_solve(state, session_key, remaining.min(budget_left()), None, None)
                    .await;
        }
    }
    if solved.get("solved").and_then(Value::as_bool) == Some(true) {
        return json!({
            "detected": true,
            "waited": true,
            "cleared": true,
            "via": "click",
            "vendor": status.get("vendor").cloned().unwrap_or(Value::Null),
            "kind": status.get("kind").cloned().unwrap_or(Value::Null),
            "auto_solve": solved,
            // 成功与否都带上：诊断数组为空时字段为空数组，不影响旧消费方。
            "diag": diag,
        });
    }
    if !blocking {
        // 组件类挑战点了也没过：如实说需要人，绝不能再报 cleared:true。
        return json!({
            "detected": true,
            "waited": false,
            "cleared": false,
            "need_human": true,
            "vendor": status.get("vendor").cloned().unwrap_or(Value::Null),
            "kind": status.get("kind").cloned().unwrap_or(Value::Null),
            "hints": status.get("hints").cloned().unwrap_or_else(|| json!([])),
            "auto_solve": solved,
            "diag": diag,
        });
    }
    // 直接轮询 `status`（350ms 一次）而不是注入「4 秒一片」的等待脚本。
    //
    // 后者必须等一整片结束才回报，页面导航时还会撞上「执行上下文被销毁」，
    // 恢复得慢；纯 interstitial 页面本来就不需要页面内观察器（没有任何可点/可等的东西，
    // 只是等 CF 脚本跑完并跳转）。实测同一次挑战：分片等待 12~15s 仍未识别为放行，
    // 轮询 350ms 在 1s 内就能看到 `detected:false`。
    let budget = budget_left();
    let deadline = Instant::now() + budget;
    let (cleared, last, _verifying) =
        challenge_wait_result(state, session_key, budget, deadline).await;
    // hints 以**最后一轮**的为准：首轮往往还在「页面没跑完」的阶段，
    // 而「Turnstile 没渲染」「窗口不可见」这些结论只有后面才出得来。
    let hints = last
        .get("hints")
        .cloned()
        .filter(|value| {
            value
                .as_array()
                .map(|items| !items.is_empty())
                .unwrap_or(false)
        })
        .or_else(|| status.get("hints").cloned())
        .unwrap_or_else(|| json!([]));
    // 没放行时要给出**可操作的原因**。只回一个 `need_human: true` 等于没说：
    // 用户无从判断是「再等等」「点一下」还是「这台机器根本过不去」。
    let reason = if cleared {
        Value::Null
    } else if last.get("stalled").and_then(Value::as_bool) == Some(true) {
        json!("turnstile_not_rendered")
    } else if last
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
        .unwrap_or(false)
    {
        json!("window_not_visible")
    } else {
        json!("timeout")
    };
    json!({
        "detected": true,
        "waited": true,
        "cleared": cleared,
        "need_human": !cleared,
        "reason": reason,
        "vendor": status.get("vendor").cloned().unwrap_or(Value::Null),
        "kind": status.get("kind").cloned().unwrap_or(Value::Null),
        "hints": hints,
        "env": last.get("env").cloned().unwrap_or(Value::Null),
        "turnstile": last.get("turnstile").cloned().unwrap_or(Value::Null),
        // 求解过程的结论要跟着一起交出去：只回一个 need_human，用户无从知道
        // 是「组件没找到」「点了没反应」还是「点了但 CF 一直不给结果」。
        "auto_solve": solved,
        "stalled_ms": last.get("stalled_ms").cloned().unwrap_or(Value::Null),
        "wait": last,
        "diag": diag,
    })
}

/// 会出现「人机验证组件」的 frame URL 特征。
/// 命中才有必要做几何定位；普通 iframe（广告、编辑器）一律不碰。
pub(crate) const CHALLENGE_FRAME_HINTS: [&str; 9] = [
    "challenges.cloudflare.com",
    "cdn-cgi/challenge-platform",
    "recaptcha/api2",
    "recaptcha/enterprise",
    "hcaptcha.com",
    "captcha-delivery.com",
    "px-captcha",
    "awswaf.com",
    "arkoselabs.com",
];

/// 一个验证码 iframe 在页面视口里的几何。
///
/// 只保留点击真正需要的字段（四边形 + 尺寸）：DOM nodeId 在拿到几何后就没用了，
/// 留着会让人误以为后续还能拿它二次定位（页面一重排就失效）。
pub(crate) struct ChallengeFrame {
    pub(crate) url: String,
    /// content box 四角（TL, TR, BR, BL），已包含 CSS transform 的效果。
    pub(crate) quad: [f64; 8],
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl ChallengeFrame {
    /// 局部坐标（iframe 内容坐标，CSS px）→ 页面视口坐标。
    ///
    /// 不能简单相加：有的站点（如 nowsecure.nl 的测试页）给挑战 iframe 加 3D 旋转，
    /// 纯加法点不到勾选框。这里用四角反推仿射基向量，旋转/缩放/斜切都能对上。
    pub(crate) fn to_screen(&self, lx: f64, ly: f64) -> (f64, f64) {
        let [x1, y1, x2, y2, _x3, _y3, x4, y4] = self.quad;
        let ux = x2 - x1;
        let uy = y2 - y1;
        let vx = x4 - x1;
        let vy = y4 - y1;
        let u = if self.width > 0.0 {
            lx / self.width
        } else {
            0.0
        };
        let v = if self.height > 0.0 {
            ly / self.height
        } else {
            0.0
        };
        (x1 + u * ux + v * vx, y1 + u * uy + v * vy)
    }
}

/// 取一个 iframe 元素「真正加载的文档 URL」。
///
/// 不能只读 src 属性：CF 的挑战 iframe 常常先用 `srcdoc`/属性为空、再用 JS 赋
/// `contentWindow.location` 或 `document.write` 填充，属性值读不到真实地址。
/// describeNode(depth:0) 的 contentDocument.documentURL 是实际导航地址，更可靠。
pub(crate) async fn iframe_doc_url(
    state: &AppState,
    session_key: &str,
    node_id: i64,
    timeout: Duration,
) -> String {
    let described = cdp_call(
        state,
        session_key,
        "DOM.describeNode",
        json!({ "nodeId": node_id, "depth": 0, "pierce": true }),
        timeout,
    )
    .await;
    if let Ok(result) = described {
        if let Some(data) = result.data {
            if let Some(url) = data
                .pointer("/node/contentDocument/documentURL")
                .and_then(Value::as_str)
            {
                if !url.is_empty() {
                    return url.to_string();
                }
            }
        }
    }
    // 退一步读属性（src / data-src）。
    let attrs = cdp_call(
        state,
        session_key,
        "DOM.getAttributes",
        json!({ "nodeId": node_id }),
        timeout,
    )
    .await
    .ok()
    .and_then(|result| result.data)
    .and_then(|data| data.get("attributes").and_then(Value::as_array).cloned())
    .unwrap_or_default();
    let mut src = String::new();
    for pair in attrs.chunks(2) {
        let Some(name) = pair.first().and_then(Value::as_str) else {
            continue;
        };
        let value = pair.get(1).and_then(Value::as_str).unwrap_or_default();
        if name.eq_ignore_ascii_case("src") || name.eq_ignore_ascii_case("data-src") {
            src = value.to_string();
        }
    }
    src
}

/// 收集文档里所有 iframe 的 nodeId（常规 DOM + shadow root 内部，去重）。
///
/// 只在已确认「页面存在挑战」时调用，那时页面很小，全量 pierce 的代价可控；
/// 换来的是不漏掉 shadow root 里的组件。
///
/// 第二个返回值是成败标记：**查询本身失败**（CDP 附不上 / DOM 域不可用）
/// 与「文档里真的没有 iframe」是两件事。旧实现把两种都返回空集合，求解器就把
/// 「查不出来」当成「组件已被摘掉」= 点击被接收，然后整轮求解停手等一个永不到来的结论。
pub(crate) async fn collect_iframe_ids(
    state: &AppState,
    session_key: &str,
    timeout: Duration,
) -> (Vec<i64>, bool) {
    let mut ids: Vec<i64> = Vec::new();
    // 至少有一次 DOM.getDocument 成功，才算「查过了」。
    let mut queried = false;
    if let Ok(result) = cdp_call(
        state,
        session_key,
        "DOM.getDocument",
        json!({ "depth": 1, "pierce": true }),
        timeout,
    )
    .await
    {
        if let Some(root_id) = result
            .data
            .as_ref()
            .and_then(|data| data.pointer("/root/nodeId"))
            .and_then(Value::as_i64)
        {
            queried = true;
            if let Ok(found) = cdp_call(
                state,
                session_key,
                "DOM.querySelectorAll",
                json!({ "nodeId": root_id, "selector": "iframe" }),
                timeout,
            )
            .await
            {
                if let Some(list) = found
                    .data
                    .as_ref()
                    .and_then(|data| data.get("nodeIds"))
                    .and_then(Value::as_array)
                {
                    ids.extend(list.iter().filter_map(Value::as_i64));
                }
            }
        }
    }
    // querySelectorAll 不穿 shadow root，补一遍 pierce 全量树。
    if let Ok(result) = cdp_call(
        state,
        session_key,
        "DOM.getDocument",
        json!({ "depth": -1, "pierce": true }),
        timeout,
    )
    .await
    {
        if let Some(root) = result.data.as_ref().and_then(|data| data.get("root")) {
            queried = true;
            collect_iframe_nodes(root, &mut ids);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    (ids, !queried)
}

/// 从 pierce 过的 DOM 树里收集 iframe 的 nodeId（含 shadow root 内部）。
///
/// 深度上限与 `html.rs` 的 `MAX_DOM_DEPTH` 同阈值：`DOM.getDocument{depth:-1}`
/// 给出的是整棵文档树，构造一个几千层嵌套的页面就能让这个递归在 tokio worker
/// 的 2MB 栈上爆栈（实测栈溢出是 abort，不是 panic）。正常页面（含深层组件库）
/// 远到不了 512 层，命中即停止下钻——少收集几个深层 iframe 也不会漏掉验证码组件，
/// 因为验证码 iframe 都挂在可见的浅层容器里。
pub(crate) const MAX_IFRAME_DOM_DEPTH: usize = 512;

pub(crate) fn collect_iframe_nodes(node: &Value, out: &mut Vec<i64>) {
    collect_iframe_nodes_at(node, out, 0);
}

fn collect_iframe_nodes_at(node: &Value, out: &mut Vec<i64>, depth: usize) {
    if depth > MAX_IFRAME_DOM_DEPTH {
        return;
    }
    if node.get("nodeName").and_then(Value::as_str) == Some("IFRAME") {
        if let Some(id) = node.get("nodeId").and_then(Value::as_i64) {
            out.push(id);
        }
    }
    for key in ["children", "shadowRoots"] {
        if let Some(children) = node.get(key).and_then(Value::as_array) {
            for child in children {
                collect_iframe_nodes_at(child, out, depth + 1);
            }
        }
    }
}

/// CDP 附加失败是不是「页面里嵌了别的扩展的 iframe」造成的。
///
/// Chrome 的硬限制：一个标签页里只要存在 `chrome-extension://<别的扩展>` 的 iframe，
/// chrome.debugger 就拒绝 attach（"Cannot access a chrome-extension:// URL of
/// different extension"）。这不是本工具的 bug，但会让验证码定位/点击整条链路失效，
/// 所以必须识别出来并给出可操作的处理，而不是报一句 "no iframe element in document"。
pub(crate) fn is_foreign_extension_frame_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("different extension")
        || lower.contains("chrome-extension:// url")
        || message.contains("内嵌了其他扩展")
}

/// 摘掉页面里别的扩展注入的 iframe，让 chrome.debugger 能 attach。
///
/// 只处理当前文档这一次：这类扩展会在下一次导航时重新注入，做成持久化反而会
/// 影响用户其它页面。返回值是摘掉的个数，写进 diag 让排障能看到「为什么突然能用了」。
///
/// 失败必须返回 `Err`（C1-07）：旧实现 `.ok()...unwrap_or(0)` 把「脚本根本没跑」
/// 和「跑完了、一个都没摘掉」压成同一个 0，排障时完全看不出 attach 为什么还是失败。
pub(crate) async fn strip_foreign_extension_frames(
    state: &AppState,
    session_key: &str,
) -> Result<u64> {
    let script = "const frames = Array.prototype.slice.call(document.querySelectorAll('iframe'))\
         .filter(function (frame) { return String(frame.src || '').indexOf('chrome-extension://') === 0; });\
         frames.forEach(function (frame) { try { frame.remove(); } catch (_) {} });\
         return frames.length;";
    let executed = execute_page_js_with_target(
        state,
        script,
        SessionSelector::new(Some(session_key.to_string()), None, None),
        true,
        Duration::from_secs(5),
        &ExecTargetOptions::default(),
    )
    .await?;
    executed
        .get("js_return")
        .cloned()
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_f64().map(|number| number as u64))
        })
        // 没有计数就等于「不知道摘了几个」：不能让调用方以为已经处理干净了。
        .ok_or_else(|| anyhow!("strip-foreign-frames: 页面脚本未返回摘除计数"))
}
