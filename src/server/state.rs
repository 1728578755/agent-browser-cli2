use super::*;
use std::io::Write as _;

pub(crate) const HOST: &str = "127.0.0.1";
pub(crate) const API_PORT: u16 = 18767;
pub(crate) const LOOPBACK_API_ORIGIN: &str = "http://127.0.0.1:18767";
pub(crate) const LOCALHOST_API_ORIGIN: &str = "http://localhost:18767";
pub(crate) const CHROME_EXTENSION_ORIGIN_PREFIX: &str = "chrome-extension://";
// daemon 在无 CLI/API 业务请求后自动退出，避免浏览器扩展长期保持“已连接”浮层。
// 这是默认值，可用 ATB_IDLE_TTL（秒，旧名 AGENT_BROWSER_CLI_IDLE_TTL）覆盖，见 `idle_shutdown_ttl_from_env`。
pub(crate) const IDLE_SHUTDOWN_TTL: Duration = Duration::from_secs(300);
pub(crate) const IDLE_SHUTDOWN_CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// 断开的会话保留一段时间（便于短暂重连复用），超过后连同 snapshot 缓存一起清理。
pub(crate) const STALE_SESSION_TTL: Duration = Duration::from_secs(600);
/// `wait_for_sessions` 的只读档：浏览器没开时，`tabs`/`lookup` 不值得白等 5 秒。
pub(crate) const WAIT_SESSIONS_READ: Duration = Duration::from_millis(800);
/// `wait_for_sessions` 的动作档：用户可能刚点开浏览器，动作命令要多等一会儿。
pub(crate) const WAIT_SESSIONS_ACTION: Duration = Duration::from_secs(5);
/// `acked` 表的兜底上限。加了「只有 pending 里存在才登记」之后理论上不会超，
/// 这里只是防止异常输入把内存吃掉。
pub(crate) const ACK_TRACK_LIMIT: usize = 4096;
/// 「已请求过扩展重载」登记表的上限：键是 `browser_id:version`，旧版本不会回流。
pub(crate) const EXT_RELOAD_TRACK_LIMIT: usize = 256;
/// POST 幂等回放缓存的保留时间：只覆盖「响应丢失后立刻重发」的窗口。
pub(crate) const IDEMPOTENCY_TTL: Duration = Duration::from_secs(60);
/// 幂等回放缓存的兜底上限：防异常客户端用随机 key 灌爆内存，超出淘汰最旧一项。
pub(crate) const IDEMPOTENCY_CACHE_LIMIT: usize = 512;
/// 同一幂等键的并发重发最多等多久。
///
/// 超过这个时间说明首个请求要么卡住要么已经异常消失：调用方必须自己决定
/// 「继续等」还是「换个检查方式」，绝不能在等待后静默重放一次未完成的执行。
pub(crate) const IDEMPOTENCY_INFLIGHT_WAIT: Duration = Duration::from_secs(90);
/// 幂等中间件愿意读进内存的请求体上限。
///
/// 超过它 / 拿不到 `Content-Length`（流式上传）时**不读体**，直接把请求原样放行
/// 且不缓存：能执行，只是失去「响应丢失后回放」的保护。
pub(crate) const IDEMPOTENCY_BODY_LIMIT: usize = 4 * 1024 * 1024;

/// 幂等缓存里的一项。
///
/// 旧实现只有 `(Instant, Value)`：同一 key 的第二个请求在首个请求**还在执行**时
/// 查不到任何东西，于是又执行一遍——幂等键在最需要它的窗口（并发重发 / CLI 超时后
/// 立刻重试）完全失效。现在用「占位 → 完成」两态：
/// - `Pending` 表示首个请求正在执行，后来者必须等它的通知；
/// - `Done` 表示已有可回放的响应。
///
/// 注意：`Pending` 不参与 TTL 过期。执行中的动作最长可以跑 600s，若按 60s 把它
/// 清掉，第 61 秒的重发会被当成首个请求再执行一次——正是要防的双击。
pub(crate) enum IdempotencyEntry {
    /// 首个请求正在执行；等待者订阅这个 `Notify`（完成时 `notify_waiters`）。
    Pending(Arc<Notify>),
    /// 已完成：(登记时刻, 可回放的响应 JSON)。
    Done(Instant, Value),
}

/// 一条幂等缓存项：指纹 + 状态。
///
/// 指纹（`agent|method|path|query|body_hash`）与状态放在一起，而不是拼进表键：
/// 表键只用 `{agent}|{用户 key}`，换了请求体的同 key 才能命中同一条记录并判成 409。
pub(crate) struct IdempotencySlot {
    pub(crate) scope: String,
    pub(crate) entry: IdempotencyEntry,
}

impl IdempotencyEntry {
    /// 是否还值得回放。`Pending` 永不过期（见类型注释）。
    fn is_fresh(&self, now: Instant) -> bool {
        match self {
            IdempotencyEntry::Pending(_) => true,
            IdempotencyEntry::Done(at, _) => now.duration_since(*at) < IDEMPOTENCY_TTL,
        }
    }
}

/// 一条已建立的扩展连接。`revoked` 是给 socket 循环轮询的撤销信号：
/// `/reload-trust` 置位后，连接会在下一次轮询（≤500ms）里收到 Close 并断开。
pub(crate) struct WsConn {
    pub(crate) extension_id: String,
    /// 握手时扩展在 `?token=` 里提供的值的指纹（没带就是 `None`）。
    ///
    /// 存指纹而不是明文：登记表可能出现在日志/调试输出里，明文 token 不该跟着走。
    /// `None` 同时覆盖两种情况（"当时没配 token" 与 "配了但没带"）——因为前者握手
    /// 一定放行、后者一定被 401 拒绝，所以能走到登记的 `None` 必然属于"当时没配"。
    /// `/reload-trust` 用它复核"连接建立之后 token 才收紧"这类变更。
    pub(crate) token_fingerprint: Option<u64>,
    pub(crate) revoked: Arc<AtomicBool>,
    /// 这条连接登记过的会话 key。
    ///
    /// 以前只有 socket 循环内部的局部 `registered_ids`，`refresh_tabs_via_extension`
    /// 自己攒一份、函数返回就丢——于是"首次由 refresh 登记的会话"在 WS 断开时
    /// 永远不会被标成断开，`/health.ready` 谎报、空闲自杀也永远判不出可退出。
    /// 放进连接登记表后，refresh 与断开路径共享同一份清单。
    ///
    /// 类型是 `HashSet`：连接是长命的，标签反复开关时 `Vec` 只增不减，
    /// 而且每次登记都要做一次线性查重。断开清理末尾会把本连接登记的项整体回收。
    pub(crate) session_registry: Arc<std::sync::Mutex<HashSet<String>>>,
    /// 握手时创建的下行通道。断开清理时用它和会话上的 `sender` 比对
    /// （`same_channel`），避免扩展重载时新旧连接互相踩。
    pub(crate) sender: mpsc::UnboundedSender<String>,
}

/// token 的稳定指纹：**只用做相等比较，不承担密码学职责**。
///
/// 用途是判断"这条连接的 token 与当前配置的 token 是否同一个"，不是防碰撞的凭证。
/// 只需保证同一次构建内同一字符串哈希一致——`DefaultHasher` 不保证跨 Rust 版本稳定，
/// 而指纹只在进程内存里活（连接与配置在同一进程比较），所以这不构成问题。
pub(crate) fn token_fingerprint(token: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut hasher);
    hasher.finish()
}

/// 元素计数缓存：值里带 URL，导航后自动失效（见 `page_element_count`）。
/// 抽成别名是因为 clippy 认为内联的 `Arc<Mutex<HashMap<String, (usize, Instant, String)>>>`
/// 可读性太差——它说得对。
pub(crate) type PageScaleCache = Arc<Mutex<HashMap<String, (usize, Instant, String)>>>;

#[derive(Clone)]
pub struct AppState {
    pub(crate) driver: Arc<RwLock<DriverState>>,
    /// 在途执行独立成一把锁：结果回传/ACK 不再和会话读写抢同一把锁。
    pub(crate) execs: Arc<Mutex<crate::protocol::ExecRegistry>>,
    pub(crate) jobs: Arc<Mutex<JobStore>>,
    /// 每会话一把异步锁：同一标签的命令串行，不同标签互不阻塞。
    pub(crate) session_locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// 页面元素数缓存：(元素数, 采样时刻, 采样时的 URL)。
    ///
    /// 带上 URL 一起校验：只按时间过期时，导航到另一个页面后 2 分钟内仍拿旧页面
    /// 的元素数做判断，小页面→大页面的跳转会被误判成「小页面」，降级路径不触发。
    pub(crate) page_scale: PageScaleCache,
    /// 扩展 WebSocket 端口绑定失败的原因；为 None 表示正常。
    pub(crate) ws_bind_error: Arc<Mutex<Option<String>>>,
    /// 已经主动要求过重载的 (browser_id, 版本)，避免版本没变时反复触发。
    pub(crate) ext_reload_sent: Arc<Mutex<HashSet<String>>>,
    /// profile label 变更的串行锁：查重与写回之间隔着一次最长 10s 的扩展往返，
    /// 只用读写锁挡不住两个并发请求同时通过查重（A2-05）。
    pub(crate) profile_label_lock: Arc<Mutex<()>>,
    /// net 配置落盘的串行锁：落盘改成「后台线程读盘 → 合并当前 profile → 写回」，
    /// 读-改-写之间必须有锁，否则两个 profile 的并发保存会互相覆盖。
    pub(crate) net_persist_lock: Arc<Mutex<()>>,
    pub(crate) started_at: SystemTime,
    pub(crate) last_activity: Arc<Mutex<Instant>>,
    /// 正在处理的 HTTP 请求数。>0 时禁止空闲自杀：
    /// 单次长请求（download-wait/run/batch/broadcast/wait）期间不会再有新请求来 touch，
    /// 只看 `last_activity` 会把它们腰斩。
    pub(crate) inflight: Arc<AtomicUsize>,
    /// 「最近一次有请求进来」的时刻：**每条**请求进入 `track_inflight` 时刷新，
    /// 计数回到 0 时清空。只给 `monitor_idle_shutdown` 判断「在途请求是不是已经卡死」用。
    ///
    /// 语义刻意不是「最早一条在途请求的起点」：旧实现只在 0→1 时记起点，
    /// 那条请求离场后时间戳滞留，后续长请求会被提前判死；改成每条请求都刷新之后，
    /// 「`inflight > 0` 且这个时刻已经超过阈值」等价于「阈值时间内没有任何新请求进来」，
    /// 持续重叠的健康流量不会误触发。
    pub(crate) inflight_since: Arc<Mutex<Option<Instant>>>,
    /// 空闲自杀阈值（环境变量可覆盖），`health` 与实际行为共用同一个值。
    pub(crate) idle_shutdown_ttl: Duration,
    /// 「WebSocket 未启用身份校验」的提示每个进程只打一次，避免握手刷屏。
    pub(crate) ws_security_warned: Arc<AtomicBool>,
    /// 已建立的扩展连接登记表：`/reload-trust` 靠它把不再可信的连接真正踢掉。
    ///
    /// 用 `std::sync::Mutex`（而不是 tokio 的）：只在握手、断开、管理接口这三处短暂加锁，
    /// 且从不在持锁期间 await。好处是注销路径可以写在 `Drop` 里，panic 也不会漏删。
    pub(crate) ws_connections: Arc<std::sync::Mutex<HashMap<u64, WsConn>>>,
    /// 连接 id 自增，永不复用（日志与 `/reload-trust` 回包都靠它定位）。
    pub(crate) ws_conn_seq: Arc<AtomicUsize>,
    pub(crate) shutdown: mpsc::UnboundedSender<()>,
    pub(crate) sessions_ready: Arc<Notify>,
    pub(crate) extension_port: u16,
    pub(crate) api_token: String,
    /// 每个调用方（智能体）自己的默认标签页。多个智能体同时用时，
    /// `open` 只改自己的默认目标，不会把别人的 `read` 带到自己的页面上。
    pub(crate) agent_defaults: Arc<Mutex<HashMap<String, AgentTarget>>>,
    /// 每个调用方通过 `open` 打开过的会话，供 `cleanup` 收尾。
    pub(crate) agent_opened: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// 扩展每次 Service Worker 冷启动自增的序号，按 `browser_id:profile_id` 记录。
    ///
    /// 扩展在 `ext_ready` 里带 `restartSeq`（见 `assets/*/background.js`），
    /// daemon 侧必须记住上一次看到的值才能判断"这次是不是重启"：
    /// 重启意味着扩展侧的 Fetch 拦截状态已经没了，daemon 持有的
    /// `net_hooks` 必须标记为待重建，否则 `net route` 仍以为规则已生效。
    pub(crate) ext_restart_seq: Arc<Mutex<HashMap<String, u32>>>,
    /// `cleanup_on_shutdown` 是否已经跑过。
    ///
    /// idle / inflight-stall / job-stall 三条退出路径都是「先 cleanup 再
    /// `shutdown.send(())`」，而 `graceful_shutdown` 的闭包还会再调一次，
    /// `/shutdown` handler 也各调一次。没有幂等位就会向扩展重复下发
    /// `debugClearAll`，把刚重连上的新连接的拦截规则一并清掉（A1-06）。
    pub(crate) cleanup_done: Arc<AtomicBool>,
    /// 扩展最近一次自检上报的 `warnings`。
    ///
    /// 只在非空时覆盖：连上来的旧扩展不发这个字段，不能把已有告警清空。
    pub(crate) ext_warnings: Arc<Mutex<Vec<Value>>>,
    /// POST 幂等回放缓存：`{agent}|{用户 key}` -> 占位或已完成响应。
    ///
    /// 表的键**只到「调用方 + 用户 key」**，指纹（method/path/query/body）存在
    /// 条目内部：只有这样才能在「同一个 key 换了请求体」时识别出冲突并回 409。
    /// 若把指纹也拼进键里，换了 body 就变成另一条缓存项，冲突会静默通过并再执行一次。
    ///
    /// A1：`Done` 条目同时追加到 `~/.atb/idempotency.jsonl`，daemon 重启时由
    /// `load_idempotency_cache` 读回（`Pending` 永不落盘）。
    pub(crate) idempotency: Arc<Mutex<HashMap<String, IdempotencySlot>>>,
    /// 迟到结果暂存（A1-09）：`exec_id` -> (结果/错误, 登记时刻)。
    ///
    /// 扩展在 pending 已被超时路径 take 掉之后才回包的结果不再静默丢弃，
    /// 5 分钟内可凭 `GET /exec-result/:exec_id` 查回。
    pub(crate) late_results: Arc<Mutex<HashMap<String, (Value, Instant)>>>,
}

/// 单个智能体的目标状态。
#[derive(Debug, Clone, Default)]
pub(crate) struct AgentTarget {
    /// 已生效的默认会话。
    pub(crate) session_key: Option<String>,
    /// `open` 之后尚未完成注册的期望会话。
    pub(crate) preferred_key: Option<String>,
    /// `preferred_key` 的归属智能体。
    ///
    /// 旧实现只在 `agent == "default"` 时清 `preferred`，于是 A `open` 出的
    /// "待生效会话"会留给下一个没有默认目标的 B：B 的第一条命令直接落到 A 刚打开的
    /// 标签，并被写成 B 的默认。记下 owner 后只有 owner 能命中，命中即清空。
    pub(crate) preferred_owner: Option<String>,
}

tokio::task_local! {
    /// 当前 HTTP 请求的调用方身份。用 task-local 而不是给每个 handler 加参数：
    /// 选标签发生在很深的调用链里，逐层透传会污染所有函数签名。
    pub(crate) static AGENT_ID: String;
}

pub(crate) fn current_agent() -> String {
    AGENT_ID
        .try_with(|value| value.clone())
        .unwrap_or_else(|_| "default".to_string())
}

/// 所有 `Duration` 构造的统一入口：先过参数护栏再转换。
///
/// `Duration::from_secs_f64` 遇到 NaN / 负数 / >≈5.8e8 秒会 **panic**。
/// daemon 的超时值来自请求体，命令级校验难免有漏，这里兜最后一道。
pub(crate) fn secs(value: f64) -> Duration {
    std::time::Duration::from_secs_f64(crate::preflight::clamp_timeout_seconds(value))
}

/// 把 task-local 的调用方身份带进 `tokio::spawn` 出来的任务。
///
/// task-local **不会**随 spawn 继承，任务体里 `current_agent()` 会退回 `"default"`，
/// 于是 A 智能体的异步任务会落到 B 的默认标签上，且结果里看不出异常。
pub(crate) fn with_agent<F>(
    agent: String,
    future: F,
) -> tokio::task::futures::TaskLocalFuture<String, F>
where
    F: std::future::Future,
{
    AGENT_ID.scope(agent, future)
}

/// POST 幂等回放中间件。
///
/// 旧实现（v9 及以前）有三个互相独立的漏洞：
/// 1. 缓存键**只有用户给的 key**：同一个 key 换个 route / 换个 body 也会命中旧响应，
///    调用方拿到的是「另一个动作」的结果，却以为是自己这次的结果；
/// 2. 没有 in-flight 占位：首个请求还在执行时第二个请求查不到缓存，于是**又执行一次**，
///    幂等键在最需要它的窗口（并发重发 / CLI 超时后立刻重试）完全失效；
/// 3. 缓存里没有调用方身份：A 智能体用 key=k 的动作会被 B 智能体用 key=k 直接回放。
///
/// 现在的语义：
/// - 先读请求体，按 `method + path + query + body` 算指纹（`DefaultHasher`，不引依赖），
///   指纹与调用方一起绑定到条目上；
/// - 同一个 `{agent}|{key}` 但指纹不同 → 409 `idempotency_key_reused`，**不执行**；
/// - 同 key 同指纹且首个请求仍在执行 → 等它的完成通知（≤90s），再回放结果；
/// - 只对 POST 生效：GET 无副作用，重复请求天然安全；
/// - 幂等键只保证「响应丢失后重发」这一件事，不承担分布式锁的职责。
///
/// 分层位置（见 `build_api_router`）：在 token 校验之内——未授权请求不进缓存。
pub(crate) async fn idempotency_replay(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    if method != axum::http::Method::POST {
        return next.run(request).await;
    }
    let key = request
        .headers()
        .get("x-atb-idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let Some(key) = key else {
        return next.run(request).await;
    };
    // 只在「长度已知且不超限」时读体。
    //
    // 流式上传（没有 Content-Length）与超大请求体在这里直接放行、不缓存：
    // 它们的重放本来就不安全（无法确认体是否被截断），但绝不能因为幂等中间件
    // 读不动就失败——那是把「可选的加固」变成了「新增的故障点」。
    let declared_len = request
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<usize>().ok());
    if declared_len
        .map(|len| len > IDEMPOTENCY_BODY_LIMIT)
        .unwrap_or(true)
    {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let path = parts.uri.path().to_string();
    let query = parts.uri.query().unwrap_or("").to_string();
    let bytes = match axum::body::to_bytes(body, IDEMPOTENCY_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(err) => {
            // 体已经被这次带长度上限的读取消耗掉了，只能以空体重建后放行：
            // 该路径只在「Content-Length 撒谎 / 传输中断」时出现，不缓存。
            eprintln!("idempotency: 读取 {path} 的请求体失败，本次放行且不缓存: {err}");
            return next.run(Request::from_parts(parts, Body::empty())).await;
        }
    };
    // dry-run 没有副作用：缓存它会让「先 dry-run、再真跑」被回放成 dry-run 的结果。
    // 空 body（无参数动作）没有可比较的指纹差异，同样不缓存。
    let cacheable = !bytes.is_empty() && !body_requests_dry_run(&bytes);
    let request = Request::from_parts(parts, Body::from(bytes.clone()));
    if !cacheable {
        return next.run(request).await;
    }

    let agent = current_agent();
    let scope = idempotency_scope(&agent, method.as_str(), &path, &query, &bytes);
    // 键只到「调用方 + 用户 key」为止，指纹存在条目里：
    // 只有这样才能在「同一个 key 换了请求体」时识别出冲突并回 409
    // （把指纹拼进键里就变成另一条缓存项，冲突会静默通过并再执行一次）。
    let cache_key = format!("{agent}|{key}");
    let notify = match claim_idempotency_slot(&state, &cache_key, &scope, &key).await {
        SlotDecision::Replay(value) => return replay_response(value),
        SlotDecision::Conflict => return idempotency_conflict(&key, &scope),
        SlotDecision::Timeout => return idempotency_inflight_timeout(&key),
        SlotDecision::Lead(notify) => notify,
    };

    let response = next.run(request).await;
    // 缓存要求能完整读出响应体并解析成 JSON；流式/超大响应原样放行，不缓存也不破坏。
    let (parts, body) = response.into_parts();
    // 落盘记录里带上原状态码（回放本身仍按既有语义返回 200 + 标记，不改动）。
    let status = parts.status.as_u16();
    let bytes = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        // 响应体读不出来（客户端中断等）：无法缓存，也没有可重建的正文，
        // 只能返回空体 + 原状态头。该路径正常业务不会走到（响应都由 Json() 构造）。
        Err(err) => {
            // 但**必须**解除占位：否则同 key 的等待者会一直等到 90s 超时。
            eprintln!("idempotency: 读取 {path} 的响应体失败，本次不缓存且释放占位: {err}");
            yield_slot(&state, &cache_key, &scope, &notify, None, status).await;
            return Response::from_parts(parts, Body::empty());
        }
    };
    let cached = serde_json::from_slice::<Value>(&bytes).ok();
    if cached.is_none() {
        // 非 JSON 响应无法回放；移除占位并唤醒等待者，让它们各自决定下一步。
        eprintln!("idempotency: {path} 的响应不是 JSON，无法回放，已释放占位");
    }
    yield_slot(&state, &cache_key, &scope, &notify, cached, status).await;
    Response::from_parts(parts, Body::from(bytes))
}

/// 幂等指纹：`agent|method|path|query|body_hash`。
///
/// 用 `DefaultHasher` 而不是引依赖：这里只做「两次请求是不是同一个」的相等比较，
/// 不做密码学承诺；哈希值只在进程内存里比较，跨版本/跨进程稳定与否都不影响语义。
fn idempotency_scope(agent: &str, method: &str, path: &str, query: &str, body: &[u8]) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    format!("{agent}|{method}|{path}|{query}|{:016x}", hasher.finish())
}

/// 请求体是否是 `dry-run`。只认顶层布尔字段，避免把业务参数里同名的字段当成开关。
fn body_requests_dry_run(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| value.get("dry_run").and_then(Value::as_bool))
        .unwrap_or(false)
}

/// 抢占位的结果。
enum SlotDecision {
    /// 本请求是首个执行者：拿到占位，执行完必须 [`yield_slot`]。
    Lead(Arc<Notify>),
    /// 已有可回放的响应。
    Replay(Value),
    /// 同一个 key 换了请求（指纹不同）。
    Conflict,
    /// 首个请求 90s 内没有完成。
    Timeout,
}

/// 抢一个幂等占位；已有条目时按指纹决定「回放 / 409 / 等首个请求」。
///
/// **为什么是「检查 + 插入」同一把锁**：分两次加锁时两个并发请求都会看到
/// 「没有条目」，于是各自成为首个执行者——占位形同虚设。
async fn claim_idempotency_slot(
    state: &AppState,
    cache_key: &str,
    scope: &str,
    key: &str,
) -> SlotDecision {
    let deadline = Instant::now() + IDEMPOTENCY_INFLIGHT_WAIT;
    loop {
        let now = Instant::now();
        let step = {
            let mut cache = state.idempotency.lock().await;
            // 过期清理：只清 `Done`（`Pending` 永不过期，见 `IdempotencyEntry`）。
            cache.retain(|_, slot| slot.entry.is_fresh(now));
            match lookup_idempotency_step(&cache, cache_key, scope) {
                Some(step) => step,
                None => {
                    let notify = Arc::new(Notify::new());
                    evict_one_finished(&mut cache);
                    cache.insert(
                        cache_key.to_string(),
                        IdempotencySlot {
                            scope: scope.to_string(),
                            entry: IdempotencyEntry::Pending(notify.clone()),
                        },
                    );
                    // 注意：这是**自己的**占位，必须直接返回给调用方去执行，
                    // 绝不能变成 `Step::Wait`——那会变成「等自己」，空转到 90s 超时。
                    Step::Other(SlotDecision::Lead(notify))
                }
            }
        };
        match step {
            Step::Wait(notify) => {
                let now = Instant::now();
                if now >= deadline {
                    eprintln!(
                        "idempotency: key={key} 的首个请求 {}s 内未完成，本次不再等待",
                        IDEMPOTENCY_INFLIGHT_WAIT.as_secs()
                    );
                    return SlotDecision::Timeout;
                }
                // 分片等待：`notify_waiters` 不保存许可，若在「读完缓存 → 开始 await」
                // 之间恰好完成，单纯的 await 会一直睡到超时。切片 + 重新检查把
                // 这个窗口压到 ≤1s，总时长仍由 deadline 控制。
                let slice = deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(1000));
                let _ = tokio::time::timeout(slice, notify.notified()).await;
            }
            Step::Other(decision) => return decision,
        }
    }
}

/// 内层循环的两态：要么继续等，要么给出结论（避免在 match 分支里重复 `return`）。
enum Step {
    Wait(Arc<Notify>),
    Other(SlotDecision),
}

/// 在同一张表上判断「这个 key + 指纹」该怎么处理；`None` 表示表里没有这个 key。
///
/// 抽成独立函数不是为了复用，而是为了让「同 key 换 body 必须 409」这条语义可以被
/// 单测覆盖：**从磁盘恢复的条目走的也是这段判断**（恢复只重建条目，不改语义）。
fn lookup_idempotency_step(
    cache: &HashMap<String, IdempotencySlot>,
    cache_key: &str,
    scope: &str,
) -> Option<Step> {
    cache.get(cache_key).map(|slot| {
        if slot.scope != scope {
            Step::Other(SlotDecision::Conflict)
        } else {
            match &slot.entry {
                IdempotencyEntry::Done(_, value) => {
                    Step::Other(SlotDecision::Replay(value.clone()))
                }
                // 已有人拿着占位：等他的完成通知，不要自己执行。
                IdempotencyEntry::Pending(owner_notify) => Step::Wait(owner_notify.clone()),
            }
        }
    })
}

/// 兜底上限：超出时淘汰最旧的一项。
///
/// **选中 `Pending` 时跳过**（要求）：那些条目有等待者在 `notify` 上挂着，
/// 删掉它们会让等待者永远收不到完成通知，只能等到 90s 超时。
fn evict_one_finished(cache: &mut HashMap<String, IdempotencySlot>) {
    if cache.len() < IDEMPOTENCY_CACHE_LIMIT {
        return;
    }
    let oldest = cache
        .iter()
        .filter_map(|(key, slot)| match &slot.entry {
            IdempotencyEntry::Done(at, _) => Some((*at, key.clone())),
            IdempotencyEntry::Pending(_) => None,
        })
        .min_by_key(|(at, _)| *at)
        .map(|(_, key)| key);
    if let Some(oldest) = oldest {
        cache.remove(&oldest);
    } else {
        // 全是 Pending：宁可这次超限，也不能删掉有人等的条目。
        eprintln!("idempotency: 缓存已达上限 {IDEMPOTENCY_CACHE_LIMIT} 且全部在途，本次不做淘汰");
    }
}

/// 首个执行者收尾：写入可回放响应（或移除占位），然后唤醒所有等待者。
///
/// `value` 为 `None` 表示响应不可回放（非 JSON / 读取失败）：此时移除占位，
/// 等待者醒来后按「没有条目」重新抢一次——最坏情况是再执行一次，
/// 但这比「永远等不到」和「拿一份无法解析的缓存」都更可诊断。
async fn yield_slot(
    state: &AppState,
    cache_key: &str,
    scope: &str,
    notify: &Arc<Notify>,
    value: Option<Value>,
    status: u16,
) {
    {
        let mut cache = state.idempotency.lock().await;
        match value {
            Some(value) => {
                let entry = IdempotencyEntry::Done(Instant::now(), value);
                // A1：落盘与写内存在**同一把锁**内完成，顺序与内容天然一致；
                // 落盘失败只打一行日志，绝不影响本次请求的响应。
                if let Some(path) = idempotency_store_path() {
                    let at = epoch_secs() as u64;
                    append_idempotency_record(&path, cache_key, scope, at, status, &entry);
                }
                cache.insert(
                    cache_key.to_string(),
                    IdempotencySlot {
                        // 指纹保持首次执行时的那一份：同一 key 换 body 必须继续被拒。
                        scope: scope.to_string(),
                        entry,
                    },
                );
            }
            None => {
                cache.remove(cache_key);
            }
        }
    }
    notify.notify_waiters();
}

/// 回放响应打上标记，调用方能区分「刚执行的结果」与「60s 内的重复请求」。
///
/// 用 `as_object_mut` 而不是索引赋值：万一缓存里进了非对象 JSON（理论不该有），
/// 索引赋值会 panic，这里降级为不打标记，回放本身照常。
fn replay_response(mut value: Value) -> Response {
    if let Some(object) = value.as_object_mut() {
        object.insert("idempotent_replay".to_string(), json!(true));
    }
    Json(value).into_response()
}

/// 同一个幂等键被用于不同请求：拒绝执行，并说明是「key 复用」而不是「参数错」。
fn idempotency_conflict(key: &str, scope: &str) -> Response {
    eprintln!("idempotency: key={key} 已绑定到另一份请求（{scope}），本次拒绝执行");
    (
        StatusCode::CONFLICT,
        Json(json!({
            "ok": false,
            "error": "同一个幂等键被用于不同的请求（method/path/query/body 不一致）",
            "error_code": "idempotency_key_reused",
            "hint": "同一个 --idempotency-key 只能复用于同一请求；换了参数请换一个 key",
        })),
    )
        .into_response()
}

/// 首个请求 90s 内没有完成：不给「未知是否执行」的假成功，让调用方自己决定下一步。
fn idempotency_inflight_timeout(key: &str) -> Response {
    (
        StatusCode::GATEWAY_TIMEOUT,
        Json(json!({
            "ok": false,
            "error": format!("幂等键 {key} 的首个请求仍在执行，等待超过 {}s", IDEMPOTENCY_INFLIGHT_WAIT.as_secs()),
            "error_code": "idempotency_in_flight_timeout",
            "retryable": true,
            "hint": "命令很可能已在执行：不要换新 key 盲目重发，先用页面状态或 job get 确认结果",
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// A1：幂等缓存跨进程持久化
//
// 内存缓存只能在**同一个** daemon 生命周期内回放：daemon 重启（空闲退出、升级、
// 崩溃）后缓存清空，调用方带同一个 `--idempotency-key` 重发就会**二次执行**——
// 正是幂等键要防的场景。这里把 `Done` 条目追加到 `idempotency.jsonl`，启动时读回。
// ---------------------------------------------------------------------------

/// 落盘文件名：与 `api-token` 同目录（`config::user_config_dir()`）。
const IDEMPOTENCY_STORE_FILE: &str = "idempotency.jsonl";
/// 单条响应体的落盘上限：大响应（整页 HTML / base64）会迅速撑大文件，
/// 而回放窗口只有 60s，写进磁盘的性价比很低。
const IDEMPOTENCY_PERSIST_BODY_LIMIT: usize = 256 * 1024;
/// 启动期压缩阈值：文件超过它就重写一次（只保留未过期条目）。
/// 运行期不做 compaction——每条请求都去 stat/重写文件是纯浪费。
const IDEMPOTENCY_STORE_COMPACT_BYTES: u64 = 4 * 1024 * 1024;

fn idempotency_store_path() -> Option<PathBuf> {
    config::user_config_dir()
        .ok()
        .map(|dir| dir.join(IDEMPOTENCY_STORE_FILE))
}

/// 一条落盘记录。字段顺序即落盘顺序，方便人眼看。
///
/// `body` 直接是响应 JSON（不是被转义的字符串）：加载时不需要二次解析。
/// `at` 用整秒（回放窗口 60s，亚秒精度没有意义），也让 TTL 判断保持纯整数运算。
#[derive(Debug, Serialize, Deserialize)]
struct PersistedIdempotency {
    key: String,
    scope: String,
    at: u64,
    #[serde(default)]
    status: u16,
    body: Value,
}

/// 追加一条记录；**只有 `Done` 会落盘**，返回是否真的写了一行。
///
/// `Pending` 绝不能落盘：进程在动作执行到一半时崩掉，磁盘上没有任何可信结果，
/// 恢复出来只会把「不知道成没成」伪装成「执行成功」。
fn append_idempotency_record(
    path: &Path,
    cache_key: &str,
    scope: &str,
    at: u64,
    status: u16,
    entry: &IdempotencyEntry,
) -> bool {
    let IdempotencyEntry::Done(_, value) = entry else {
        return false;
    };
    let body = match serde_json::to_string(value) {
        Ok(body) => body,
        Err(err) => {
            eprintln!("idempotency: 响应序列化失败，跳过本次落盘（内存回放不受影响）: {err}");
            return false;
        }
    };
    if body.len() > IDEMPOTENCY_PERSIST_BODY_LIMIT {
        // 大响应不落盘：回放窗口只有 60s，写进去只会让文件迅速膨胀。
        return false;
    }
    let record = PersistedIdempotency {
        key: cache_key.to_string(),
        scope: scope.to_string(),
        at,
        status,
        body: value.clone(),
    };
    let mut line = match serde_json::to_string(&record) {
        Ok(line) => line,
        Err(err) => {
            eprintln!("idempotency: 落盘记录序列化失败，跳过本次落盘: {err}");
            return false;
        }
    };
    line.push('\n');
    if let Some(parent) = path.parent() {
        // 尽力而为：目录不存在（首次运行）时建出来；失败留给 open 报错。
        let _ = fs::create_dir_all(parent);
    }
    // 追加而不是重写：并发请求各自写一行，不需要读-改-写，也就不会互相截断。
    let mut file = match fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => file,
        Err(err) => {
            eprintln!(
                "idempotency: 打开 {} 失败，跳过本次落盘: {err}",
                path.display()
            );
            return false;
        }
    };
    match file.write_all(line.as_bytes()) {
        Ok(()) => true,
        Err(err) => {
            eprintln!(
                "idempotency: 写 {} 失败，跳过本次落盘: {err}",
                path.display()
            );
            false
        }
    }
}

/// 读出仍然可用的条目（按落盘顺序，越靠后越新）。
///
/// 坏行、指纹/键为空、时间戳非法的行一律 **跳过而不是 panic**：这个文件是
/// 尽力而为的加速缓存，磁盘上出现半行（写一半被杀）完全正常，不能因此让 daemon
/// 起不来。返回的条目已按 TTL 过滤，且不超过 `IDEMPOTENCY_CACHE_LIMIT` 条。
fn read_persisted_idempotency(path: &Path, now: u64) -> Vec<PersistedIdempotency> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "idempotency: 读取 {} 失败，按空缓存启动（不影响启动）: {err}",
                    path.display()
                );
            }
            return Vec::new();
        }
    };
    let mut kept: Vec<PersistedIdempotency> = Vec::new();
    let mut skipped = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<PersistedIdempotency>(line) {
            Ok(record) => {
                if record.key.trim().is_empty() || record.scope.trim().is_empty() {
                    skipped += 1;
                    continue;
                }
                // 状态码不在 1xx..5xx 内：手改/版本错位的行，按坏行处理。
                if !(100..600).contains(&record.status) {
                    skipped += 1;
                    continue;
                }
                // 时钟回拨时 `at` 可能大于 now：按「刚写入」处理，不为负数。
                let age = now.saturating_sub(record.at);
                if age >= IDEMPOTENCY_TTL.as_secs() {
                    skipped += 1;
                    continue;
                }
                kept.push(record);
            }
            // 半行 / 手改坏的行：跳过，绝不 panic。
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        eprintln!("idempotency: 忽略 {skipped} 条过期或损坏的落盘记录");
    }
    if kept.len() > IDEMPOTENCY_CACHE_LIMIT {
        // 只保留最新的 N 条：上限与内存表一致，防止异常客户端用随机 key 把文件灌大。
        kept.sort_by_key(|record| record.at);
        let overflow = kept.len() - IDEMPOTENCY_CACHE_LIMIT;
        kept.drain(0..overflow);
    }
    kept
}

/// 把落盘条目装回内存表。`at` 换算成 `Instant` 后，后续 TTL 语义与首次执行完全一致。
fn restore_idempotency_entries(
    cache: &mut HashMap<String, IdempotencySlot>,
    records: Vec<PersistedIdempotency>,
    now: u64,
) -> usize {
    let now_instant = Instant::now();
    let mut restored = 0usize;
    for record in records {
        // `is_fresh` 只看「登记时刻距今多久」，所以把 Instant 往前拨 age 秒即可，
        // 不需要额外字段；进程刚起来时 `checked_sub` 理论上不会失败（age < 60s），
        // 真失败就退化成「刚写入」，最多多活一个 TTL。
        let age = now.saturating_sub(record.at);
        let at = now_instant
            .checked_sub(Duration::from_secs(age))
            .unwrap_or(now_instant);
        cache.insert(
            record.key,
            IdempotencySlot {
                scope: record.scope,
                entry: IdempotencyEntry::Done(at, record.body),
            },
        );
        restored += 1;
    }
    restored
}

/// 启动期压缩：把仍然有效的条目重写成新文件（临时文件 + rename，避免半截文件）。
fn compact_idempotency_file(path: &Path, records: &[PersistedIdempotency]) -> bool {
    let mut text = String::new();
    for record in records {
        match serde_json::to_string(record) {
            Ok(line) => {
                text.push_str(&line);
                text.push('\n');
            }
            Err(err) => eprintln!("idempotency: 压缩时序列化失败，跳过该条: {err}"),
        }
    }
    let tmp = path.with_extension(format!("jsonl.tmp.{}", std::process::id()));
    if let Err(err) = fs::write(&tmp, text.as_bytes()) {
        eprintln!("idempotency: 写压缩临时文件失败（保留原文件）: {err}");
        return false;
    }
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        eprintln!("idempotency: 压缩后替换失败（保留原文件）: {err}");
        return false;
    }
    true
}

/// daemon 启动时恢复幂等缓存（`boot.rs` 在开始服务前调用）。
///
/// 任何失败都只是「没有跨进程回放」：文件不存在、无权限、内容损坏都不阻断启动。
pub(crate) async fn load_idempotency_cache(state: &AppState) {
    let Some(path) = idempotency_store_path() else {
        return;
    };
    let now = epoch_secs() as u64;
    let records = read_persisted_idempotency(&path, now);
    // 启动期压缩：只在此处做一次，运行期不再碰文件长度。
    let oversized = fs::metadata(&path)
        .map(|meta| meta.len() > IDEMPOTENCY_STORE_COMPACT_BYTES)
        .unwrap_or(false);
    if oversized && compact_idempotency_file(&path, &records) {
        eprintln!(
            "idempotency: {} 超过 {}MB，已压缩为 {} 条未过期记录",
            path.display(),
            IDEMPOTENCY_STORE_COMPACT_BYTES / (1024 * 1024),
            records.len()
        );
    }
    if records.is_empty() {
        return;
    }
    let restored = {
        let mut cache = state.idempotency.lock().await;
        restore_idempotency_entries(&mut cache, records, now)
    };
    eprintln!("idempotency: 从磁盘恢复 {restored} 条可回放响应");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试用独立的临时文件：并发跑测试时不会互相覆盖。
    fn temp_store_path(tag: &str) -> PathBuf {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        let mut path = std::env::temp_dir();
        path.push(format!(
            "atb-idempotency-test-{}-{tag}-{seq}-{nanos}.jsonl",
            std::process::id()
        ));
        path
    }

    fn done_entry(value: Value) -> IdempotencyEntry {
        IdempotencyEntry::Done(Instant::now(), value)
    }

    fn line_count(path: &Path) -> usize {
        fs::read_to_string(path)
            .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
            .unwrap_or(0)
    }

    /// A1 验收：Done 条目落盘后能被重新加载、body 一致；过期条目被丢弃；Pending 不落盘。
    #[test]
    fn persisted_entries_survive_reload() {
        let path = temp_store_path("reload");
        let now = epoch_secs() as u64;
        let body = json!({ "ok": true, "tab": "t1", "n": 1 });
        assert!(append_idempotency_record(
            &path,
            "agent|k1",
            "agent|POST|/open||aaaa",
            now,
            200,
            &done_entry(body.clone()),
        ));
        // 过期条目：确实写进了文件，但加载时必须被丢掉。
        assert!(append_idempotency_record(
            &path,
            "agent|k-old",
            "agent|POST|/read||bbbb",
            now.saturating_sub(IDEMPOTENCY_TTL.as_secs() + 60),
            200,
            &done_entry(json!({ "ok": true })),
        ));
        // Pending 不落盘：没有任何可信结果可回放。
        assert!(!append_idempotency_record(
            &path,
            "agent|k-pending",
            "agent|POST|/exec||cccc",
            now,
            200,
            &IdempotencyEntry::Pending(Arc::new(Notify::new())),
        ));
        assert_eq!(line_count(&path), 2, "只有两条 Done 应该落盘");

        let records = read_persisted_idempotency(&path, now);
        assert_eq!(records.len(), 1, "过期条目必须被丢弃");
        let mut cache = HashMap::new();
        assert_eq!(restore_idempotency_entries(&mut cache, records, now), 1);
        assert_eq!(cache.len(), 1, "Pending 不得出现在恢复结果里");

        let slot = cache.get("agent|k1").expect("同 key 同指纹必须命中");
        assert_eq!(slot.scope, "agent|POST|/open||aaaa");
        match &slot.entry {
            IdempotencyEntry::Done(_, value) => assert_eq!(value, &body),
            IdempotencyEntry::Pending(_) => panic!("恢复出来的条目必须是 Done"),
        }
        assert!(
            slot.entry.is_fresh(Instant::now()),
            "60s 内的条目应仍然有效"
        );
        // 回放路径：同指纹命中，且返回的就是首次响应。
        match lookup_idempotency_step(&cache, "agent|k1", "agent|POST|/open||aaaa") {
            Some(Step::Other(SlotDecision::Replay(value))) => assert_eq!(value, body),
            _ => panic!("同 key 同指纹应回放首次响应"),
        }
        let _ = fs::remove_file(&path);
    }

    /// A1 验收：恢复出来的条目仍然遵守「同 key 换 body → 409」，不会被误回放。
    #[test]
    fn reloaded_entry_still_rejects_other_fingerprint() {
        let path = temp_store_path("fingerprint");
        let now = epoch_secs() as u64;
        assert!(append_idempotency_record(
            &path,
            "agent|k2",
            "agent|POST|/open||aaaa",
            now,
            200,
            &done_entry(json!({ "ok": true })),
        ));
        let mut cache = HashMap::new();
        restore_idempotency_entries(&mut cache, read_persisted_idempotency(&path, now), now);

        match lookup_idempotency_step(&cache, "agent|k2", "agent|POST|/open||bbbb") {
            Some(Step::Other(SlotDecision::Conflict)) => {}
            _ => panic!("同一个 key 换了请求体必须判冲突"),
        }
        // 409 响应本身的语义不变（仍然是 idempotency_key_reused）。
        assert_eq!(
            idempotency_conflict("k2", "agent|POST|/open||bbbb").status(),
            StatusCode::CONFLICT
        );
        let _ = fs::remove_file(&path);
    }

    /// A1 要求：加载时超过上限只保留最新的 N 条，避免异常 key 把文件/内存撑大。
    #[test]
    fn reload_keeps_newest_entries_only() {
        let path = temp_store_path("limit");
        let now = epoch_secs() as u64;
        let total = IDEMPOTENCY_CACHE_LIMIT + 8;
        for index in 0..total {
            // at 单调不减且都在 TTL 内：`sort_by_key` 稳定排序因此保留插入顺序，
            // 淘汰的必然是最早那几条。
            let age = (total - 1 - index) as u64 / 10;
            assert!(append_idempotency_record(
                &path,
                &format!("agent|k{index}"),
                "agent|POST|/x||dddd",
                now.saturating_sub(age),
                200,
                &done_entry(json!({ "i": index })),
            ));
        }
        let records = read_persisted_idempotency(&path, now);
        assert_eq!(records.len(), IDEMPOTENCY_CACHE_LIMIT);
        assert!(
            records
                .iter()
                .any(|record| record.key == format!("agent|k{}", total - 1)),
            "最新的条目必须保留"
        );
        assert!(
            !records.iter().any(|record| record.key == "agent|k0"),
            "最旧的条目必须被淘汰"
        );
        let _ = fs::remove_file(&path);
    }

    /// 坏行（写一半被杀）不能让加载 panic，也不能影响同一文件里的好行。
    #[test]
    fn corrupt_lines_are_skipped() {
        let path = temp_store_path("corrupt");
        let now = epoch_secs() as u64;
        assert!(append_idempotency_record(
            &path,
            "agent|k3",
            "agent|POST|/open||eeee",
            now,
            200,
            &done_entry(json!({ "ok": true })),
        ));
        let mut text = fs::read_to_string(&path).unwrap_or_default();
        text.push_str("{\"key\":\"agent|half\",\"scope\":\"x\",\"at\":");
        text.push('\n');
        text.push_str("not json at all\n");
        fs::write(&path, text).unwrap_or_default();

        let records = read_persisted_idempotency(&path, now);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, "agent|k3");
        let _ = fs::remove_file(&path);
    }

    /// 构造一条最小可用的会话：A2 的判据只看 `is_active()`，其余字段只需形状正确。
    fn test_session(session_key: &str, browser_id: &str, disconnected: bool) -> Session {
        let (sender, _receiver) = mpsc::unbounded_channel();
        Session {
            session_key: session_key.to_string(),
            tab_id: "1".to_string(),
            browser_id: browser_id.to_string(),
            profile_id: "profile".to_string(),
            profile_label: None,
            extension_version: None,
            extension_protocol: 0,
            file_scheme_access: None,
            info: TabInfo {
                id: "1".to_string(),
                window_id: None,
                tab_id: "1".to_string(),
                browser_id: browser_id.to_string(),
                profile_id: "profile".to_string(),
                profile_label: None,
                browser_label: None,
                session_key: session_key.to_string(),
                url: "about:blank".to_string(),
                title: String::new(),
                tab_type: "ext_ws".to_string(),
                group_title: None,
                active: false,
                connected_at: Some(0.0),
            },
            sender,
            disconnected_at: disconnected.then(Instant::now),
            conn_id: 1,
        }
    }

    /// A2 验收：两个 browser 同时连着时，默认目标只有「当前默认失效」后才允许改选。
    ///
    /// 判据本体在 `ws.rs::should_set_default_session`；测试放在本文件，是因为本子任务
    /// 只被授权在 state.rs 自由新增代码（sessions.rs / ws.rs 仅授权指定点位）。
    #[test]
    fn default_session_is_sticky_while_active() {
        let mut driver = DriverState::default();
        let key_a = "browser-a:profile:1".to_string();
        let key_b = "browser-b:profile:2".to_string();
        driver
            .sessions
            .insert(key_a.clone(), test_session(&key_a, "browser-a", false));
        driver
            .sessions
            .insert(key_b.clone(), test_session(&key_b, "browser-b", false));
        driver.default_session_key = Some(key_a.clone());

        // 两个浏览器都健康：B 的推送不得抢走默认（无论它是不是最近连上的那个）。
        assert!(!should_set_default_session(&driver, &key_b));
        // 默认自己重新注册：也不需要改写，保持不动。
        assert!(!should_set_default_session(&driver, &key_a));

        // 第 1 条验收：default 指向「仍在表内但已 disconnected」的会话 → 允许改选。
        driver.sessions.get_mut(&key_a).unwrap().disconnected_at = Some(Instant::now());
        assert!(should_set_default_session(&driver, &key_b));

        // A 被清理（会话不存在）：同样允许改选。
        driver.sessions.remove(&key_a);
        assert!(should_set_default_session(&driver, &key_b));

        // 完全没有默认时可以直接建立。
        driver.default_session_key = None;
        assert!(should_set_default_session(&driver, &key_b));

        // 第 2 条验收：preferred（`open` 刚打开、尚未注册）存在时不改写别的会话；
        // 只有 preferred 自己命中才返回 true。
        driver.default_session_key = Some(key_b);
        driver.set_preferred_default_session("browser-c:profile:3".to_string());
        assert!(!should_set_default_session(&driver, "browser-b:profile:2"));
        assert!(should_set_default_session(&driver, "browser-c:profile:3"));
    }

    /// A2 TTL：6 小时内（1 小时前登记）仍然有效，且设置时一定写时间戳。
    #[test]
    fn preferred_default_survives_within_ttl() {
        let mut driver = DriverState::default();
        driver.set_preferred_default_session("browser-a:profile:1".to_string());
        assert!(
            driver.preferred_default_session_at.is_some(),
            "设置 preferred 时必须同时写时间戳"
        );
        assert_eq!(
            driver.preferred_default_session(),
            Some("browser-a:profile:1")
        );
        assert!(!driver.expire_preferred_default_session(), "未过期不该清");

        // 1 小时前登记的：仍在 6h 窗口内。
        driver.preferred_default_session_at = Some(epoch_secs() - 1.0 * 3600.0);
        assert_eq!(
            driver.preferred_default_session(),
            Some("browser-a:profile:1")
        );
        assert!(!driver.expire_preferred_default_session());
        assert_eq!(
            driver.preferred_default_session_key.as_deref(),
            Some("browser-a:profile:1")
        );
    }

    /// A2 TTL：超过 6 小时视为「没有」，并且被真正清掉（key 与时间戳一起清）。
    #[test]
    fn preferred_default_expires_after_ttl() {
        // 用结构体更新语法而不是「先 default 再逐字段赋值」：clippy 的
        // field_reassign_with_default 会把后者判成告警（-D warnings 下即失败）。
        let mut driver = DriverState {
            preferred_default_session_key: Some("browser-a:profile:1".to_string()),
            preferred_default_session_at: Some(epoch_secs() - 7.0 * 3600.0),
            ..DriverState::default()
        };
        assert!(
            driver.preferred_default_session().is_none(),
            "过期记忆读出来必须当作没有"
        );
        assert!(
            driver.expire_preferred_default_session(),
            "过期记忆应被清掉"
        );
        assert!(driver.preferred_default_session_key.is_none());
        assert!(driver.preferred_default_session_at.is_none());
        // 清掉之后，默认目标可以照常建立（不会被一条陈旧记忆永久卡住）。
        assert!(should_set_default_session(&driver, "browser-b:profile:2"));
    }

    /// A2：断线时只有「它就是当前默认」且「没有更晚的 preferred」才记下来。
    ///
    /// 记下来的 preferred 会在重连快照里被 `apply_tabs_snapshot` 消费掉（设回默认 + 清空），
    /// 这条断言守的是「什么时候值得记」这一半；消费那一半是快照路径，属 sessions.rs。
    #[test]
    fn disconnect_remembers_default_only_when_it_is_the_default() {
        let mut driver = DriverState::default();
        let key = "browser-a:profile:1";
        assert!(
            !remember_default_on_disconnect(&driver, key),
            "没有默认时不该记"
        );

        driver.default_session_key = Some(key.to_string());
        assert!(
            remember_default_on_disconnect(&driver, key),
            "默认就是它时应记"
        );
        assert!(
            !remember_default_on_disconnect(&driver, "browser-b:profile:2"),
            "别的会话断线不该动默认记忆"
        );

        driver.set_preferred_default_session("browser-c:profile:3".to_string());
        assert!(
            !remember_default_on_disconnect(&driver, key),
            "已有更晚的 preferred 时不能覆盖"
        );
    }
}
