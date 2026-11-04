use super::*;

#[derive(Debug, Deserialize)]
pub(crate) struct ChallengeRequest {
    /// status（默认）| wait | focus
    pub(crate) action: Option<String>,
    #[serde(default)]
    pub(crate) timeout: Option<f64>,
    #[serde(default)]
    pub(crate) interval: Option<f64>,
    /// 等多久还没过就把标签页带到前台交给人工（秒，0 表示不聚焦）
    #[serde(default)]
    pub(crate) focus_after: Option<f64>,
    #[serde(default)]
    pub(crate) focus: Option<bool>,
    /// 等这么久仍被拦就整页 reload 一次再继续等（秒，0 = 不 reload）。
    /// 卡住的挑战页多半是服务端已经放了 cookie、页面脚本没走完，reload 一次比死等有效。
    #[serde(default)]
    pub(crate) reload_after: Option<f64>,
    /// action=click 时直接点的页面坐标（x,y）：自动求解失败后由调用方自己点。
    #[serde(default)]
    pub(crate) point: Option<[f64; 2]>,
    /// 页面侧「判定清除需要稳定多久」（毫秒，`confirmMs`）。None 表示不下发，
    /// 保留页面默认 250ms。挑战页刚导航提交时是空白文档，确认窗口太短会把
    /// 「还没渲染出挑战」当成「已经放行」。
    #[serde(default)]
    pub(crate) confirm_ms: Option<f64>,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) frame_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StealthRequest {
    /// audit（默认，识别页面可被站点读取的自动化痕迹）| scrub（清除本工具自身痕迹）
    #[serde(default)]
    pub(crate) action: Option<String>,
    #[serde(default)]
    pub(crate) timeout: Option<f64>,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) frame_id: Option<i64>,
}

pub(crate) async fn stealth_page(
    State(state): State<AppState>,
    Json(req): Json<StealthRequest>,
) -> Json<Value> {
    touch(&state).await;
    Json(json_result(stealth_value(&state, req).await))
}

/// 审计 / 清理当前页面的自动化痕迹。
///
/// 不伪造指纹（UA、canvas、WebGL 一律不动）：那类改动会把真实浏览器也弄坏，
/// 且属于「假装成别人」。这里只做「别主动暴露自己」+「把已经留下的痕迹清掉」。
pub(crate) async fn stealth_value(state: &AppState, req: StealthRequest) -> Result<Value> {
    let action = req
        .action
        .clone()
        .unwrap_or_else(|| "audit".to_string())
        .to_lowercase();
    if !matches!(action.as_str(), "audit" | "status" | "scrub" | "clean") {
        return Err(anyhow!(
            "stealth: 未知 action {action}（可选 audit / scrub）"
        ));
    }
    let normalized = if matches!(action.as_str(), "scrub" | "clean") {
        "scrub"
    } else {
        "audit"
    };
    let session_key = select_tab(
        state,
        SessionSelector::new(
            req.switch_tab_id.clone(),
            req.browser.clone(),
            req.profile.clone(),
        ),
    )
    .await?;
    let timeout = secs(req.timeout.unwrap_or(15.0).clamp(1.0, 120.0));
    let payload = stealth_page_call(
        state,
        &session_key,
        json!({ "action": normalized }),
        req.frame_id,
        timeout,
    )
    .await?;
    if payload.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(anyhow!(
            "stealth {normalized}: {}",
            payload
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("页面脚本执行失败")
        ));
    }
    // 反检测能力一半在扩展里（page ops 走 ISOLATED world、角标默认关闭）。
    // 浏览器里还挂着旧扩展时，审计会显示"干净"但实际仍在写页面全局 —— 必须说清楚。
    let extension_version = {
        let driver = state.driver.read().await;
        driver
            .sessions
            .get(&session_key)
            .and_then(|session| session.extension_version.clone())
    };
    let extension_ok = extension_version
        .as_deref()
        .map(|version| version_at_least(version, "5.0.0"))
        .unwrap_or(false);
    // 输出按 action 分叉：audit 给 score/findings/env，scrub 给 restored/removed/remaining。
    // 不再把整份页面 payload 再嵌一层 `page` —— 同一份数据两倍 token，而消费方是智能体。
    let mut result = json!({
        "tab_id": tab_id_of_session(&session_key),
        "session_key": session_key,
        "action": normalized,
        "extension_version": extension_version,
        "extension_ok": extension_ok,
    });
    let object = result.as_object_mut().expect("stealth result 一定是对象");
    for key in [
        "score",
        "level",
        "headline",
        "findings",
        "env",
        "self_fixable",
        "url",
        "title",
        "restored",
        "removed",
        "remaining",
        "note",
    ] {
        if let Some(value) = payload.get(key) {
            // null 字段直接不输出：JSON 里一堆 null 只会让调用方多判断一次。
            if !value.is_null() {
                object.insert(key.to_string(), value.clone());
            }
        }
    }
    if normalized == "scrub" {
        // 清理没有分数：把 note 抬成 headline，避免调用方去读一个恒为 null 的字段。
        if let Some(note) = payload.get("note").cloned() {
            object.insert("headline".to_string(), note);
        }
    }
    if !extension_ok {
        object.insert(
            "fixes".to_string(),
            json!([
                "用本仓库 assets/atb_bridge 覆盖扩展目录，再到 chrome://extensions 点「重新加载」",
                "重载后执行 atb stealth audit，确认 findings 里没有 own_globals / own_dom_badge",
            ]),
        );
    }
    // 页面脚本判定失败（scrub 仍有残留）时压成 ok:false，CLI 才会给非 0 退出码。
    if payload.get("ok").and_then(Value::as_bool) == Some(false) {
        object.insert("ok".to_string(), json!(false));
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// 挑战自动通过：主动定位验证码组件，用可信鼠标事件点它
// ---------------------------------------------------------------------------
//
// 背景：CF 的「Just a moment」JS 挑战多半自己会过（等就是了），但**带勾选框的
// Turnstile 不会**——它必须收到真实的鼠标事件。之前 `challenge solve` 只是「等 +
// 超时转人工」，所以 `open --challenge auto` 会在一个仍被拦住的页面上返回
// cleared:true，调用方以为进去了，实际所有后续命令都打在挑战页上。
//
// 这里补上真正缺的那一环：几何定位 + Input.dispatchMouseEvent。

/// 挑战轮询节拍。
///
/// 这不是「等一会儿再假设已经过了」：每次醒来都先读元素（token / 组件目标 / 页面是否还是挑战页），
/// 条件成立立刻返回。120ms 只是避免热循环把 daemon 打满。
pub(crate) const CHALLENGE_POLL: Duration = Duration::from_millis(120);

/// 一次点击之后，多久还没有任何元素变化就认为这一下被吞了。
/// Turnstile 收到有效点击会在 ~1s 内摘掉（或替换）那个 iframe；超过这个窗口还没动静，
/// 就是点在「还没就绪的组件」上了 —— 这种点击不留任何状态，可以安全重试。
pub(crate) const CLICK_ACK_WINDOW: Duration = Duration::from_millis(3000);

/// 单次求解最多点几下。被吞的点击没有副作用；但每一下都必须由元素状态驱动，
/// 不能变成「隔一秒点一下」的机械点按。
pub(crate) const MAX_CLICKS: u32 = 6;

/// 相邻鼠标事件之间的间隔。这是**事件节奏**，不是「等状态」：
/// Turnstile 会统计 mousemove 的时间分布，瞬移+点击拿不到结果。
pub(crate) const EVENT_GAP: Duration = Duration::from_millis(35);
/// 页面侧 status 的复查间隔：CDP 的组件签名负责主节拍，status 只用来下最终结论，
/// 不必每一轮都注入脚本。
pub(crate) const STATUS_INTERVAL: Duration = Duration::from_millis(400);
/// 按下到抬起的保持时长，接近真人点按。
pub(crate) const PRESS_HOLD: Duration = Duration::from_millis(80);

/// 勾选框在 iframe 本地坐标里的落点。
///
/// 实测（CDP 无障碍树）：紧凑型 Turnstile 300×65 里的 checkbox 是一个 24×24 的方块，
/// 局部坐标约 (9,21)-(33,45)，中心 (21,33) —— 即宽度的 7%、高度的 50%。
/// 之前按 (30, h/2) 点，落点压在这行文案上：label 有时会转发、有时直接吞掉，
/// 表现就是「点十次成三次」。
/// 整页挑战 iframe 里组件居中，按同样的相对位置换算。
pub(crate) fn challenge_click_point(frame: &ChallengeFrame) -> (f64, f64) {
    if frame.height <= 96.0 {
        (frame.width * 0.07, frame.height * 0.5)
    } else {
        let widget_w = 300.0_f64.min(frame.width);
        let widget_h = 65.0_f64.min(frame.height);
        (
            (frame.width - widget_w) / 2.0 + widget_w * 0.07,
            (frame.height - widget_h) / 2.0 + widget_h * 0.5,
        )
    }
}

/// 点组件里的一个局部坐标，鼠标轨迹**全部落在组件内部**。
///
/// 之前的轨迹从 `(x-90, y-60)` 起步，那已经在组件外面甚至窗口外（x 可能是负数），
/// Turnstile 看到的是一次「凭空出现」的点击。这里改成从组件左内侧出发、水平靠近目标：
/// 鼠标进入 iframe → 连续 mousemove → 按下 → 抬起，是它期望的那条路径。
pub(crate) async fn challenge_click_widget(
    state: &AppState,
    session_key: &str,
    frame: &ChallengeFrame,
    lx: f64,
    ly: f64,
    timeout: Duration,
) -> Result<()> {
    // 轨迹与节奏照抄「人手靠近」的形态：
    //   - 事件全部落在组件内部（从左侧 8% 处滑入），保证 mouseenter → mousemove 序列完整；
    //   - 相邻事件之间留 ~35ms 间隔。这不是「等某个条件」，而是 Turnstile 会统计
    //     mousemove 的时间分布：一次性把 move/press/release 全打出去（<10ms）时，
    //     点击会被接收（页面照样进「正在验证」），但服务端始终不给结果。
    //     实测同一台机器、同一个 profile：带节奏 1.4s 通过，不带节奏永远卡住。
    // 起点必须在组件**外面**：Turnstile 靠 mouseenter（跨越边界那一下）把勾选框从
    // 「静态渲染」切到「可交互」。起点直接放在组件内部时，页面照样会进入「正在验证」，
    // 但服务端始终不给结果 —— 这是实测出来的差别。
    // 负的局部坐标经仿射映射后落在组件左侧的页面空白处，正好用来产生这次跨越。
    let points = [(-40.0, ly + 2.0), (-18.0, ly + 1.0), (-6.0, ly), (lx, ly)];
    const MOUSE: &str = "Input.dispatchMouseEvent";
    for (px, py) in points {
        let (sx, sy) = frame.to_screen(px, py);
        // 扩展回 `{ok:false}` 时必须停下 —— 鼠标没进到组件里，
        // 后面的按下/抬起只会点在一个不存在的光标位置上。
        cdp_call(
            state,
            session_key,
            MOUSE,
            json!({ "type": "mouseMoved", "x": sx, "y": sy, "button": "none", "buttons": 0, "pointerType": "mouse" }),
            timeout,
        )
        .await?
        .into_ok(MOUSE)?;
        tokio::time::sleep(EVENT_GAP).await;
    }
    let (sx, sy) = frame.to_screen(lx, ly);
    // 按下 → 保持 → 抬起这段时间里，只有 press 的成败会影响要不要抬起：
    // 按下成功就**一定**要释放（页面停在 mousedown 会让组件永远收不到 click）。
    // Rust 没有异步 Drop，所以用「显式收尾」实现 finally 语义：
    // press 失败也 best-effort 抬一次（状态未知），但不覆盖 press 的错误。
    let pressed = cdp_call(
        state,
        session_key,
        MOUSE,
        json!({ "type": "mousePressed", "x": sx, "y": sy, "button": "left", "buttons": 1, "clickCount": 1, "pointerType": "mouse" }),
        timeout,
    )
    .await
    .and_then(|result| result.into_ok(MOUSE));
    match pressed {
        Ok(_) => {
            tokio::time::sleep(PRESS_HOLD).await;
            cdp_call(
                state,
                session_key,
                MOUSE,
                json!({ "type": "mouseReleased", "x": sx, "y": sy, "button": "left", "buttons": 0, "clickCount": 1, "pointerType": "mouse" }),
                timeout,
            )
            .await?
            .into_ok(MOUSE)?;
            Ok(())
        }
        Err(err) => {
            // 按下没成/回包是失败：按钮状态未知，best-effort 抬起一次，
            // 原始错误优先（抬起失败只说明「可能残留按下态」，不该盖住真正原因）。
            let _ = cdp_call(
                state,
                session_key,
                MOUSE,
                json!({ "type": "mouseReleased", "x": sx, "y": sy, "button": "left", "buttons": 0, "clickCount": 1, "pointerType": "mouse" }),
                timeout,
            )
            .await
            .and_then(|result| result.into_ok(MOUSE));
            Err(err)
        }
    }
}

/// 求解预算的毫秒数（用于算「还剩多少」）。
pub(crate) fn default_solve_ms(budget: Duration) -> u64 {
    budget.as_millis() as u64
}

/// 单次页面 status 查询的超时上限（实际还会被本轮剩余预算再截断）。
pub(crate) const STATUS_CALL_TIMEOUT: Duration = Duration::from_secs(8);
/// 单次 CDP 几何定位的超时上限（同上，会被剩余预算截断）。
pub(crate) const FRAME_CALL_TIMEOUT: Duration = Duration::from_secs(6);

/// 页面 status 载荷是否真的「探测过」。
///
/// `challenge_page_call` 的 eval 兜底在页面脚本没有返回值时给的是 `Value::Null`，
/// 而 Null 上 `blocking` / `interactive` / `token_ready` 全是 `None` → 一律按 `false`
/// 处理。于是 `!blocking && !interactive` 恒成立、`token_ready || !detected` 恒成立，
/// 「扩展失联 + 页面禁 eval」就被说成「没有挑战、已通过」。
/// 只有带 `detected`（或 `blocking`）布尔字段的载荷才算探测成功 —— 这是
/// page_challenge.js 的 status 一定会写的两个字段。
pub(crate) fn challenge_status_usable(status: &Value) -> bool {
    status
        .as_object()
        .map(|object| {
            object.get("detected").and_then(Value::as_bool).is_some()
                || object.get("blocking").and_then(Value::as_bool).is_some()
        })
        .unwrap_or(false)
}

/// 把调用方给的确认时长（毫秒）塞进页面 op 参数。
///
/// 键名固定为 `confirmMs`（`page_challenge.js` 读的就是它，缺省 250ms）。
/// `None` 时**不下发**该键：老页面脚本不认识它，多一个字段只会增加载荷。
/// 负值 / NaN / 无穷一律视为未传——下发给页面只会得到一个无意义的计时窗口。
pub(crate) fn with_confirm_ms(mut payload: Value, confirm_ms: Option<f64>) -> Value {
    let Some(ms) = confirm_ms.filter(|value| value.is_finite() && *value >= 0.0) else {
        return payload;
    };
    if let Value::Object(map) = &mut payload {
        map.insert("confirmMs".to_string(), json!(ms));
    }
    payload
}

/// 主动过挑战：元素出现就点，点完只看元素有没有变化。
///
/// 规则只有四条，全部可观测：
///
/// 1. **组件出现**（`Target.getTargets` 里有 `challenges.cloudflare.com` 的 iframe，
///    且几何尺寸 > 100px）→ 立刻点，不预热、不等待；
/// 2. **点击被吞**（`CLICK_ACK_WINDOW` 内组件目标既没消失也没换新，页面也没有进入
///    「正在验证」）→ 重新点（最多 `MAX_CLICKS` 次）。被吞的点击不留状态，重点无副作用；
/// 3. **点击被接收**（组件目标消失 / URL 换成新的 / 页面文案变成「正在验证」）→
///    停止一切点击，只等页面结论。Turnstile 验证期间再点一下会把流程打回原点；
/// 4. **通过**（`cf-turnstile-response` 有值，或已经离开挑战页）→ 立刻返回。
///
/// 没有「睡 1.2s 等它就绪」「隔 5s 再点」这种假设：所有推进都由元素状态驱动。
pub(crate) async fn challenge_auto_solve(
    state: &AppState,
    session_key: &str,
    budget: Duration,
    frame_id: Option<i64>,
    confirm_ms: Option<f64>,
) -> Value {
    let started = Instant::now();
    // `budget` 是硬截止：每一轮探测前都按它算「还剩多少」，单次查询的超时也必须
    // 用剩余量截断。旧实现每次固定 6s(几何) + 8s(status)，只想在循环头判
    // `elapsed < budget` —— 最后一轮可以轻松越过 --timeout（P3 预算越界）。
    let deadline = started + budget;
    let mut clicks = 0u32;
    let mut tried: Vec<Value> = Vec::new();
    let mut last_status = Value::Null;
    let mut diag: Vec<Value> = Vec::new();
    let mut saw_widget = false;
    let mut widget_url: Option<String> = None;
    let mut last_click_at: Option<Instant> = None;
    // 点击已被接收的证据：widget-removed / widget-replaced / verifying。
    let mut accepted: Option<&'static str> = None;
    // 页面上可能同时有两个（甚至更多）验证码组件（站点自己的检测框 +
    // 真正的组件，或两套验证码）。旧实现只点 `frames.first()`：第一个点不动时
    // 整段预算都耗在它身上，等大的第二个组件永远轮不到。
    // 现在按面积序（`challenge_frames` 已排好）取前 3 个可用组件**轮转**点击，
    // 每个组件在同一轮里只点一次，共享同一份预算；任一被接收即停止点击。
    // 只有一个组件时轮转退化成「反复点它」，与旧行为完全一致。
    const MAX_FRAME_CANDIDATES: usize = 3;
    let mut frame_cursor: usize = 0;
    // 派发尝试数（含失败的派发）用来兜住 MAX_CLICKS，避免 CDP 持续报错时热循环。
    let mut attempts = 0u32;
    let mut clicked_frames: Vec<String> = Vec::new();
    let mut frame_attempts: Vec<Value> = Vec::new();
    // 几何诊断只在第一轮收一次：后面每轮都补会让 diag 膨胀几十倍。
    let mut took_frame_diag = false;
    // 「查询失败」诊断也只记一次。
    let mut noted_frames_query_failed = false;

    // 后台标签的定时器会被 Chrome 节流到分钟级，Turnstile 的轮询/上报要跑几十秒。
    // 失败必须留诊断：切不过去时挑战永远等不到结果，而返回里只会出现
    // 「click-no-effect / 超时」，调用方无从知道真正原因（C1-08）。
    if let Err(err) = extension_control(
        state,
        session_key,
        "tabs",
        json!({ "method": "switch", "allowFocus": false }),
        DEFAULT_EXTENSION_CONTROL_TIMEOUT,
    )
    .await
    {
        diag.push(
            json!({ "step": "activate-tab", "error": humanize_extension_error(&err.to_string()) }),
        );
    }

    // 页面侧 status 要注入一段脚本、还要扫正文，比 CDP 查询重得多；
    // 120ms 一次会把挑战页的主线程占满，反而拖慢组件渲染（实测会导致整段
    // 预算内都看不到组件）。这里用 CDP 的组件签名做主循环节拍，status 降频复查。
    let mut next_status_at = Instant::now();
    // `--frame` 下挑战在子框架里，而 challenge_frames 只走主文档的 pierce DOM：
    // 它给出的坐标是主框架元素的位置，点下去可能命中同位置的另一个元素。
    // 这种场景只在该框架内做状态判定，绝不派发主框架坐标的点击（P3）。
    let frame_scoped = frame_id.is_some();
    if frame_scoped {
        diag.push(json!({ "step": "frame", "error": "frame-scope-unsupported" }));
    }
    while started.elapsed() < budget {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        // ① 页面自己的元素结论优先：拿到 token 或已经离开挑战页，立刻收工。
        let status_now = Instant::now() >= next_status_at;
        if status_now {
            next_status_at = Instant::now() + STATUS_INTERVAL;
        }
        if status_now {
            // C1-02：`Ok` 不等于「探测过」。`challenge_page_call` 在页面脚本没有返回值时
            // 给 `Value::Null`，Null 上 token_ready / detected 全是 None（按 false 处理），
            // 于是 `token_ready || !detected` 恒真 —— 旧实现在这里直接返回 solved:true，
            // 把「扩展失联 / 空载荷」报成「挑战已通过」。守卫与 challenge_click.rs 的
            // `challenge_wait_result` 保持同一份口径：不可用的载荷一律当作「还没结论」，
            // 继续轮询（预算耗尽后按未解决返回）。
            let status = challenge_page_call(
                state,
                session_key,
                with_confirm_ms(json!({ "action": "status" }), confirm_ms),
                frame_id,
                left.min(STATUS_CALL_TIMEOUT),
            )
            .await
            .ok()
            .filter(challenge_status_usable);
            if let Some(status) = status {
                let token_ready = status
                    .get("token_ready")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let detected = status
                    .get("detected")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if token_ready || !detected {
                    return json!({
                        "solved": true,
                        "attempted": clicks > 0,
                        "clicks": clicks,
                        "via": if token_ready { "token" } else { "navigated" },
                        "evidence": accepted,
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "challenge": status,
                    });
                }
                // 「点击被接收」的证据必须建立在我们**确实点过**之上：页面自己进入
                // 「正在验证」时把它算成 accepted，会让求解器一次都不点就停手，
                // 然后耗完预算报 solved:false（P3）。
                if clicks > 0
                    && status.get("verifying").and_then(Value::as_bool) == Some(true)
                    && accepted.is_none()
                {
                    accepted = Some("verifying");
                }
                last_status = status;
            }
        }

        // frame 作用域下没有可信的主框架几何：继续只复查状态，等它自己通过。
        if frame_scoped {
            tokio::time::sleep(CHALLENGE_POLL).await;
            continue;
        }

        // ② 组件还在不在 + 几何（pierce DOM，穿 closed shadow root）。
        //    一次调用同时给出三件事：组件在不在、是不是换了一个、该点哪里。
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let (frames, mut frame_diag, frames_query_failed) =
            challenge_frames(state, session_key, left.min(FRAME_CALL_TIMEOUT)).await;
        if !took_frame_diag {
            took_frame_diag = true;
            diag.append(&mut frame_diag);
        }
        // 状态机（组件签名 / 替换 / 尺寸 / 确认窗口）始终以**面积最大**的那个
        // 组件为准：它是主挑战，页面刷新、被摘掉、进入 verifying 都先体现在它身上。
        // 点击目标则按 ⑥ 的轮转在前 3 个候选之间轮换。
        let Some(frame) = frames.first() else {
            // 查询失败 ≠ 组件被摘掉。CDP 附不上 / DOM 域不可用时会返回空集合，
            // 那不是「点击被接收」的证据，只是这一轮没拿到几何 —— 继续下一轮，
            // 绝不能据此设 accepted（否则求解器立刻停手，等一个永不到来的结论）。
            if frames_query_failed {
                if !noted_frames_query_failed {
                    noted_frames_query_failed = true;
                    diag.push(json!({ "step": "frames", "error": "frames_query_failed" }));
                }
                tokio::time::sleep(CHALLENGE_POLL).await;
                continue;
            }
            // 组件消失只有在「我们点过」之后才算点击被接收：一次没点就出现/消失
            // 是 CF 自己在刷新组件，当成证据会直接停手（P3）。
            if clicks > 0 && saw_widget && accepted.is_none() {
                accepted = Some("widget-removed");
            }
            tokio::time::sleep(CHALLENGE_POLL).await;
            continue;
        };
        saw_widget = true;
        let signature = frame.url.clone();
        let replaced = widget_url.as_ref().is_some_and(|prev| prev != &signature);
        if replaced {
            // 组件被换成新的实例。两种可能，必须分开处理：
            //   - 我们刚点过 → 上一次点击被接收了（Turnstile 重新起了个挑战）；
            //   - 没点过 → CF 自己刷新了组件（常见于超时重发），继续按新组件处理。
            // 把「用户没动过」也算成 accepted，会让求解器一次都不点就干等到超时。
            // 只有「我们已经点过」之后的替换才算点击被接收：那种替换是 Turnstile
            // 收到点击后重开一轮验证。还没点过就发生的替换是 CF 自己刷新组件，
            // 必须继续按新组件处理，否则会一次都不点干等到超时。
            // 一旦认定被接收就彻底停止点击 —— 每次再点都会让组件重新生成，
            // 表现就是「碰一下元素就变」。
            if clicks > 0 && accepted.is_none() {
                accepted = Some("widget-replaced");
            }
            last_click_at = None;
        }
        widget_url = Some(signature);

        // ③ 已确认点击被接收：只等结论，绝不再点。
        if accepted.is_some() {
            tokio::time::sleep(CHALLENGE_POLL).await;
            continue;
        }

        // ④ 尺寸到位才能点：组件刚插入时是 0×0。
        if frame.width < 100.0 || frame.height < 24.0 {
            tokio::time::sleep(CHALLENGE_POLL).await;
            continue;
        }

        // ⑤ 刚点过、还在确认窗口内：继续观察，不补点。
        if last_click_at.is_some_and(|at| at.elapsed() < CLICK_ACK_WINDOW) {
            tokio::time::sleep(CHALLENGE_POLL).await;
            continue;
        }
        if attempts >= MAX_CLICKS {
            accepted = Some("click-limit");
            tokio::time::sleep(CHALLENGE_POLL).await;
            continue;
        }

        // ⑥ 点。鼠标轨迹也要受剩余预算约束，否则一次点击就能把 --timeout 顶破。
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        // 剩余预算不够走完一次「滑入 → 按下 → 抬起」（4×35ms + 80ms 的节奏），
        // 就别起这个头：半截的点击拿不到结论，只会把最后一点预算烧掉。
        if left < Duration::from_millis(300) {
            break;
        }
        let candidates: Vec<&ChallengeFrame> = frames.iter().take(MAX_FRAME_CANDIDATES).collect();
        let Some(target) = candidates.get(frame_cursor % candidates.len()).copied() else {
            tokio::time::sleep(CHALLENGE_POLL).await;
            continue;
        };
        // 轮转：同一个组件在一轮内只点一次，点过就换下一个；单组件时等于重试自己。
        frame_cursor = frame_cursor.wrapping_add(1);
        attempts += 1;
        let (lx, ly) = challenge_click_point(target);
        match challenge_click_widget(
            state,
            session_key,
            target,
            lx,
            ly,
            left.min(FRAME_CALL_TIMEOUT),
        )
        .await
        {
            Ok(()) => {
                clicks += 1;
                last_click_at = Some(Instant::now());
                let label: String = target.url.chars().take(90).collect();
                if !clicked_frames.contains(&label) {
                    clicked_frames.push(label.clone());
                }
                if tried.len() < 12 {
                    let (sx, sy) = target.to_screen(lx, ly);
                    tried.push(json!({
                        "frame": label,
                        "x": sx.round(),
                        "y": sy.round(),
                    }));
                }
            }
            Err(err) => {
                // 这个组件连事件都派发不出去（目标已销毁 / CDP 抖动）：记下来，
                // 下一轮换下一个候选，不要用同一个坏组件把整段预算耗完。
                frame_attempts.push(json!({
                    "step": "click-frame",
                    "frame": target.url.chars().take(90).collect::<String>(),
                    "error": humanize_extension_error(&err.to_string()),
                }));
            }
        }
        tokio::time::sleep(CHALLENGE_POLL).await;
    }

    // 所有候选都没能推进时，把每个组件的失败原因交回去。只说
    // 「点了没反应」的话，调用方无从知道是哪个组件、卡在哪一步。
    if accepted.is_none() && saw_widget {
        for label in &clicked_frames {
            let recorded = frame_attempts
                .iter()
                .any(|item| item.get("frame").and_then(Value::as_str) == Some(label.as_str()));
            if !recorded {
                frame_attempts.push(json!({
                    "step": "click-frame",
                    "frame": label,
                    "error": "no-effect",
                }));
            }
        }
    }
    if !frame_attempts.is_empty() {
        diag.append(&mut frame_attempts);
    }

    json!({
        "solved": false,
        "attempted": clicks > 0,
        "reason": if frame_scoped {
            "frame-scope"
        } else if accepted == Some("click-limit") {
            "click-limit"
        } else if accepted.is_some() {
            "verifying_not_completed"
        } else if saw_widget {
            "click-no-effect"
        } else {
            "no-widget-frame"
        },
        "clicks": clicks,
        "tried": tried,
        "evidence": accepted,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "challenge": last_status,
        "diag": diag,
        "hint": if frame_scoped {
            "该挑战在 iframe 内（--frame）：当前实现只在主框架做几何定位与点击，无法保证点到的是同一个元素，因此没有自动点击。请人工处理，或去掉 --frame 后用 `challenge widget` 取主文档坐标"
        } else if accepted.is_some() {
            "点击已被 Turnstile 接收（组件被摘掉/替换，或页面进入「正在验证」），但 CF 一直没给出 token 或跳转：这是服务端判定，客户端点多少次都一样。可先换出口 IP / 稍后重试，或用 `challenge widget` 看组件当前状态"
        } else if saw_widget {
            "组件在、也点过，但页面完全没有反应：确认窗口是否真的可见（--challenge focus），或组件已被站点改成需要滑块等交互"
        } else {
            "整个预算内都没出现验证码组件：确认 CDP 可用（别的扩展注入的 iframe 会挡住 debugger），或站点只下发纯 JS 挑战"
        },
    })
}

pub(crate) async fn challenge_page(
    State(state): State<AppState>,
    Json(req): Json<ChallengeRequest>,
) -> Json<Value> {
    touch(&state).await;
    // 用「跟随内层 ok」的包装：超时/未通过时外层也是 ok:false。
    Json(json_result_with_inner_ok(
        challenge_value(&state, req).await,
    ))
}

/// 挑战页 / 验证码：识别、等待、需要人时转人工。
///
/// `wait` 在页面里用 MutationObserver 等（一次注入一片，默认 4 秒一片），
/// 通过挑战后页面通常会跳转、注入上下文被销毁——那不是失败，重新注入再确认即可。
pub(crate) async fn challenge_value(state: &AppState, req: ChallengeRequest) -> Result<Value> {
    let action = req
        .action
        .clone()
        .unwrap_or_else(|| "status".to_string())
        .to_lowercase();
    if !matches!(
        action.as_str(),
        "status" | "detect" | "wait" | "solve" | "focus" | "auto" | "click" | "widget"
    ) {
        return Err(anyhow!(
            "challenge: 未知 action {action}（可选 status / wait / solve / auto / click / widget / focus）"
        ));
    }
    let session_key = select_tab(
        state,
        SessionSelector::new(
            req.switch_tab_id.clone(),
            req.browser.clone(),
            req.profile.clone(),
        ),
    )
    .await?;

    if action == "focus" {
        let reply = extension_control(
            state,
            &session_key,
            "tabs",
            json!({ "method": "switch", "allowFocus": req.focus.unwrap_or(true) }),
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await?;
        return Ok(json!({
            "tab_id": tab_id_of_session(&session_key),
            "session_key": session_key,
            "action": "focus",
            "ok": true,
            "focused": true,
            "detail": reply,
        }));
    }

    // 只想看「组件在哪、该点哪个坐标」：给智能体自己点的兜底入口。
    // 返回的所有数据都来自元素（CDP 的 pierce DOM / OOPIF 目标），不含任何猜测。
    if action == "widget" {
        let timeout = Duration::from_secs(6);
        let (frames, diag, frames_query_failed) =
            challenge_frames(state, &session_key, timeout).await;
        let present = !frames.is_empty();
        let widgets: Vec<Value> = frames
            .iter()
            .map(|frame| {
                let (lx, ly) = challenge_click_point(frame);
                let (sx, sy) = frame.to_screen(lx, ly);
                json!({
                    "url": frame.url.chars().take(160).collect::<String>(),
                    "rect": {
                        "x": frame.quad[0].round(),
                        "y": frame.quad[1].round(),
                        "w": frame.width.round(),
                        "h": frame.height.round(),
                    },
                    "click": {
                        "x": sx.round(),
                        "y": sy.round(),
                        "local": { "x": lx.round(), "y": ly.round() },
                    },
                })
            })
            .collect();
        // 这里**不**调 challenge status：那会往页面 MAIN world 注入一段脚本，
        // 在被 CF 全程监控的挑战页上等于自报身份。探测只走 CDP。
        //
        // `--frame` 下这套几何探测仍然只看主文档：它给出的坐标是主框架元素的位置，
        // 与调用方指定的子框架无关。必须说清楚，否则调用方拿着这些坐标点击就可能
        // 打到同位置的另一个元素（P3）。
        let frame_note = req.frame_id.is_some();
        return Ok(json!({
            "tab_id": tab_id_of_session(&session_key),
            "session_key": session_key,
            "action": "widget",
            "ok": !widgets.is_empty(),
            "found": !widgets.is_empty(),
            "count": widgets.len(),
            "present": present,
            // 查询本身失败时不能读成「页面上没有组件」。带独立标记 + error_code，
            // 调用方据此知道该修 CDP 通道，而不是判「组件已消失」。
            "frames_query_failed": frames_query_failed,
            "error_code": if frames_query_failed { json!("frames_query_failed") } else { Value::Null },
            "frame_id": req.frame_id,
            "frame_scope": "main-document",
            "widgets": widgets,
            "diag": diag,
            "next": if frames_query_failed {
                "组件查询失败（不是「页面没有组件」）：CDP 没附上或 DOM 域不可用，先看 diag；常见原因是页面内嵌了其它扩展的 iframe 挡住了 debugger"
            } else if widgets.is_empty() {
                "没找到组件：先用 `challenge status` 确认是不是挑战页；若 turnstile.shadow_widget 为 true 但这里为空，说明 CDP 没附加成功（别的扩展注入的 iframe 会挡住 debugger）"
            } else if frame_note {
                "用 `atb challenge click --at <click.x>,<click.y>` 点一次，再用 `atb challenge status` 复查；**注意**：--frame 下这些坐标仍取自主文档，可能落到同位置的另一个元素，优先人工处理"
            } else {
                "用 `atb challenge click --at <click.x>,<click.y>` 点一次，再用 `atb challenge status` 复查"
            },
        }));
    }

    // 一次性坐标点击：自动求解失败后，调用方拿着 `challenge widget` 给的坐标自己点。
    if let Some([x, y]) = req.point {
        // C1-06：这条路径以前把「点一次 + 复查一次」各自写死 8s，完全忽略 --timeout：
        // 扩展挂死时一次坐标点击能耗 16s。现在按调用方给的超时两等分（点一次 / 查一次），
        // 每次不小于 1s（低于一次 CDP 往返的实用下限）；没传 --timeout 时保持旧行为。
        let per_call = match req
            .timeout
            .filter(|value| value.is_finite() && *value > 0.0)
        {
            // 上限 300s：与 challenge 命令自己的 --timeout 量级一致，
            // 也避免异常大的值把 Duration::from_secs_f64 顶到溢出。
            Some(total) => Duration::from_secs_f64((total / 2.0).clamp(1.0, 300.0)),
            None => Duration::from_secs(8),
        };
        let clicked = challenge_click_at(state, &session_key, x, y, per_call).await;
        // 取证失败时不能给 `cleared:true`：`Value::Null` 上 token_ready 与 detected
        // 都是 false，旧的 `token_ready || !detected` 恒为真，于是「点了 + 探测不到」
        // 被说成「挑战已通过」（机制 B）。这里把探测结果单独收起来。
        let (status, status_error, probed) = match challenge_page_call(
            state,
            &session_key,
            with_confirm_ms(json!({ "action": "status" }), req.confirm_ms),
            req.frame_id,
            per_call,
        )
        .await
        {
            Ok(status) if challenge_status_usable(&status) => (status, Value::Null, true),
            Ok(_) => (
                Value::Null,
                json!("页面 status 返回空载荷（扩展未注入或页面脚本未执行）"),
                false,
            ),
            Err(err) => (
                Value::Null,
                json!(humanize_extension_error(&err.to_string())),
                false,
            ),
        };
        let token_ready = status
            .get("token_ready")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let detected = status
            .get("detected")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return Ok(json!({
            "tab_id": tab_id_of_session(&session_key),
            "session_key": session_key,
            "action": "click",
            "ok": clicked.is_ok(),
            "at": { "x": x, "y": y },
            "clicked": clicked.is_ok(),
            // 没有探测结论就没有 cleared：宁可说「不知道」，也不能说「已通过」。
            "cleared": probed && (token_ready || !detected),
            "blocking": !probed,
            "probed": probed,
            "status_error": status_error,
            "token_ready": token_ready,
            "detected": detected,
            "challenge": status,
            "error": clicked.err().map(|err| humanize_extension_error(&err.to_string())),
        }));
    }

    // open 返回时新标签可能还停在 about:blank 或上一个文档上；
    // 先等它落到真实页面，否则会把「空白页」误判成「没有验证码」。
    let _ = wait_for_js_condition_with_target(
        state,
        Some(&session_key),
        "return location.href !== 'about:blank' && document.readyState !== 'loading';",
        2.5,
        0.1,
        None,
        &ExecTargetOptions {
            frame_id: req.frame_id,
            ..Default::default()
        },
    )
    .await;

    if action == "status" || action == "detect" {
        let challenge = challenge_page_call(
            state,
            &session_key,
            with_confirm_ms(json!({ "action": "status" }), req.confirm_ms),
            req.frame_id,
            Duration::from_secs(10),
        )
        .await?;
        return Ok(json!({
            "tab_id": tab_id_of_session(&session_key),
            "session_key": session_key,
            "action": "status",
            "challenge": challenge,
        }));
    }

    // wait / solve / auto / click
    // auto 与 click 是「只做主动求解、结果立刻返回」的一次性动作，供上层判断用；
    // solve 是「主动求解 + 被动等待 + 转人工」的接力。
    let one_shot = action == "auto" || action == "click";
    let is_solve = action == "solve" || one_shot;
    let timeout = req
        .timeout
        .unwrap_or(if action == "solve" { 120.0 } else { 30.0 })
        .clamp(0.5, 600.0);
    let focus_after = req.focus_after.unwrap_or(8.0).max(0.0);
    // 页面侧的计时基准是「本次注入的起点」，而单片最多只等 4s：
    // 固定下发 8000ms 时 `at - started` 永远到不了，`need_human` 从不返回，
    // 需要人工的验证码页会一路静默等到总超时。所以要下发「还差多久」，每片递减。
    // focus_after <= 0 仍然下发 0（页面侧 0 = 关闭聚焦，保持旧语义）。
    let focus_after_ms = if focus_after > 0.0 {
        (focus_after * 1000.0).round() as u64
    } else {
        0
    };
    let interval_ms = (req.interval.unwrap_or(0.25).clamp(0.05, 5.0) * 1000.0) as u64;
    let deadline = Instant::now() + Duration::from_millis((timeout * 1000.0) as u64);
    let started = Instant::now();
    let mut injections: u64 = 0;
    let mut focused = false;
    let mut last = Value::Null;
    let reload_after = req.reload_after.unwrap_or(0.0).max(0.0);
    let mut reloaded = false;
    // reload 失败的原因（C1-04）：与 `reloaded` 一起交回去，别让「没重载成功」
    // 沉默地变成一个纯超时。
    let mut reload_error: Option<String> = None;

    // 进循环前先看一眼现场（wait 也看）：
    //   - 没挑战 / 已经有 token → 直接返回，不用进循环；
    //   - CF 硬拦截（"Sorry, you have been blocked"）→ 点也点不了、等也不放行，
    //     立刻回结论，否则 wait 会老老实实把 --timeout 全耗完再报超时，用户拿到的
    //     信息还不如「被判定为风险」这一句有用。
    // 探测失败必须与「页面没有挑战」区分开。`challenge_page_call` 的 eval 兜底在
    // 页面脚本没有返回值时给 `Value::Null`，Null 上 blocking / interactive /
    // token_ready 全按 false 处理 ⇒ `!blocking && (token_ready || !interactive)`
    // 恒成立 ⇒ 直接返回 passed:true / via:"no-challenge"。扩展失联 + 页面禁 eval
    // 这种最需要人工介入的场景反而被告知「挑战已通过」（机制 C）。
    let mut probe_error: Option<String> = None;
    {
        match challenge_page_call(
            state,
            &session_key,
            with_confirm_ms(json!({ "action": "status" }), req.confirm_ms),
            req.frame_id,
            Duration::from_secs(10),
        )
        .await
        {
            Ok(status) if challenge_status_usable(&status) => {
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
                let blocked = status.get("kind").and_then(Value::as_str) == Some("blocked");
                // 文档还在加载时必须重探，不能给结论：CF 的 "Just a moment" 在
                // `readyState==='interactive'` 阶段常常还没建出 `#challenge-stage` /
                // turnstile iframe，此刻 `detected:false, blocking:false,
                // interactive:false` 是**假阴性**。旧实现据此直接返回
                // `passed:true, via:"no-challenge"`，等于对一张正在变成拦截页的
                // 文档说「没有挑战、已通过」。pending 时跳过所有早退，
                // 交给下面的 auto_solve / 等待循环继续观察。
                let pending = status
                    .get("pending")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                // 「没有挑战」必须先有探测结论：status 里 detected / blocking 是页面
                // 脚本给出的，缺了它们就说明这一趟根本没问出东西。
                if is_solve && !pending && !blocking && (token_ready || !interactive) {
                    return Ok(json!({
                        "tab_id": tab_id_of_session(&session_key),
                        "session_key": session_key,
                        // 回显调用方真正用的 action：auto/solve 混在一起会让调用方误判路径。
                        "action": action.clone(),
                        "ok": true,
                        "passed": true,
                        "via": if token_ready { "token" } else { "no-challenge" },
                        "focused": focused,
                        "injections": 0,
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "challenge": status,
                    }));
                }
                if blocked {
                    return Ok(json!({
                        "tab_id": tab_id_of_session(&session_key),
                        "session_key": session_key,
                        "action": action.clone(),
                        "ok": false,
                        "passed": false,
                        "cleared": false,
                        "blocked": true,
                        "via": "hard-block",
                        "focused": focused,
                        "injections": 0,
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "error": "站点返回 Cloudflare 硬拦截页：当前出口 IP / 环境被判为风险，等待与点击都不会放行",
                        "error_code": "challenge_blocked",
                        "challenge": status,
                    }));
                }
                last = status;
            }
            Ok(_) => {
                probe_error =
                    Some("页面 status 返回空载荷（扩展未注入 / 页面脚本未执行）".to_string());
            }
            Err(err) => {
                probe_error = Some(humanize_extension_error(&err.to_string()));
            }
        }
    }

    // 主动求解：Turnstile 勾选框这类「必须真点」的挑战，光等永远不会过。
    // 预算取总超时的一半（上限 30s），剩下的时间留给被动等待兜底——
    // 有些站点点完之后还要等它自己跳转。
    // 主动求解的结论要一直带着：后面接被动等待时，超时结果里也要能回答
    // 「到底点没点、点到哪一步」。
    // 预算必须从**真正的 deadline** 算：写死 timeout/2 再让 auto_solve 用自己的
    // 计时器，等于把外层 deadline 推后到 auto_solve 返回之后才生效（P3）。
    let mut solve_summary = Value::Null;
    if is_solve {
        // 探测不到状态时，solve/auto/click 一律不给 passed:true：没有证据就没有结论，
        // 把原因交回去让人处理，比一句「已通过」有用得多。
        if let Some(reason) = probe_error.clone() {
            return Ok(json!({
                "tab_id": tab_id_of_session(&session_key),
                "session_key": session_key,
                "action": action.clone(),
                "ok": false,
                "passed": false,
                "cleared": false,
                // 没探测成功 = 不能认为放行，与「已确认仍在拦截」同一个安全侧。
                "blocking": true,
                "via": "probe-failed",
                "focused": focused,
                "injections": 0,
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "status_error": reason,
                "error_code": "challenge_probe_failed",
                "error": "无法确认挑战状态（扩展未连接或页面禁止 eval）：自动判定不可信，请人工确认页面",
                "challenge": last,
            }));
        }
        let remaining_total = deadline.saturating_duration_since(Instant::now());
        let budget = if one_shot {
            remaining_total
        } else {
            (remaining_total / 2).min(Duration::from_secs(30))
        };
        let solved =
            challenge_auto_solve(state, &session_key, budget, req.frame_id, req.confirm_ms).await;
        solve_summary = solved.clone();
        let passed = solved
            .get("solved")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if passed || one_shot {
            return Ok(json!({
                "tab_id": tab_id_of_session(&session_key),
                "session_key": session_key,
                "action": if one_shot { action.clone() } else { "solve".to_string() },
                "ok": passed,
                "passed": passed,
                "cleared": passed,
                "via": if passed {
                    solved.get("via").cloned().unwrap_or_else(|| json!("click"))
                } else {
                    json!("none")
                },
                "focused": false,
                "injections": 0,
                "clicks": solved.get("clicks").cloned().unwrap_or_else(|| json!(0)),
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "auto_solve": solved,
            }));
        }
    }

    // 「已经拦了多久」只能由 daemon 记：页面脚本刻意不写 `window.*`（那会留下自动化
    // 指纹，实测注入过之后 CF 会卡在「正在验证」），所以 `stalledMs` 必须由这里下发。
    // 旧实现从不传，页面侧 `stalled` 恒 false，`turnstile_not_rendered` 的快速失败
    // 与归因整条链路都是死代码，`--challenge-timeout` 只能被完整耗光。
    let mut first_blocking_at: Option<Instant> = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        // 只用 deadline 判终止：每片至少 1ms、最多 4s，剩余时间必然收敛。
        // 旧实现写死 `injections >= 80`（= 最多 320s），把 --timeout 600 静默截断成 320s，
        // 而且超时后既不说明原因也不报错。injections 现在只做诊断计数。
        if remaining.is_zero() {
            break;
        }
        let slice_ms = (remaining.as_millis() as u64).clamp(1, 4_000);
        injections += 1;
        let opts = with_confirm_ms(
            json!({
                "action": "wait",
                "timeoutMs": slice_ms,
                "intervalMs": interval_ms,
                // solve 还要等「验证码通过令牌」，wait 只看页面是否还被拦。
                "waitToken": is_solve,
                // 首次判定为拦截/有组件至今的毫秒数（见上面的 `first_blocking_at`）。
                "stalledMs": first_blocking_at
                    .map(|at| at.elapsed().as_millis() as u64)
                    .unwrap_or(0),
                // 已经聚焦过就不再要求「转人工」，否则会反复返回。
                // 递减到 0 时补 1ms：0 在页面侧是「禁用」而不是「立刻」，
                // 这一片必须真的把 need_human 递上来。
                "focusAfterMs": if focused || focus_after_ms == 0 {
                    0
                } else {
                    focus_after_ms
                        .saturating_sub(started.elapsed().as_millis() as u64)
                        .max(1)
                },
            }),
            // 确认窗口由调用方决定；None 不下发，页面沿用默认 250ms。
            req.confirm_ms,
        );
        match challenge_page_call(
            state,
            &session_key,
            opts,
            req.frame_id,
            Duration::from_millis(slice_ms + 4_000),
        )
        .await
        {
            Ok(payload) => {
                // 维护「从什么时候开始被拦」，供下一轮 `stalledMs` 使用。
                // 清除条件用 `!detected`：只靠 `!blocking` 会把「组件还在但能浏览」
                // 当成已恢复，时间戳被反复重置，stalled 永远攒不够。
                let flagged =
                    |key: &str| payload.get(key).and_then(Value::as_bool).unwrap_or(false);
                if flagged("blocking") || flagged("interactive") {
                    first_blocking_at.get_or_insert_with(Instant::now);
                } else if !flagged("detected") {
                    first_blocking_at = None;
                }
                if payload.get("cleared").and_then(Value::as_bool) == Some(true) {
                    let via = payload
                        .get("via")
                        .and_then(Value::as_str)
                        .unwrap_or("clear");
                    let mut result = json!({
                        "tab_id": tab_id_of_session(&session_key),
                        "session_key": session_key,
                        "action": if is_solve { "solve" } else { "wait" },
                        "ok": true,
                        "cleared": true,
                        "focused": focused,
                        "injections": injections,
                        "reloaded": reloaded,
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "challenge": payload,
                    });
                    // `passed` 的语义统一成「目标已达成」：solve 的目标是拿到 token，
                    // wait 的目标是离开挑战页。旧实现只在 `is_solve` 时写 `passed`，
                    // 而 CLI 对 `action=="wait"` 只读 `/result/passed`、不回落
                    // `/result/cleared`，于是**成功清除被判成 challenge_timeout**
                    // （退出码 1）——功能是好的，报告说它坏了。
                    result["passed"] = json!(true);
                    result["via"] = json!(via);
                    if let Some(reason) = &reload_error {
                        // 重载失败但挑战后来还是过了：如实带上，便于排查「为什么慢」。
                        result["reload_error"] = json!(reason);
                    }
                    return Ok(result);
                }
                let need_human = payload.get("need_human").and_then(Value::as_bool) == Some(true);
                last = payload;
                if need_human && !focused && req.focus != Some(false) {
                    // 超时前没自动过：把标签页带到前台，人处理完继续等。
                    let _ = extension_control(
                        state,
                        &session_key,
                        "tabs",
                        json!({ "method": "switch", "allowFocus": true }),
                        DEFAULT_EXTENSION_CONTROL_TIMEOUT,
                    )
                    .await;
                    focused = true;
                    continue;
                }
                // 卡住时先整页 reload 一次：挑战 cookie 可能已经下发，页面脚本没走完。
                if !reloaded
                    && reload_error.is_none()
                    && reload_after > 0.0
                    && started.elapsed().as_secs_f64() >= reload_after
                {
                    // C1-04：`reloaded` 必须在**调用成功之后**才置位。旧实现先置 true
                    // 再 `let _ =` 丢掉错误：reload 失败（CDP 不可用 / 页面禁止 eval）
                    // 时输出里照样是 `reloaded:true`，调用方会以为「已经重载过一次，
                    // 还是没通过」，据此下「这站过不去」的错误结论。
                    match execute_page_js_with_target(
                        state,
                        "setTimeout(() => location.reload(), 50); return 'reload'",
                        SessionSelector::new(Some(session_key.clone()), None, None),
                        true,
                        Duration::from_secs(5),
                        &ExecTargetOptions {
                            frame_id: req.frame_id,
                            ..Default::default()
                        },
                    )
                    .await
                    {
                        Ok(_) => reloaded = true,
                        Err(err) => {
                            // 失败后也不再重试：每片都试一次 reload 只会把预算耗在
                            // 同一个必然失败的调用上。原因写进结果里的 `reload_error`。
                            reload_error = Some(humanize_extension_error(&err.to_string()));
                        }
                    }
                }
                // 这一片等满 / 仍在拦截：只要还没到总超时就再等一片
                continue;
            }
            Err(err) => {
                // 通过挑战后页面会跳转、执行上下文被销毁——这恰恰是「清了」的信号，
                // 重新注入确认一次，而不是直接判失败。
                last = json!({ "error": humanize_extension_error(&err.to_string()) });
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                tokio::time::sleep(remaining.min(Duration::from_millis(150))).await;
            }
        }
    }

    // 超时 = 失败：以前这里返回 ok:true + cleared:false，CLI 会以退出码 0 收场，
    // 智能体以为「等待成功」直接进入下一步。status/detect 分支的 ok 不动（识别本身成功）。
    let mut result = json!({
        "tab_id": tab_id_of_session(&session_key),
        "session_key": session_key,
        "action": if is_solve { "solve" } else { "wait" },
        "ok": false,
        "cleared": false,
        "error_code": "challenge_timeout",
        "error": format!(
            "challenge 在 {timeout}s 内未清除（injections={injections}）；可增大 --timeout，或人工完成验证码后重试"
        ),
        "focused": focused,
        "injections": injections,
        "reloaded": reloaded,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "challenge": last,
    });
    // 进循环前那次探测失败时，超时结论里必须带上原因：否则调用方只看到
    // 「等了 N 秒没清」，无从判断是挑战没过还是根本探测不到。
    if let Some(reason) = probe_error {
        result["status_error"] = json!(reason);
        result["blocking"] = json!(true);
    }
    // C1-04：reload 失败要能被看见（`reloaded` 此时恒为 false）。
    if let Some(reason) = reload_error {
        result["reload_error"] = json!(reason);
    }
    if is_solve {
        // 没等到：把球交回给调用方，不要伪装成通过。
        result["passed"] = json!(false);
        result["reason"] = json!("timeout");
        if !solve_summary.is_null() {
            result["auto_solve"] = solve_summary;
        }
    }
    Ok(result)
}

/// 自动化痕迹审计 / 自查清理。
///
/// 与其它页面脚本不同，这份脚本必须在 **MAIN world** 执行：站点能读到的只有页面的
/// window，ISOLATED world 里的同名变量站点根本看不到，在那里审计等于自欺欺人。
/// eval 通道天然是 MAIN world；CSP 页面走扩展时由 background.js 的 PAGE_OP_WORLD 指定。
pub(crate) async fn stealth_page_call(
    state: &AppState,
    session_key: &str,
    opts: Value,
    frame_id: Option<i64>,
    timeout: Duration,
) -> Result<Value> {
    let script = format!(
        "{}\nreturn __abcStealth({});",
        html::js_page_stealth(),
        opts
    );
    let target = ExecTargetOptions {
        frame_id,
        ..Default::default()
    };
    let executed = execute_page_js_with_target(
        state,
        &script,
        SessionSelector::new(Some(session_key.to_string()), None, None),
        true,
        timeout,
        &target,
    )
    .await;
    match executed {
        Ok(result) => Ok(result.get("js_return").cloned().unwrap_or(Value::Null)),
        Err(err) if is_csp_error(&err.to_string()) => {
            page_op_via_extension(state, session_key, "stealth", opts, frame_id, timeout).await
        }
        Err(err) => Err(err),
    }
}

/// 判断「扩展通道根本不可用」，用来决定要不要退回 MAIN world 的 eval。
///
/// 只认两类：扩展不认识这个 op（版本过旧），或连接已经断了。除此之外的错误
/// （exec_timeout、上下文销毁、页面脚本自身报错）都应该如实返回或重试——
/// 退到 MAIN world 会留下站点可读的注入痕迹。
fn is_extension_channel_unavailable(message: &str) -> bool {
    const MARKERS: &[&str] = &[
        "unknown cmd",
        "unknown pageOp",
        "浏览器扩展连接已断开",
        "extension_disconnected",
        "没有可用的扩展连接",
        "扩展未连接",
    ];
    MARKERS.iter().any(|marker| message.contains(marker))
}

pub(crate) async fn challenge_page_call(
    state: &AppState,
    session_key: &str,
    opts: Value,
    frame_id: Option<i64>,
    timeout: Duration,
) -> Result<Value> {
    // **优先走扩展的 ISOLATED world**。
    //
    // 挑战页是 CF 全程监控的页面：往它的 MAIN world 注入脚本（哪怕只定义一个函数、
    // 只写一个 window 变量）都会留下站点可读的痕迹。实测同一个点击坐标、同一台机器：
    // 不注入 → 1.4s 通过；注入过（`window.__abcChlSeenAt`）→ 页面进「正在验证」后卡死。
    // 扩展的 ISOLATED world 能读同一份 DOM，但站点看不到里面的一切。
    match page_op_via_extension(
        state,
        session_key,
        "challenge",
        opts.clone(),
        frame_id,
        timeout,
    )
    .await
    {
        Ok(payload) => return Ok(payload),
        // 只有「扩展根本不支持这个 op / 连不上」才退回 eval。以前是**任何** Err 都退，
        // 于是 exec_timeout（挑战页导航期间是常态）、执行上下文被销毁、扩展抖动
        // 全都会把整轮等待变成 MAIN world 注入 —— 而注入痕迹站点读得到，实测会让
        // CF 卡在「正在验证」再也放行不了。超时应当重试扩展通道，
        // 不该换一条更危险的通道。
        Err(err) if is_extension_channel_unavailable(&err.to_string()) => {
            eprintln!("challenge: 扩展通道不可用（{err}），退回 eval 读取挑战状态");
        }
        Err(err) => return Err(err),
    }
    // 扩展通道不可用（版本过旧 / 未连接）才退回 eval。
    let script = format!(
        "{}\nreturn __abcChallenge({});",
        html::js_page_challenge(),
        opts
    );
    let target = ExecTargetOptions {
        frame_id,
        ..Default::default()
    };
    let executed = execute_page_js_with_target(
        state,
        &script,
        SessionSelector::new(Some(session_key.to_string()), None, None),
        true,
        timeout,
        &target,
    )
    .await;
    match executed {
        Ok(result) => Ok(result.get("js_return").cloned().unwrap_or(Value::Null)),
        // 固定脚本、不走 eval；CSP 页面走扩展注入的同一份脚本。
        Err(err) if is_csp_error(&err.to_string()) => {
            page_op_via_extension(state, session_key, "challenge", opts, frame_id, timeout).await
        }
        Err(err) => Err(err),
    }
}
