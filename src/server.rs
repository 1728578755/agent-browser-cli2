use crate::open_target::{resolve_open_target, OpenTargetError, TargetKind};
use crate::protocol::{
    DriverState, ElementDomInfo, ElementRef, ExecResult, RectInfo, Session, SnapshotCache, TabInfo,
    WsIncoming,
};
use crate::{config, html};
use anyhow::{anyhow, Result};
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header::ORIGIN, HeaderMap, Request, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{mpsc, oneshot, Mutex, Notify, RwLock};
use uuid::Uuid;

const HOST: &str = "127.0.0.1";
const API_PORT: u16 = 18767;
const LOOPBACK_API_ORIGIN: &str = "http://127.0.0.1:18767";
const LOCALHOST_API_ORIGIN: &str = "http://localhost:18767";
const CHROME_EXTENSION_ORIGIN_PREFIX: &str = "chrome-extension://";
// daemon 在无 CLI/API 业务请求后自动退出，避免浏览器扩展长期保持“已连接”浮层。
// 这是默认值，可用 AGENT_BROWSER_CLI_IDLE_TTL（秒）覆盖，见 `idle_shutdown_ttl_from_env`。
const IDLE_SHUTDOWN_TTL: Duration = Duration::from_secs(300);
const IDLE_TTL_ENV: &str = "AGENT_BROWSER_CLI_IDLE_TTL";
const IDLE_SHUTDOWN_CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// 断开的会话保留一段时间（便于短暂重连复用），超过后连同 snapshot 缓存一起清理。
const STALE_SESSION_TTL: Duration = Duration::from_secs(600);
/// `wait_for_sessions` 的只读档：浏览器没开时，`tabs`/`lookup` 不值得白等 5 秒。
const WAIT_SESSIONS_READ: Duration = Duration::from_millis(800);
/// `wait_for_sessions` 的动作档：用户可能刚点开浏览器，动作命令要多等一会儿。
const WAIT_SESSIONS_ACTION: Duration = Duration::from_secs(5);
/// `acked` 表的兜底上限。加了「只有 pending 里存在才登记」之后理论上不会超，
/// 这里只是防止异常输入把内存吃掉。
const ACK_TRACK_LIMIT: usize = 4096;
/// 「已请求过扩展重载」登记表的上限：键是 `browser_id:version`，旧版本不会回流。
const EXT_RELOAD_TRACK_LIMIT: usize = 256;

/// 一条已建立的扩展连接。`revoked` 是给 socket 循环轮询的撤销信号：
/// `/reload-trust` 置位后，连接会在下一次轮询（≤500ms）里收到 Close 并断开。
struct WsConn {
    extension_id: String,
    /// 握手时扩展在 `?token=` 里提供的值的指纹（没带就是 `None`）。
    ///
    /// 存指纹而不是明文：登记表可能出现在日志/调试输出里，明文 token 不该跟着走。
    /// `None` 同时覆盖两种情况（"当时没配 token" 与 "配了但没带"）——因为前者握手
    /// 一定放行、后者一定被 401 拒绝，所以能走到登记的 `None` 必然属于"当时没配"。
    /// `/reload-trust` 用它复核"连接建立之后 token 才收紧"这类变更。
    token_fingerprint: Option<u64>,
    revoked: Arc<AtomicBool>,
}

/// token 的稳定指纹：**只用做相等比较，不承担密码学职责**。
///
/// 用途是判断"这条连接的 token 与当前配置的 token 是否同一个"，不是防碰撞的凭证。
/// 只需保证同一次构建内同一字符串哈希一致——`DefaultHasher` 不保证跨 Rust 版本稳定，
/// 而指纹只在进程内存里活（连接与配置在同一进程比较），所以这不构成问题。
fn token_fingerprint(token: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut hasher);
    hasher.finish()
}

/// 元素计数缓存：值里带 URL，导航后自动失效（见 `page_element_count`）。
/// 抽成别名是因为 clippy 认为内联的 `Arc<Mutex<HashMap<String, (usize, Instant, String)>>>`
/// 可读性太差——它说得对。
type PageScaleCache = Arc<Mutex<HashMap<String, (usize, Instant, String)>>>;

#[derive(Clone)]
pub struct AppState {
    driver: Arc<RwLock<DriverState>>,
    /// 在途执行独立成一把锁：结果回传/ACK 不再和会话读写抢同一把锁。
    execs: Arc<Mutex<crate::protocol::ExecRegistry>>,
    jobs: Arc<Mutex<JobStore>>,
    /// 每会话一把异步锁：同一标签的命令串行，不同标签互不阻塞。
    session_locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// 页面元素数缓存：(元素数, 采样时刻, 采样时的 URL)。
    ///
    /// 带上 URL 一起校验：只按时间过期时，导航到另一个页面后 2 分钟内仍拿旧页面
    /// 的元素数做判断，小页面→大页面的跳转会被误判成「小页面」，降级路径不触发。
    page_scale: PageScaleCache,
    /// 扩展 WebSocket 端口绑定失败的原因；为 None 表示正常。
    ws_bind_error: Arc<Mutex<Option<String>>>,
    /// 已经主动要求过重载的 (browser_id, 版本)，避免版本没变时反复触发。
    ext_reload_sent: Arc<Mutex<HashSet<String>>>,
    /// profile label 变更的串行锁：查重与写回之间隔着一次最长 10s 的扩展往返，
    /// 只用读写锁挡不住两个并发请求同时通过查重。
    profile_label_lock: Arc<Mutex<()>>,
    /// net 配置落盘的串行锁：落盘改成「后台线程读盘 → 合并当前 profile → 写回」，
    /// 读-改-写之间必须有锁，否则两个 profile 的并发保存会互相覆盖（B-13）。
    net_persist_lock: Arc<Mutex<()>>,
    started_at: SystemTime,
    last_activity: Arc<Mutex<Instant>>,
    /// 正在处理的 HTTP 请求数。>0 时禁止空闲自杀：
    /// 单次长请求（download-wait/run/batch/broadcast/wait）期间不会再有新请求来 touch，
    /// 只看 `last_activity` 会把它们腰斩。
    inflight: Arc<AtomicUsize>,
    /// 最早一条在途请求的开始时间：计数 0→1 时记录，回到 0 时清空。
    /// 只给 `monitor_idle_shutdown` 判断「在途请求是不是已经卡死」用。
    inflight_since: Arc<Mutex<Option<Instant>>>,
    /// 空闲自杀阈值（环境变量可覆盖），`health` 与实际行为共用同一个值。
    idle_shutdown_ttl: Duration,
    /// 「WebSocket 未启用身份校验」的提示每个进程只打一次，避免握手刷屏。
    ws_security_warned: Arc<AtomicBool>,
    /// 已建立的扩展连接登记表：`/reload-trust` 靠它把不再可信的连接真正踢掉。
    ///
    /// 用 `std::sync::Mutex`（而不是 tokio 的）：只在握手、断开、管理接口这三处短暂加锁，
    /// 且从不在持锁期间 await。好处是注销路径可以写在 `Drop` 里，panic 也不会漏删。
    ws_connections: Arc<std::sync::Mutex<HashMap<u64, WsConn>>>,
    /// 连接 id 自增，永不复用（日志与 `/reload-trust` 回包都靠它定位）。
    ws_conn_seq: Arc<AtomicUsize>,
    shutdown: mpsc::UnboundedSender<()>,
    sessions_ready: Arc<Notify>,
    extension_port: u16,
    api_token: String,
}

#[derive(Debug, Deserialize)]
struct ScanRequest {
    #[serde(default)]
    tabs_only: bool,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    #[serde(default)]
    text_only: bool,
    /// 快速模式：单次注入的紧凑文本抽取，超大页面也不整页序列化。
    #[serde(default)]
    fast: bool,
    /// 节点/元素处理上限（0 表示默认）。
    #[serde(default)]
    max_nodes: Option<usize>,
    /// 页面内容抓取上限（秒）。
    #[serde(default)]
    timeout: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ExecRequest {
    #[serde(default)]
    script: String,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    #[serde(default)]
    no_monitor: bool,
    wait_js: Option<String>,
    #[serde(default = "default_wait_timeout")]
    wait_timeout: f64,
    #[serde(default = "default_wait_interval")]
    wait_interval: f64,
    /// 页面脚本执行上限（秒）。后台长任务需要能放宽，默认 60 秒。
    #[serde(default)]
    timeout: Option<f64>,
    /// 返回值上限（字符）。超出时返回截断预览，避免一条命令占满上下文。
    #[serde(default)]
    max_bytes: Option<usize>,
    /// 执行环境：MAIN（默认，可访问页面变量）或 ISOLATED（不受页面 CSP 影响）。
    #[serde(default)]
    world: Option<String>,
    /// 指定 iframe 的 frameId；配合 frames 命令使用。
    #[serde(default)]
    frame_id: Option<i64>,
    /// 在标签页的所有 frame 里执行。
    #[serde(default)]
    all_frames: bool,
}

#[derive(Debug, Deserialize)]
struct OpenRequest {
    target: Option<String>,
    url: Option<String>,
    cwd: Option<PathBuf>,
    #[serde(default = "default_active")]
    active: bool,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    session: Option<String>,
    group_title: Option<String>,
    #[serde(default)]
    window: bool,
    #[serde(default)]
    allow_focus: bool,
    /// 保持受控标签页不被 Chrome 内存节省机制丢弃，后台长任务必需。
    #[serde(default = "default_true")]
    keep_alive: bool,
    #[serde(default = "default_open_readiness_timeout")]
    readiness_timeout: f64,
}

#[derive(Debug, Deserialize)]
struct CloseRequest {
    tab_id: String,
    browser: Option<String>,
    profile: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SnapshotRequest {
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_snapshot_limit")]
    limit: usize,
    #[serde(default)]
    details: bool,
    /// 强制走 DOM 快照（不走 CDP），用于 CDP 被其他扩展 iframe 挡住的页面。
    #[serde(default)]
    js: bool,
    /// 只保留可操作元素（按钮/链接/输入框等），显著缩小大树。
    #[serde(default)]
    interactive_only: bool,
    /// 返回节点上限，超出截断（默认 0 表示不限制）。
    #[serde(default)]
    max_nodes: usize,
    #[serde(default = "default_high_level_timeout")]
    timeout: f64,
}

#[derive(Debug, Deserialize)]
struct TargetActionRequest {
    target: String,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    #[serde(default)]
    monitor: bool,
    wait_js: Option<String>,
    #[serde(default = "default_wait_timeout")]
    wait_timeout: f64,
    #[serde(default = "default_wait_interval")]
    wait_interval: f64,
    #[serde(default = "default_high_level_timeout")]
    timeout: f64,
    frame_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct FillRequest {
    target: String,
    #[serde(default)]
    value: String,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    #[serde(default)]
    append: bool,
    #[serde(default)]
    clear: bool,
    #[serde(default)]
    has_value: bool,
    #[serde(default)]
    monitor: bool,
    wait_js: Option<String>,
    #[serde(default = "default_wait_timeout")]
    wait_timeout: f64,
    #[serde(default = "default_wait_interval")]
    wait_interval: f64,
    #[serde(default = "default_high_level_timeout")]
    timeout: f64,
    frame_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SendKeysRequest {
    #[serde(default)]
    frame_id: Option<i64>,
    keys: String,
    target: Option<String>,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    #[serde(default)]
    monitor: bool,
    wait_js: Option<String>,
    #[serde(default = "default_wait_timeout")]
    wait_timeout: f64,
    #[serde(default = "default_wait_interval")]
    wait_interval: f64,
    #[serde(default = "default_high_level_timeout")]
    timeout: f64,
}

#[derive(Debug, Deserialize)]
struct ScreenshotRequest {
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    target: Option<String>,
    selector: Option<String>,
    out: Option<PathBuf>,
    #[serde(default = "default_screenshot_format")]
    format: String,
    quality: Option<u8>,
    #[serde(default)]
    full_page: bool,
    #[serde(default = "default_high_level_timeout")]
    timeout: f64,
}

#[derive(Debug, Deserialize)]
struct SavePdfRequest {
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    out: Option<PathBuf>,
    #[serde(default = "default_pdf_paper")]
    paper: String,
    #[serde(default)]
    landscape: bool,
    #[serde(default = "default_pdf_scale")]
    scale: f64,
    #[serde(default = "default_true")]
    print_background: bool,
    #[serde(default = "default_high_level_timeout")]
    timeout: f64,
}

#[derive(Debug, Deserialize, Default)]
struct TabsQuery {
    browser: Option<String>,
    profile: Option<String>,
    /// 输出完整 URL（默认截断到 80 字符以节省 token）。
    #[serde(default)]
    full: bool,
}

#[derive(Debug, Deserialize, Default)]
struct CookiesRequest {
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    /// 不传则用当前页面 URL。
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UploadRequest {
    target: String,
    #[serde(default)]
    files: Vec<String>,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    #[serde(default = "default_high_level_timeout")]
    timeout: f64,
}

#[derive(Debug, Deserialize, Default)]
struct LookupTabQuery {
    browser: Option<String>,
    profile: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct TabtreeQuery {
    tab: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    #[serde(default)]
    full: bool,
}

#[derive(Debug, Clone)]
struct SessionTreeItem {
    browser_id: String,
    profile_id: String,
    profile_label: Option<String>,
    tab_id: String,
    session_key: String,
    title: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct ProfileLabelRequest {
    label: Option<String>,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TabRequest {
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NetworkListRequest {
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    filter: Option<String>,
    #[serde(default = "default_debug_list_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct NetworkDetailRequest {
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    request_id: String,
}

#[derive(Debug, Deserialize)]
struct ConsoleListRequest {
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    level: Option<String>,
    #[serde(default = "default_debug_list_limit")]
    limit: usize,
}

fn default_active() -> bool {
    true
}

fn default_wait_timeout() -> f64 {
    3.0
}

fn default_open_readiness_timeout() -> f64 {
    10.0
}

fn default_wait_interval() -> f64 {
    0.1
}

fn default_snapshot_limit() -> usize {
    200
}

fn default_high_level_timeout() -> f64 {
    30.0
}

fn default_screenshot_format() -> String {
    "png".to_string()
}

fn default_pdf_paper() -> String {
    "a4".to_string()
}

fn default_pdf_scale() -> f64 {
    1.0
}

fn default_true() -> bool {
    true
}

fn default_debug_list_limit() -> usize {
    100
}

fn default_job_list_limit() -> usize {
    20
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    fn as_str(self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    fn is_running(self) -> bool {
        matches!(self, JobStatus::Running)
    }
}

/// 后台异步任务：CLI 提交后立即返回 job_id，页面操作在 daemon 内继续执行。
/// 这样智能体不必阻塞等待，用户也可以继续使用浏览器。
struct JobRecord {
    id: String,
    op: String,
    status: JobStatus,
    created_at: f64,
    started_at: Option<f64>,
    finished_at: Option<f64>,
    elapsed_ms: u64,
    result: Option<Value>,
    error: Option<Value>,
    session_key: Option<String>,
    cancel: Arc<AtomicBool>,
}

impl JobRecord {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "op": self.op,
            "status": self.status.as_str(),
            "created_at": self.created_at,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "elapsed_ms": self.elapsed_ms,
            "result": self.result,
            "error": self.error,
            "session_key": self.session_key,
        })
    }
}

#[derive(Default)]
struct JobStore {
    jobs: HashMap<String, JobRecord>,
}

/// 任务表软上限：超过它就开始按「最旧已结束」淘汰。
const JOB_STORE_SOFT_LIMIT: usize = 100;
/// 任务表硬上限：全是 Running 时也不能无限增长。
///
/// 旧实现只在 `len >= 100` 时从 `values()` 里随便删 20 条（HashMap 无序），
/// 可能删掉刚完成、正等 `job wait` 的新任务，陈旧结果反而留着；
/// 且 >=100 条全 Running 时一条也删不掉，insert 照常 → 表没有上界。
const JOB_STORE_HARD_LIMIT: usize = 500;
/// 一次常规淘汰的批量大小。
const JOB_EVICT_BATCH: usize = 20;

impl JobStore {
    /// 为即将插入的新任务腾空间。
    ///
    /// 排序键固定为 `finished_at.unwrap_or(created_at)` 升序（同值按 id 字典序），
    /// 保证「先完成、最旧」的先走，而不是取决于 HashMap 的迭代顺序。
    /// 已结束的都删完了还到硬上限时，只能强制淘汰最旧的 Running——
    /// 任务本身继续跑，只是结果再也查不到；这是表有界的代价，必须显式打日志。
    fn evict_for_insert(&mut self) {
        if self.jobs.len() < JOB_STORE_SOFT_LIMIT {
            return;
        }
        let mut finished: Vec<(f64, String)> = self
            .jobs
            .values()
            .filter(|job| !job.status.is_running())
            .map(|job| (job.finished_at.unwrap_or(job.created_at), job.id.clone()))
            .collect();
        finished.sort_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.1.cmp(&right.1))
        });
        for (_, id) in finished.into_iter().take(JOB_EVICT_BATCH) {
            self.jobs.remove(&id);
        }
        if self.jobs.len() < JOB_STORE_HARD_LIMIT {
            return;
        }
        // 走到这里说明几乎全是 Running（已结束的不足一批，或刚好都被删完）。
        // 强制淘汰：按 started_at/created_at 升序删到硬上限以下，给新任务留一个位置。
        let mut running: Vec<(f64, String)> = self
            .jobs
            .values()
            .filter(|job| job.status.is_running())
            .map(|job| (job.started_at.unwrap_or(job.created_at), job.id.clone()))
            .collect();
        running.sort_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.1.cmp(&right.1))
        });
        let mut forced = 0usize;
        for (_, id) in running {
            if self.jobs.len() < JOB_STORE_HARD_LIMIT {
                break;
            }
            self.jobs.remove(&id);
            forced += 1;
        }
        if forced > 0 {
            eprintln!(
                "job: 任务表达到硬上限 {JOB_STORE_HARD_LIMIT}，已强制淘汰 {forced} 条最旧的 running 任务（任务继续执行，结果不可再查询）"
            );
        }
    }
}

#[derive(Debug, Deserialize)]
struct JobSubmitRequest {
    op: String,
    #[serde(default)]
    args: Value,
}

#[derive(Debug, Deserialize)]
struct JobListQuery {
    #[serde(default = "default_job_list_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct RunStep {
    op: String,
    #[serde(default)]
    args: Value,
}

#[derive(Debug, Deserialize)]
struct RunRequest {
    steps: Vec<RunStep>,
    /// 某一步失败后是否终止后续步骤，默认终止。
    #[serde(default = "default_true")]
    stop_on_error: bool,
}

fn default_read_max_chars() -> usize {
    20000
}

fn default_find_limit() -> usize {
    50
}

fn default_batch_concurrency() -> usize {
    4
}

#[derive(Debug, Deserialize, Default)]
struct ReadRequest {
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    selector: Option<String>,
    #[serde(default = "default_read_max_chars")]
    max_chars: usize,
    #[serde(default)]
    links: bool,
    #[serde(default = "default_true")]
    forms: bool,
    /// 连隐藏内容一起返回（textContent 语义，不做可见性过滤）。
    #[serde(default)]
    include_hidden: bool,
    /// visible / all，等价于 --include-hidden 的显式写法。
    mode: Option<String>,
    /// 页面内文本抽取时间预算（毫秒）。
    budget_ms: Option<u64>,
    /// 单次读取最多处理多少个节点。
    max_nodes: Option<usize>,
    /// 在指定 iframe 内读取。
    frame_id: Option<i64>,
    timeout: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
struct FindRequest {
    selector: Option<String>,
    text: Option<String>,
    xpath: Option<String>,
    #[serde(default = "default_find_limit")]
    limit: usize,
    #[serde(default = "default_true")]
    visible: bool,
    #[serde(default)]
    exact: bool,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    frame_id: Option<i64>,
    timeout: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ActionRequest {
    op: String,
    target: Option<Value>,
    #[serde(default)]
    params: Value,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    frame_id: Option<i64>,
    timeout: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
struct WaitRequest {
    js: Option<String>,
    selector: Option<String>,
    text: Option<String>,
    #[serde(default = "default_wait_timeout")]
    timeout: f64,
    /// 轮询间隔（秒），与 exec 的 --wait-interval 一致。
    #[serde(default = "default_wait_interval")]
    interval: f64,
    #[serde(default)]
    visible: bool,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    frame_id: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
struct InjectRequest {
    /// run | persist | remove | list
    action: Option<String>,
    code: Option<String>,
    name: Option<String>,
    /// MAIN（默认）或 ISOLATED
    world: Option<String>,
    matches: Option<Vec<String>>,
    frame_id: Option<i64>,
    #[serde(default)]
    all_frames: bool,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BatchRequest {
    steps: Vec<RunStep>,
    /// 并发度：同时在多个标签页上执行的上限。
    #[serde(default = "default_batch_concurrency")]
    concurrency: usize,
}

fn epoch_secs() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|value| value.as_secs_f64())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default)]
struct SessionSelector {
    tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
}

impl SessionSelector {
    fn new(tab_id: Option<String>, browser: Option<String>, profile: Option<String>) -> Self {
        Self {
            tab_id,
            browser,
            profile,
        }
    }
}

pub async fn run_daemon() -> Result<()> {
    let extension_port = config::load_or_create()?.extension_port;
    let addr: SocketAddr = format!("{HOST}:{API_PORT}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let api_token = Uuid::new_v4().simple().to_string();
    config::save_api_token(&api_token)?;
    let (shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel::<()>();
    let state = AppState {
        driver: Arc::new(RwLock::new(DriverState::default())),
        execs: Arc::new(Mutex::new(crate::protocol::ExecRegistry::default())),
        jobs: Arc::new(Mutex::new(JobStore::default())),
        session_locks: Arc::new(Mutex::new(HashMap::new())),
        page_scale: Arc::new(Mutex::new(HashMap::new())),
        ws_bind_error: Arc::new(Mutex::new(None)),
        ext_reload_sent: Arc::new(Mutex::new(HashSet::new())),
        profile_label_lock: Arc::new(Mutex::new(())),
        net_persist_lock: Arc::new(Mutex::new(())),
        started_at: SystemTime::now(),
        last_activity: Arc::new(Mutex::new(Instant::now())),
        inflight: Arc::new(AtomicUsize::new(0)),
        inflight_since: Arc::new(Mutex::new(None)),
        idle_shutdown_ttl: idle_shutdown_ttl_from_env(),
        ws_security_warned: Arc::new(AtomicBool::new(false)),
        ws_connections: Arc::new(std::sync::Mutex::new(HashMap::new())),
        ws_conn_seq: Arc::new(AtomicUsize::new(0)),
        shutdown: shutdown_tx,
        sessions_ready: Arc::new(Notify::new()),
        extension_port,
        api_token,
    };

    // 读回上次落盘的 net 规则：daemon 重启后 persist 注入仍在页面/扩展侧，
    // 若不恢复内存配置，`net routes` 会读到空，表现为与实际拦截行为不一致。
    // 这里不做 fail-closed（net 规则不是安全配置），解析失败只提示并忽略。
    match config::load_net_hooks() {
        Ok(map) if !map.is_empty() => {
            state.driver.write().await.net_hooks = map;
        }
        Ok(_) => {}
        Err(err) => {
            eprintln!("net: 读取 net-hooks.json 失败，忽略持久化规则（不影响启动）: {err}");
        }
    }

    let ws_state = state.clone();
    let ws_bind_error = state.ws_bind_error.clone();
    tokio::spawn(async move {
        if let Err(err) = run_ws_server(ws_state).await {
            eprintln!("WebSocket 服务异常: {err:?}");
            // 不要只打日志：端口被占用时 API 仍然可用，若不记录状态，
            // doctor 会显示"端口已监听"（其实是别的进程），排查会非常困难。
            *ws_bind_error.lock().await = Some(err.to_string());
        }
    });

    let idle_state = state.clone();
    tokio::spawn(async move {
        monitor_idle_shutdown(idle_state).await;
    });

    let app = build_api_router(state.clone());

    println!("agent-browser-cli rust server listening on http://{addr}");
    let cleanup_state = state.clone();
    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.recv().await;
            cleanup_on_shutdown(&cleanup_state).await;
        })
        .await;
    // Keep the last token file after shutdown. The next daemon rotates it after binding the API
    // port, avoiding an exiting daemon racing with and deleting a newer daemon's token.
    serve_result?;
    Ok(())
}

fn build_api_router(state: AppState) -> Router {
    // /ping 不需要 token：CLI 用它判断 daemon 是否存活。
    // 之前复用 /health，token 文件过期时会被误判成「未启动」，进而重复拉起 daemon 并卡 15 秒。
    let public = Router::new().route("/ping", get(ping));
    let protected = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/reload-trust", post(reload_trust))
        .route("/tabs", get(tabs))
        .route("/tabtree", get(tabtree))
        .route("/lookup/tab/:tab_id", get(lookup_tab))
        .route("/lookup/browser/:browser_id", get(lookup_browser))
        .route("/lookup/profile/:profile", get(lookup_profile))
        .route("/profile-label", post(set_profile_label))
        .route("/scan", post(scan))
        .route("/exec", post(exec))
        .route("/read", post(read_page))
        .route("/inject", post(inject))
        .route("/frames", post(frames))
        .route("/downloads", post(downloads))
        .route("/net", post(net))
        .route("/browsers", get(browsers))
        .route("/broadcast", post(broadcast))
        .route("/find", post(find_page))
        .route("/action", post(action))
        .route("/wait", post(wait_page))
        .route("/challenge", post(challenge_page))
        .route("/cookies", post(cookies))
        .route("/upload", post(upload))
        .route("/open", post(open_tab))
        .route("/close", post(close))
        .route("/snapshot", post(snapshot))
        .route("/click", post(click))
        .route("/fill", post(fill))
        .route("/send-keys", post(send_keys))
        .route("/mouse-click", post(mouse_click))
        .route("/screenshot", post(screenshot))
        .route("/save-pdf", post(save_pdf))
        .route("/network/start", post(network_start))
        .route("/network/list", post(network_list))
        .route("/network/detail", post(network_detail))
        .route("/network/clear", post(network_clear))
        .route("/network/stop", post(network_stop))
        .route("/console/start", post(console_start))
        .route("/console/list", post(console_list))
        .route("/console/clear", post(console_clear))
        .route("/console/stop", post(console_stop))
        .route("/job/submit", post(job_submit))
        .route("/job/get/:job_id", get(job_get))
        .route("/job/list", get(job_list))
        .route("/job/cancel/:job_id", post(job_cancel))
        .route("/run", post(run_steps))
        .route("/batch", post(batch_steps))
        .route("/shutdown", post(shutdown))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_api_access,
        ))
        // 放在 `require_api_access` 之后 = 更外层：401/403 也算「在途」，
        // 且请求结束时统一续期 `last_activity`（长请求靠这条续期，否则会被空闲自杀腰斩）。
        .layer(middleware::from_fn_with_state(
            state.clone(),
            track_inflight,
        ));
    public.merge(protected).with_state(state)
}

/// 请求离开时必定执行的减计数守卫：即便 handler panic（栈展开会 drop）或被取消，
/// 也不会漏减；否则 `inflight` 泄漏会让 daemon 永远不进入空闲自杀。
struct InflightGuard {
    inflight: Arc<AtomicUsize>,
    since: Arc<Mutex<Option<Instant>>>,
}

impl InflightGuard {
    /// 计数 +1 与守卫创建必须是同一步：若先递增、再 `.await` 取锁，请求在取锁期间
    /// 被取消，就只留下「计数 +1、守卫从未创建」的泄漏，daemon 再也不会空闲退出。
    /// 返回值第二项表示这次是否是 0→1（调用方据此记录卡死判定起点）。
    fn acquire(state: &AppState) -> (Self, bool) {
        let first = state.inflight.fetch_add(1, Ordering::Relaxed) == 0;
        (
            Self {
                inflight: state.inflight.clone(),
                since: state.inflight_since.clone(),
            },
            first,
        )
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        if self.inflight.fetch_sub(1, Ordering::Relaxed) == 1 {
            // 回到 0：清空起点。Drop 里不能 await，用 try_lock 快速路径；
            // 抢不到锁（有人正同时改写）就留着旧值——下一个 0→1 会覆盖它，
            // 而只有计数 0→1 的那一刻才会记录起点，所以不会误判。
            if let Ok(mut slot) = self.since.try_lock() {
                *slot = None;
            }
        }
    }
}

async fn track_inflight(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    // 0→1 时记录「最早一条在途请求」的起点，供空闲自杀判断它是否已经卡死。
    // 先完成「计数 +1 + 构造守卫」，再 await 锁：即使请求在取锁期间被取消，
    // 守卫也已在栈上，Drop 会把计数减回去。
    let (_guard, first) = InflightGuard::acquire(&state);
    if first {
        *state.inflight_since.lock().await = Some(Instant::now());
    }
    // 必须先把响应接住再收尾，不能 early return。
    let response = next.run(request).await;
    // 续期：请求「结束」时刷新活跃时间。请求进行中由 `inflight` 挡住空闲自杀，
    // 这里保证结束后空闲计时从头算，不会刚跑完长命令就立刻自退。
    *state.last_activity.lock().await = Instant::now();
    response
}

async fn ping(State(state): State<AppState>) -> Json<Value> {
    let ws_bind_error = state.ws_bind_error.lock().await.clone();
    Json(json!({
        "ok": true,
        "running": true,
        "pid": std::process::id(),
        "protocol": 2,
        "ws_bind_error": ws_bind_error
    }))
}

async fn require_api_access(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if let Err((status, code, message)) = validate_api_access(request.headers(), &state.api_token) {
        return (
            status,
            Json(json!({ "ok": false, "error": message, "error_code": code })),
        )
            .into_response();
    }
    next.run(request).await
}

/// 恒定时间字符串比较：只用于 token 校验。
///
/// 防的是本地进程通过响应时间逐字节猜 token 的时间侧信道，**不是**强 token
/// 的替代品：真正的防线仍是 128-bit 随机 token + 只监听 127.0.0.1。
/// 长度不同会提前返回——长度本身不是秘密（token 长度固定），这样还能保持 O(1)。
fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    // `black_box` 不在这里用：diff 已经参与最终比较，编译器无法把它折叠成
    // 「发现不同就早退」的短路径（没有数据依赖分支）。
    diff == 0
}

fn validate_api_access(
    headers: &HeaderMap,
    expected_token: &str,
) -> std::result::Result<(), (StatusCode, &'static str, &'static str)> {
    if let Some(origin) = headers.get(ORIGIN) {
        let allowed = origin
            .to_str()
            .map(|value| matches!(value, LOOPBACK_API_ORIGIN | LOCALHOST_API_ORIGIN))
            .unwrap_or(false);
        if !allowed {
            return Err((
                StatusCode::FORBIDDEN,
                "origin_not_allowed",
                "Browser origins are not allowed to access the local API",
            ));
        }
    }
    let authorized = headers
        .get(config::API_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| constant_time_eq(value, expected_token));
    if !authorized {
        return Err((
            StatusCode::UNAUTHORIZED,
            "invalid_api_token",
            "A valid API token is required",
        ));
    }
    Ok(())
}

fn build_ws_router(state: AppState) -> Router {
    Router::new().route("/", get(ws_handler)).with_state(state)
}

async fn run_ws_server(state: AppState) -> Result<()> {
    let extension_port = state.extension_port;
    let app = build_ws_router(state);
    let addr: SocketAddr = format!("{HOST}:{extension_port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("WebSocket server running on ws://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// WebSocket 信任配置：`<用户配置目录>/ws-trust.json`。
///
/// 默认不存在 = 兼容模式（任何 Chrome 扩展都能连）。用户填了 `extension_ids`
/// 或 `token` 之后才真正收紧。每次握手都重新读文件，CLI 改完无需重启 daemon。
#[derive(Debug, Default, Deserialize)]
struct WsTrust {
    #[serde(default)]
    extension_ids: Vec<String>,
    #[serde(default)]
    token: Option<String>,
}

impl WsTrust {
    fn token_value(&self) -> Option<&str> {
        self.token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
    }

    fn enforces_identity(&self) -> bool {
        !self.extension_ids.is_empty() || self.token_value().is_some()
    }
}

/// 白名单是否放行某个扩展 id。空白名单 = 兼容模式，一律放行。
/// 握手与 `/reload-trust` 共用这一处判断，避免两边对「大小写/空白」的处理跑偏。
fn ws_trust_allows(trust: &WsTrust, extension_id: &str) -> bool {
    trust.extension_ids.is_empty()
        || trust
            .extension_ids
            .iter()
            .any(|item| item.trim().eq_ignore_ascii_case(extension_id))
}

/// `/reload-trust` 的判定核心：`Some(reason)` = 撤销，`None` = 保留。
///
/// 判定顺序固定为「损坏 → 白名单 → token」，理由如下：
/// 1. 配置损坏必须 fail-closed，先于任何"看起来合法"的分支，绝不能让损坏变成放行；
/// 2. 白名单是最粗的那道门，被移出名单的连接连比对 token 都不必做；
/// 3. 最后比 token：当前配置**有** token 时，连接的指纹必须等于当前 token 的指纹。
///    连接指纹为 `None` 也要撤销——它是在"还没配 token"时连上的，现在配置收紧了就该断。
///    （这是 §4.1：token-only 配置以前恒不踢人，等于"加了 token 也没锁死"。）
///
/// `reason` 取值：`corrupt_config` / `untrusted_id` / `token_mismatch`，仅用于回包与文案。
fn ws_trust_revoke_reason(
    trust: &WsTrustState,
    extension_id: &str,
    conn_token_fp: Option<u64>,
) -> Option<&'static str> {
    match trust {
        // 配置损坏 = fail-closed，与握手侧一致：撤销所有连接。
        WsTrustState::Corrupt(_) => Some("corrupt_config"),
        WsTrustState::Ok(trust) => {
            if !ws_trust_allows(trust, extension_id) {
                return Some("untrusted_id");
            }
            // 配置没 token（含从有改回无）时不复核指纹：那是回到兼容模式，
            // 已建立的老连接不该因为"少了把锁"被踢。
            if let Some(expected) = trust.token_value() {
                if conn_token_fp != Some(token_fingerprint(expected)) {
                    return Some("token_mismatch");
                }
            }
            None
        }
    }
}

fn ws_trust_should_revoke(
    trust: &WsTrustState,
    extension_id: &str,
    conn_token_fp: Option<u64>,
) -> bool {
    ws_trust_revoke_reason(trust, extension_id, conn_token_fp).is_some()
}

fn ws_trust_path() -> Option<PathBuf> {
    config::user_config_dir()
        .ok()
        .map(|dir| dir.join("ws-trust.json"))
}

/// `ws-trust.json` 的读取结果。
///
/// `Corrupt` 是 fail-closed 的落点：文件存在但读不懂时**不能**当成「从没配过」。
/// 否则用户在配置里多写一个尾逗号，整套白名单就会静默失效（daemon 在后台，
/// 那行 stderr 也没人看得见），等于「越写配置越不安全」。
#[derive(Debug)]
enum WsTrustState {
    /// 正常读到配置；`default()` 表示从没配过 = 兼容模式。
    Ok(WsTrust),
    /// 文件存在但读取/解析失败，细节用于报错文案。
    Corrupt(String),
}

/// 纯解析：只负责把 JSON 文本变成状态，便于单测覆盖「坏 JSON 不能退化成兼容模式」。
///
/// 空/纯空白同样算 `Corrupt`（§4.4）：写文件是原子 rename，正常不会出现空文件，
/// 一旦出现只可能是手工 `> ws-trust.json` 或编辑器保存中断。若把它当成"未配置"，
/// 用户会**静默**回到「任意扩展可连」——配置越写越不安全。回到兼容模式的正确做法
/// 是删除该文件（`NotFound` 那条分支），而不是清空内容。
fn parse_ws_trust(raw: &str) -> WsTrustState {
    if raw.trim().is_empty() {
        return WsTrustState::Corrupt(
            "配置为空：空文件不会被视为「未配置」，删除该文件才会回到兼容模式".to_string(),
        );
    }
    match serde_json::from_str::<WsTrust>(raw) {
        Ok(trust) => WsTrustState::Ok(trust),
        Err(err) => WsTrustState::Corrupt(err.to_string()),
    }
}

fn load_ws_trust() -> WsTrustState {
    let Some(path) = ws_trust_path() else {
        return WsTrustState::Ok(WsTrust::default());
    };
    // 用带备份回退的读取：Windows 的原子替换是「rename(target, .bak) → rename(temp, target)」
    // 三步，进程在两步之间被杀时目标文件暂时不存在。裸 `read_to_string` 会把这种
    // 情况判成「从没配过」→ 退回兼容模式（fail-open）；`read_with_backup` 会读 `.bak`。
    let content = match config::read_with_backup(&path) {
        Ok(content) => content,
        // 目标与 .bak 都不存在 = 从没配过，兼容模式（有意保留）。
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return WsTrustState::Ok(WsTrust::default());
        }
        // 存在但读不了（权限/编码/路径是个目录……）同样按损坏处理：
        // 用户显然配了东西，此时退回兼容模式是一样的静默降级。
        Err(err) => return WsTrustState::Corrupt(format!("读取失败: {err}")),
    };
    // 空内容不在这里短路：交给 `parse_ws_trust` 判成 `Corrupt`（§4.4）。
    parse_ws_trust(&content)
}

/// 从 Origin 头里取出合法的 Chrome 扩展 ID（32 位 a-p）。
fn extension_id_from_origin(headers: &HeaderMap) -> Option<String> {
    let origin = headers.get(ORIGIN)?.to_str().ok()?;
    let extension_id = origin.strip_prefix(CHROME_EXTENSION_ORIGIN_PREFIX)?;
    if extension_id.len() == 32 && extension_id.bytes().all(|byte| matches!(byte, b'a'..=b'p')) {
        Some(extension_id.to_string())
    } else {
        None
    }
}

/// 仅作为「Origin 是否是合法扩展」的谓词保留（单元测试用途）；
/// 生产握手路径用 `extension_id_from_origin`，因为它还要拿 id 做白名单比对。
#[cfg(test)]
fn is_allowed_extension_origin(headers: &HeaderMap) -> bool {
    extension_id_from_origin(headers).is_some()
}

/// 从 query string 里取一个参数（不引入额外依赖）。
fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        if name != key {
            return None;
        }
        Some(percent_decode(value))
    })
}

/// 百分号解码（query string 语义）。
///
/// **不**把裸 `+` 当空格：`+` 只在 `application/x-www-form-urlencoded` 的
/// **请求体**里有空格含义，query 里的 `+` 是合法字面字符。以前这里把 `+`
/// 解成空格，token 里带 `+` 且客户端没有 `encodeURIComponent` 时握手恒失败——
/// 表现是「token 明明抄对了却 401」。真正的空格请用 `%20`。
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    None => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

async fn ws_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    ws: WebSocketUpgrade,
) -> Response {
    // 1) 必须来自 Chrome 扩展。这一条保持原样：没有 Origin 的网页/本地进程一律拒绝。
    let Some(extension_id) = extension_id_from_origin(&headers) else {
        return (
            StatusCode::FORBIDDEN,
            "WebSocket connections require a Chrome extension Origin",
        )
            .into_response();
    };
    let trust = match load_ws_trust() {
        WsTrustState::Ok(trust) => trust,
        // 配置损坏 = fail-closed：拒绝握手，绝不静默退回「任意扩展可连」。
        WsTrustState::Corrupt(detail) => {
            let path = ws_trust_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<用户配置目录>/ws-trust.json".to_string());
            eprintln!(
                "WebSocket 信任配置损坏，已拒绝所有握手（fail-closed，不回退到兼容模式）: {path} ({detail})"
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "WebSocket 信任配置损坏，已拒绝所有连接：为避免静默降级成「任意扩展可连」，daemon 不会把它当作未配置。配置文件：{path}；解析错误：{detail}。修复：删除该文件（回到兼容模式），或运行 agent-browser-cli trust-extension --clear 重建（agent-browser-cli trust-extension --list 会显示同一处错误）"
                ),
            )
                .into_response();
        }
    };
    // 2) 白名单：仅在配置了 extension_ids 时生效（空白名单 = 兼容模式，放行）。
    if !ws_trust_allows(&trust, &extension_id) {
        let path = ws_trust_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<用户配置目录>/ws-trust.json".to_string());
        return (
            StatusCode::FORBIDDEN,
            format!(
                "扩展未信任：把 {extension_id} 加入 {path} 的 extension_ids，或运行 agent-browser-cli trust-extension {extension_id}"
            ),
        )
            .into_response();
    }
    // 3) token：仅在配置了 token 时生效，从 `?token=` 取。
    //
    // 无论配没配都记下"这次实际提供了什么"，登记连接时用它的指纹：
    // 配置后来才收紧（白名单空 + token 有值）时，`/reload-trust` 靠这份指纹
    // 把"当初没带 token 就进来了"的连接踢掉（§4.1）。
    let provided_token = query_param(uri.query(), "token");
    if let Some(expected) = trust.token_value() {
        // 恒定时间比较，避免本地进程用响应时间侧信道逐字节猜 token。
        if !provided_token
            .as_deref()
            .is_some_and(|provided| constant_time_eq(provided, expected))
        {
            return (
                StatusCode::UNAUTHORIZED,
                "WebSocket token 无效：请在扩展里填写与 ws-trust.json 中 token 一致的值",
            )
                .into_response();
        }
    }
    // 4) 两者都没配 = 兼容模式。每个进程只提醒一次，别在握手热路径刷屏。
    if !trust.enforces_identity() && !state.ws_security_warned.swap(true, Ordering::Relaxed) {
        eprintln!(
            "WebSocket 未启用身份校验，任意 Chrome 扩展均可连接；可用 agent-browser-cli trust-extension <id> 加固"
        );
    }
    // 只留指纹进登记表，明文 token 不落地。
    let conn_token_fp = provided_token.as_deref().map(token_fingerprint);
    ws.on_upgrade(move |socket| handle_socket(socket, state, extension_id, conn_token_fp))
        .into_response()
}

/// 撤销信号的轮询间隔。信任配置变更极少发生，所以用轮询而不是全局条件变量：
/// 连接循环里只读一个 `AtomicBool`，不碰登记表锁（热路径无锁）。
const WS_REVOKE_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 登记表加锁的唯一入口：即使某次持锁 panic 导致中毒，也继续用里面的数据，
/// 不能让「中毒」把撤销能力也一起弄坏（fail-closed 优先）。
fn ws_connections_lock(state: &AppState) -> std::sync::MutexGuard<'_, HashMap<u64, WsConn>> {
    state
        .ws_connections
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 连接注销守卫。`handle_socket` 无论正常 break、提前 return 还是 panic 展开，
/// `Drop` 都会把这次连接从登记表里删掉——漏删会让登记表无限增长，
/// 而且 `/reload-trust` 会对着早已死掉的连接反复置撤销位。
struct WsConnGuard {
    state: AppState,
    conn_id: u64,
}

impl Drop for WsConnGuard {
    fn drop(&mut self) {
        // 全程无 await（std Mutex），所以能在 Drop 里安全完成。
        ws_connections_lock(&self.state).remove(&self.conn_id);
    }
}

/// 空闲自杀阈值：默认 300s，`AGENT_BROWSER_CLI_IDLE_TTL=<秒>` 可覆盖（0 或非法值回落默认）。
fn idle_shutdown_ttl_from_env() -> Duration {
    std::env::var(IDLE_TTL_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(IDLE_SHUTDOWN_TTL)
}

/// 在途请求「卡死」判定的下限（N3-07）。
///
/// `inflight > 0` 会阻止空闲自杀，这本身是对的（否则长命令被腰斩）；但如果某个请求
/// 永远挂着（`download-wait --timeout 3600` 遇上扩展失联），daemon 就再也不会退出，
/// 端口和浏览器里的“已连接”浮层一直留着。取 1800s 起步：正常的长命令（run/batch/
/// broadcast）分钟级就结束，半小时还在跑的只可能是扩展已经丢了。
const MAX_INFLIGHT_STALL: Duration = Duration::from_secs(1800);

/// 卡死阈值 = max(2×空闲 TTL, 1800s)。
///
/// - TTL 配小（默认 300s）时用 1800s 兜底，别把正常的十分钟长命令误判成卡死；
/// - TTL 配大（比如 2h）时跟着放大到 4h，保证「卡死」一定晚于「正常空闲」。
fn max_inflight_stall(state: &AppState) -> Duration {
    state
        .idle_shutdown_ttl
        .saturating_mul(2)
        .max(MAX_INFLIGHT_STALL)
}

async fn monitor_idle_shutdown(state: AppState) {
    loop {
        tokio::time::sleep(IDLE_SHUTDOWN_CHECK_INTERVAL).await;
        // 有请求正在处理时不允许退出：`download-wait --timeout 600`、`run`、`batch`
        // 这类命令整个生命周期只有一次请求，`last_activity` 期间不会变，
        // 只看时间会把它们连同 cleanup 一起腰斩。
        //
        // 但「在途」也可能是永久挂住：扩展失联时 `download-wait --timeout 3600`
        // 会让 inflight 永远 >0，daemon 再也退不出去。超过阈值就当作卡死放行，
        // 并明确打一行日志（含已挂时长），免得用户以为 daemon 行为诡异。
        if state.inflight.load(Ordering::Relaxed) > 0 {
            let since = *state.inflight_since.lock().await;
            let stalled_for = match since {
                // 起点丢失（极端竞态）时保守处理：不算卡死，宁可多等一轮。
                None => Duration::ZERO,
                Some(since) => since.elapsed(),
            };
            let stall_limit = max_inflight_stall(&state);
            if stalled_for < stall_limit {
                continue;
            }
            eprintln!(
                "在途请求已挂起 {}s（阈值 {}s），判定为卡死，daemon 直接退出（inflight={}）",
                stalled_for.as_secs(),
                stall_limit.as_secs(),
                state.inflight.load(Ordering::Relaxed)
            );
            // 不能落入下面的 idle 判定：其它请求会不断刷新 `last_activity`，
            // 那样只会每 5s 重打一条「允许退出」却永远不退出。
            cleanup_on_shutdown(&state).await;
            let _ = state.shutdown.send(());
            break;
        }
        // 后台异步任务运行期间不允许退出，否则用户提交的长时间任务会被腰斩。
        // 但 job 也可能卡死（页面脚本不返回、扩展失联），此时只看「有 running job」
        // 会让 daemon 永不退出——HTTP 在途有 1800s 兜底，job 也要有对称的保护。
        let (running_jobs, oldest_running) = running_jobs_snapshot(&state).await;
        if running_jobs > 0 {
            if let Some(stalled_for) = oldest_running {
                let stall_limit = max_inflight_stall(&state);
                if stalled_for >= stall_limit {
                    eprintln!(
                        "后台 job 已运行 {}s（阈值 {}s，running={}），判定为卡死，daemon 直接退出",
                        stalled_for.as_secs(),
                        stall_limit.as_secs(),
                        running_jobs
                    );
                    cleanup_on_shutdown(&state).await;
                    let _ = state.shutdown.send(());
                    break;
                }
            }
            continue;
        }
        let idle_for = state.last_activity.lock().await.elapsed();
        if idle_for >= state.idle_shutdown_ttl {
            eprintln!(
                "agent-browser-cli daemon idle for {}s, shutting down",
                idle_for.as_secs()
            );
            cleanup_on_shutdown(&state).await;
            let _ = state.shutdown.send(());
            break;
        }
    }
}

/// 运行中的 job 数，以及最老一条已运行多久（没有 running job 时返回 None）。
///
/// 用 `started_at.unwrap_or(created_at)`：job 从提交到真正开跑之间 status 已经是
/// Running，但 started_at 还没写，此时按 created_at 计不会把它误判成刚启动。
async fn running_jobs_snapshot(state: &AppState) -> (usize, Option<Duration>) {
    let store = state.jobs.lock().await;
    let now = epoch_secs();
    let mut count = 0usize;
    let mut oldest = f64::INFINITY;
    for job in store.jobs.values().filter(|job| job.status.is_running()) {
        count += 1;
        oldest = oldest.min(job.started_at.unwrap_or(job.created_at));
    }
    let age =
        (count > 0 && oldest.is_finite()).then(|| Duration::from_secs_f64((now - oldest).max(0.0)));
    (count, age)
}

async fn handle_socket(
    mut socket: WebSocket,
    state: AppState,
    extension_id: String,
    conn_token_fp: Option<u64>,
) {
    // 登记这条连接（N3-04）：`/reload-trust` 通过它把「握手时合法、之后被移出白名单
    // 或 token 收紧」的连接真正断开，而不是只在下次握手时才生效。
    let conn_id = state.ws_conn_seq.fetch_add(1, Ordering::Relaxed) as u64;
    let revoked = Arc::new(AtomicBool::new(false));
    ws_connections_lock(&state).insert(
        conn_id,
        WsConn {
            extension_id: extension_id.clone(),
            token_fingerprint: conn_token_fp,
            revoked: revoked.clone(),
        },
    );
    // 注销靠 Drop：正常 break / 提前 return / panic 都不会漏。
    let _conn_guard = WsConnGuard {
        state: state.clone(),
        conn_id,
    };
    // 撤销检查走独立定时分支：连接里只读一个 AtomicBool，不查询全局登记表。
    let mut revoke_tick = tokio::time::interval(WS_REVOKE_POLL_INTERVAL);
    // 首次 tick 立刻完成，等价于「进来就查一次」，不影响后续节奏。
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let mut registered_ids: Vec<String> = Vec::new();
    loop {
        tokio::select! {
            Some(outgoing) = rx.recv() => {
                if socket.send(Message::Text(outgoing)).await.is_err() {
                    break;
                }
            }
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break; };
                let Message::Text(text) = msg else { continue; };
                if text.trim() == r#"{"type":"ping"}"# {
                    continue;
                }
                match serde_json::from_str::<WsIncoming>(&text) {
                    Ok(incoming) => handle_ws_message(incoming, &state, tx.clone(), &mut registered_ids).await,
                    Err(err) => eprintln!("WebSocket 消息解析失败: {err}: {text}"),
                }
            }
            _ = revoke_tick.tick() => {
                if revoked.load(Ordering::Relaxed) {
                    // 撤销原因可能是白名单也可能是 token（见 `ws_trust_revoke_reason`），
                    // 这里只说"不再被信任"，具体原因由 `/reload-trust` 回包给出。
                    eprintln!(
                        "扩展 {extension_id} 已不再满足当前信任配置，断开其现有连接（connection_id={conn_id}）"
                    );
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    }
    // 只处理「确实属于这条连接」的会话：扩展重载时新旧连接会短暂并存，
    // 旧连接晚一步收尾不能把新连接刚注册的会话标成断开再删掉。
    let owned: Vec<String> = {
        let mut driver = state.driver.write().await;
        let mut owned = Vec::new();
        for id in &registered_ids {
            if let Some(session) = driver.sessions.get_mut(id) {
                if !session.sender.same_channel(&tx) {
                    continue;
                }
                session.disconnected_at = Some(Instant::now());
                owned.push(id.clone());
            }
        }
        owned
    };
    // 扩展断开时，挂在这些会话上的命令不会再有结果。
    // 立即以 extension_disconnected 失败，别让 CLI 干等到 15~60 秒超时。
    {
        let mut execs = state.execs.lock().await;
        for session_key in owned.iter() {
            for pending in execs.take_session(session_key) {
                let _ = pending.tx.send(Err(anyhow!(
                    "extension_disconnected: 浏览器扩展连接已断开，命令未完成（daemon 可能重启或插件被重载）"
                )));
            }
        }
    }
    // 连接已断 = 会话确实销毁，会话级缓存（snapshots / page_scale / session_locks）
    // 一并回收；扩展重连会重新 ExtReady 注册，代价只是重新探测一次页面规模。
    for session_key in owned.iter() {
        forget_session(&state, session_key).await;
    }
}

fn should_set_default_session(driver: &DriverState, session_key: &str) -> bool {
    match driver.preferred_default_session_key.as_deref() {
        Some(preferred) => preferred == session_key,
        None => driver.default_session_key.is_none(),
    }
}

async fn handle_ws_message(
    incoming: WsIncoming,
    state: &AppState,
    sender: mpsc::UnboundedSender<String>,
    registered_ids: &mut Vec<String>,
) {
    // 结果 / ACK 只碰「在途执行表」，不再抢全局 driver 锁：
    // 每个命令的回包都会走到这里，之前它和会话读写共用一把锁。
    match &incoming {
        WsIncoming::Ack { id } => {
            let mut execs = state.execs.lock().await;
            // 只有仍然在途的请求才记 ACK：迟到的 / 重复的 ACK 以前会永久留在
            // `acked` 里（只有 Result/Error/超时 才 take），长跑 daemon 上只增不减。
            // 加了这层判断后集合规模天然被 pending 数量约束。
            if execs.pending.contains_key(id) {
                // 兜底上限：`acked` 无序，直接整体清空最省事；它只影响超时文案的
                // 「是否收到过 ACK」提示，清空不会让任何请求提前失败。
                if execs.acked.len() >= ACK_TRACK_LIMIT {
                    eprintln!("ack 跟踪表超过 {ACK_TRACK_LIMIT} 条，已重置");
                    execs.acked.clear();
                }
                execs.acked.insert(id.clone());
                if let Some(pending) = execs.pending.get_mut(id) {
                    pending.delivered_at = Some(Instant::now());
                }
            }
            return;
        }
        WsIncoming::Result {
            id,
            result,
            new_tabs,
        } => {
            let pending = state.execs.lock().await.take(id);
            if let Some(pending) = pending {
                let _ = pending.tx.send(Ok(ExecResult {
                    data: Some(result.clone()),
                    result: None,
                    closed: None,
                    new_tabs: new_tabs.clone(),
                }));
            }
            return;
        }
        WsIncoming::Error {
            id,
            error,
            new_tabs,
        } => {
            let pending = state.execs.lock().await.take(id);
            if let Some(pending) = pending {
                let mut value = json!({ "error": error.clone() });
                if let Some(tabs) = new_tabs.clone() {
                    value["newTabs"] = tabs;
                }
                let _ = pending.tx.send(Err(anyhow!(value.to_string())));
            }
            return;
        }
        _ => {}
    }

    let mut driver = state.driver.write().await;
    match incoming {
        WsIncoming::ExtReady {
            browser_id,
            profile_id,
            profile_label,
            extension_version,
            file_scheme_access,
            tabs,
        }
        | WsIncoming::TabsUpdate {
            browser_id,
            profile_id,
            profile_label,
            extension_version,
            file_scheme_access,
            tabs,
        } => {
            let first_tab_id = tabs
                .first()
                .map(|t| t.id.to_string().trim_matches('"').to_string());
            let current: std::collections::HashSet<String> = tabs
                .iter()
                .map(|t| {
                    let tab_id = t.id.to_string().trim_matches('"').to_string();
                    crate::protocol::make_session_key(&browser_id, &profile_id, &tab_id)
                })
                .collect();
            for session in driver.sessions.values_mut() {
                if session.info.tab_type == "ext_ws"
                    && session.browser_id == browser_id
                    && session.profile_id == profile_id
                    && !current.contains(&session.session_key)
                {
                    session.disconnected_at = Some(Instant::now());
                }
            }
            for tab in tabs {
                let mut info = tab.into_tab_info(&browser_id, &profile_id, profile_label.clone());
                let session_key = info.session_key.clone();
                let tab_id = info.tab_id.clone();
                // 首次出现才记连接时间；已存在的会话（同一标签的 tabs_update / 重连）
                // 保留原值，否则看不出真实连接时长。
                info.connected_at = Some(
                    driver
                        .sessions
                        .get(&session_key)
                        .and_then(|existing| existing.info.connected_at)
                        .unwrap_or_else(epoch_secs),
                );
                if !registered_ids.contains(&session_key) {
                    registered_ids.push(session_key.clone());
                }
                driver.latest_session_key = Some(session_key.clone());
                if should_set_default_session(&driver, &session_key) {
                    driver.default_session_key = Some(session_key.clone());
                    if driver.preferred_default_session_key.as_deref() == Some(&session_key) {
                        driver.preferred_default_session_key = None;
                    }
                }
                driver.sessions.insert(
                    session_key.clone(),
                    Session {
                        session_key,
                        tab_id,
                        browser_id: browser_id.clone(),
                        profile_id: profile_id.clone(),
                        profile_label: profile_label.clone(),
                        extension_version: extension_version.clone(),
                        file_scheme_access,
                        info,
                        sender: sender.clone(),
                        disconnected_at: None,
                    },
                );
            }
            let pruned = prune_sessions(&mut driver);
            let alive: Vec<String> = if pruned.is_empty() {
                Vec::new()
            } else {
                driver.sessions.keys().cloned().collect()
            };
            // 锁序约定是 driver → execs，且不能在持 driver 写锁时 await 别的锁，
            // 所以先放掉 driver 再清理在途执行与会话级缓存。
            drop(driver);
            if !pruned.is_empty() {
                state.execs.lock().await.retain_sessions(&alive);
                for stale_key in pruned.iter() {
                    forget_session(state, stale_key).await;
                }
            }
            state.sessions_ready.notify_waiters();
            // 版本落后时主动让浏览器重载扩展：Chrome 只执行加载时那份 background.js，
            // 不重载就会出现「代码已更新、报错照旧」。
            if let Some(version) = extension_version.as_deref() {
                if !version_at_least(version, bundled_extension_version()) {
                    request_extension_reload(
                        state,
                        &browser_id,
                        version,
                        first_tab_id,
                        sender.clone(),
                    )
                    .await;
                }
            }
        }
        // Ack / Result / Error 已在函数开头用独立的 execs 锁处理。
        _ => {}
    }
}

async fn root() -> &'static str {
    "agent-browser-cli"
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let active_tabs_count = active_tabs(&state, false).await.len();
    let extension_connected = has_extension_connection(&state).await;
    let profile_capabilities = extension_profile_capabilities(&state).await;
    let extension_freshness = extension_freshness(&profile_capabilities);
    let ready = extension_connected && active_tabs_count > 0;
    let uptime = state
        .started_at
        .elapsed()
        .map(|d| d.as_secs_f64())
        .unwrap_or_default();
    let idle_for = state.last_activity.lock().await.elapsed().as_secs_f64();
    let (jobs_running, jobs_total) = {
        let store = state.jobs.lock().await;
        (
            store
                .jobs
                .values()
                .filter(|job| job.status.is_running())
                .count(),
            store.jobs.len(),
        )
    };
    let ttl = state.idle_shutdown_ttl.as_secs_f64();
    // 含本次 /health 请求本身（`track_inflight` 在进入前已 +1）。
    let inflight = state.inflight.load(Ordering::Relaxed);
    // 信任配置只做「报告」，损坏也不能让 /health 自己失败：
    // 否则用户失去唯一能看到「为什么连不上」的入口。
    let ws_security = match load_ws_trust() {
        WsTrustState::Ok(ws_trust) => json!({
            "trusted_ids": ws_trust.extension_ids.len(),
            "token_required": ws_trust.token_value().is_some(),
            "trust_config_error": Value::Null,
        }),
        // 数字不伪造：此时白名单根本没读出来，0/false 只是「未知」的占位，
        // 真实原因看 `trust_config_error`（非 null 即代表握手正在被拒绝）。
        WsTrustState::Corrupt(detail) => json!({
            "trusted_ids": 0,
            "token_required": false,
            "trust_config_error": detail,
        }),
    };
    let configured_extension_port = config::load_existing()
        .map(|config| config.extension_port)
        .unwrap_or(state.extension_port);
    let ws_bind_error = state.ws_bind_error.lock().await.clone();
    Json(json!({
        "ok": true,
        "running": true,
        "ready": ready,
        "ports": {
            "api": API_PORT,
            "extension": {
                "configured": configured_extension_port,
                "listening": state.extension_port,
                "matched": configured_extension_port == state.extension_port,
                "bind_error": ws_bind_error
            }
        },
        "connection": {
            "extension_connected": extension_connected,
            "active_tabs": active_tabs_count,
            "profiles": profile_capabilities
        },
        "extension": extension_freshness,
        "uptime": uptime,
        "idle_for": idle_for,
        "ttl": ttl,
        "ttl_remaining": (ttl - idle_for).max(0.0),
        "inflight": inflight,
        "ws_security": ws_security,
        "jobs": {
            "running": jobs_running,
            "total": jobs_total
        }
    }))
}

/// `POST /reload-trust`：让 `ws-trust.json` 的变更立刻作用于**已建立**的连接。
///
/// 握手阶段每次都会重读配置，但已经连上的扩展不会因为文件变化而受影响：
/// 用户跑 `trust-extension <自己的 id>` 加固后，此前连上的可疑扩展仍然能收命令。
/// 这个端点补上那一步：重读配置，把不再可信的连接置撤销位（500ms 内收到 Close）。
///
/// 规则：
/// - 白名单非空且不含该 `extension_id` → 撤销（`untrusted_id`）；
/// - 当前配置有 token → 连接的 token 指纹必须匹配，否则撤销（`token_mismatch`）；
/// - 白名单为空且无 token = 兼容模式，不踢人（否则用户刚清空白名单，所有连接会瞬间全断）；
/// - 配置损坏 → fail-closed，撤销全部（`corrupt_config`，与握手侧一致）。
///
/// 鉴权跟随其它 API：注册在 `require_api_access` 层内，需要 `X-Agent-Browser-Token`。
async fn reload_trust(State(state): State<AppState>) -> Json<Value> {
    let trust = load_ws_trust();
    let (revoked, checked): (Vec<Value>, usize) = {
        // 只在这一次同步加锁：遍历登记表并把撤销位置位，回调里不做任何 await。
        let conns = ws_connections_lock(&state);
        let checked = conns.len();
        let mut revoked: Vec<Value> = Vec::new();
        for (conn_id, conn) in conns.iter() {
            // 生产路径就走这条被单测覆盖的谓词，避免"测的东西"和"跑的东西"分叉。
            if !ws_trust_should_revoke(&trust, &conn.extension_id, conn.token_fingerprint) {
                continue;
            }
            // 只有真要撤销时才取一次原因（纯函数，与上面的判定同源，不会打架）。
            let reason = ws_trust_revoke_reason(&trust, &conn.extension_id, conn.token_fingerprint)
                .unwrap_or("untrusted_id");
            // swap 的返回值去重：已经撤销过的连接不再重复上报。
            if !conn.revoked.swap(true, Ordering::Relaxed) {
                revoked.push(json!({
                    "extension_id": conn.extension_id,
                    "connection_id": conn_id,
                    "reason": reason,
                }));
            }
        }
        (revoked, checked)
    };
    let error = match &trust {
        WsTrustState::Corrupt(detail) => Some(detail.clone()),
        WsTrustState::Ok(_) => None,
    };
    match error {
        Some(err) => Json(json!({
            "ok": false,
            "error": err,
            "revoked": revoked,
            "checked": checked,
        })),
        None => Json(json!({
            "ok": true,
            "revoked": revoked,
            "checked": checked,
        })),
    }
}

async fn tabs(State(state): State<AppState>, Query(query): Query<TabsQuery>) -> Json<Value> {
    touch(&state).await;
    // 只读命令：浏览器没开时不该白等 5 秒。
    wait_for_sessions(&state, WAIT_SESSIONS_READ).await;
    match active_tabs_filtered(
        &state,
        false,
        query.browser.as_deref(),
        query.profile.as_deref(),
    )
    .await
    {
        Ok(tabs) => {
            let items: Vec<Value> = tabs
                .iter()
                .map(|tab| tabs_display_json(tab, query.full))
                .collect();
            Json(
                json!({ "ok": true, "result": { "status": "success", "metadata": { "tabs_count": items.len(), "tabs": items } } }),
            )
        }
        Err(err) => Json(json!({ "ok": false, "error": err.to_string() })),
    }
}

/// 展示层截断：数据模型里保留完整 URL，输出按需缩短。
fn tabs_display_json(tab: &TabInfo, full: bool) -> Value {
    let url = if full {
        tab.url.clone()
    } else {
        truncate_for_tree(&tab.url, 80)
    };
    json!({
        "id": tab.id,
        "tab_id": tab.tab_id,
        "browser_id": tab.browser_id,
        "profile_id": tab.profile_id,
        "profile_label": tab.profile_label,
        "session_key": tab.session_key,
        "url": url,
        "title": tab.title,
        "type": tab.tab_type,
        "connected_at": tab.connected_at,
    })
}

async fn tabtree(State(state): State<AppState>, Query(query): Query<TabtreeQuery>) -> Json<Value> {
    touch(&state).await;
    // 只读命令：浏览器没开时用短等待，别再白等 5 秒。
    wait_for_sessions(&state, WAIT_SESSIONS_READ).await;
    let items: Vec<SessionTreeItem> = {
        let driver = state.driver.read().await;
        driver
            .sessions
            .values()
            .filter(|s| s.is_active())
            .filter(|s| {
                query
                    .browser
                    .as_deref()
                    .map(|browser| s.browser_id == browser)
                    .unwrap_or(true)
            })
            .filter(|s| {
                query
                    .profile
                    .as_deref()
                    .map(|profile| {
                        s.profile_id == profile || s.profile_label.as_deref() == Some(profile)
                    })
                    .unwrap_or(true)
            })
            .filter(|s| {
                query
                    .tab
                    .as_deref()
                    .map(|tab| s.tab_id == tab || s.session_key == tab)
                    .unwrap_or(true)
            })
            .map(|s| SessionTreeItem {
                browser_id: s.browser_id.clone(),
                profile_id: s.profile_id.clone(),
                profile_label: s.profile_label.clone(),
                tab_id: s.tab_id.clone(),
                session_key: s.session_key.clone(),
                title: s.info.title.clone(),
                url: s.info.url.clone(),
            })
            .collect()
    };

    let mut browser_map: HashMap<String, HashMap<String, Vec<SessionTreeItem>>> = HashMap::new();
    for item in items {
        browser_map
            .entry(item.browser_id.clone())
            .or_default()
            .entry(item.profile_id.clone())
            .or_default()
            .push(item);
    }

    let mut browsers: Vec<Value> = browser_map
        .into_iter()
        .map(|(browser_id, profiles)| {
            let mut profile_nodes: Vec<Value> = profiles
                .into_iter()
                .map(|(profile_id, mut sessions)| {
                    sessions.sort_by(|a, b| a.tab_id.cmp(&b.tab_id));
                    let profile_label = sessions.iter().find_map(|s| s.profile_label.clone());
                    let tabs: Vec<Value> = sessions
                        .iter()
                        .map(|s| tabtree_tab_json(s, query.full))
                        .collect();
                    json!({
                        "profile_id": profile_id,
                        "profile_label": profile_label,
                        "tabs_count": tabs.len(),
                        "tabs": tabs,
                    })
                })
                .collect();
            profile_nodes.sort_by(|a, b| {
                a.get("profile_label")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .cmp(b.get("profile_label").and_then(Value::as_str).unwrap_or(""))
                    .then_with(|| {
                        a.get("profile_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .cmp(b.get("profile_id").and_then(Value::as_str).unwrap_or(""))
                    })
            });
            let tabs_count: usize = profile_nodes
                .iter()
                .map(|p| p.get("tabs_count").and_then(Value::as_u64).unwrap_or(0) as usize)
                .sum();
            json!({
                "browser_id": browser_id,
                "profiles_count": profile_nodes.len(),
                "tabs_count": tabs_count,
                "profiles": profile_nodes,
            })
        })
        .collect();
    browsers.sort_by(|a, b| {
        a.get("browser_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("browser_id").and_then(Value::as_str).unwrap_or(""))
    });
    let profiles_count: usize = browsers
        .iter()
        .map(|b| b.get("profiles_count").and_then(Value::as_u64).unwrap_or(0) as usize)
        .sum();
    let tabs_count: usize = browsers
        .iter()
        .map(|b| b.get("tabs_count").and_then(Value::as_u64).unwrap_or(0) as usize)
        .sum();
    Json(json!({
        "ok": true,
        "result": {
            "status": "success",
            "compact": !query.full,
            "filters": {
                "tab": query.tab,
                "browser": query.browser,
                "profile": query.profile,
            },
            "browsers_count": browsers.len(),
            "profiles_count": profiles_count,
            "tabs_count": tabs_count,
            "browsers": browsers,
        }
    }))
}

fn tabtree_tab_json(item: &SessionTreeItem, full: bool) -> Value {
    if full {
        json!({
            "tab_id": item.tab_id,
            "session_key": item.session_key,
            "title": item.title,
            "url": item.url,
        })
    } else {
        json!({
            "tab_id": item.tab_id,
            "title": item.title,
            "url": truncate_for_tree(&item.url, 120),
        })
    }
}

fn truncate_for_tree(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    format!("{}...", input.chars().take(max_chars).collect::<String>())
}

async fn lookup_tab(
    State(state): State<AppState>,
    AxumPath(tab_id): AxumPath<String>,
    Query(query): Query<LookupTabQuery>,
) -> Json<Value> {
    touch(&state).await;
    // 只读命令：浏览器没开时用短等待，别再白等 5 秒。
    wait_for_sessions(&state, WAIT_SESSIONS_READ).await;
    let driver = state.driver.read().await;
    let matches: Vec<&Session> = driver
        .sessions
        .values()
        .filter(|s| s.is_active())
        .filter(|s| s.tab_id == tab_id || s.session_key == tab_id)
        .filter(|s| {
            query
                .browser
                .as_deref()
                .map(|browser| s.browser_id == browser)
                .unwrap_or(true)
        })
        .filter(|s| {
            query
                .profile
                .as_deref()
                .map(|profile| {
                    s.profile_id == profile || s.profile_label.as_deref() == Some(profile)
                })
                .unwrap_or(true)
        })
        .collect();
    if matches.is_empty() {
        return Json(json!({ "ok": false, "error": format!("tab {tab_id} 未连接") }));
    }
    if matches.len() > 1 {
        let sessions: Vec<Value> = matches.iter().map(|s| session_lookup_json(s)).collect();
        return Json(json!({
            "ok": false,
            "error": format!("tab {tab_id} 存在歧义，匹配到 {} 个会话，请补 --profile 或 --browser", sessions.len()),
            "matches": sessions,
        }));
    }
    Json(json!({ "ok": true, "result": session_lookup_json(matches[0]) }))
}

async fn lookup_browser(
    State(state): State<AppState>,
    AxumPath(browser_id): AxumPath<String>,
) -> Json<Value> {
    touch(&state).await;
    // 只读命令：浏览器没开时用短等待，别再白等 5 秒。
    wait_for_sessions(&state, WAIT_SESSIONS_READ).await;
    let driver = state.driver.read().await;
    let sessions: Vec<&Session> = driver
        .sessions
        .values()
        .filter(|s| s.is_active() && s.browser_id == browser_id)
        .collect();
    if sessions.is_empty() {
        return Json(json!({ "ok": false, "error": format!("browser {browser_id} 未连接") }));
    }
    let first = sessions[0];
    Json(json!({
        "ok": true,
        "result": {
            "browser_id": browser_id,
            "profile_id": first.profile_id,
            "profile_label": first.profile_label,
            "tabs_count": sessions.len(),
            "tabs": sessions.iter().map(|s| session_lookup_json(s)).collect::<Vec<_>>(),
        }
    }))
}

async fn lookup_profile(
    State(state): State<AppState>,
    AxumPath(profile): AxumPath<String>,
) -> Json<Value> {
    touch(&state).await;
    // 只读命令：浏览器没开时用短等待，别再白等 5 秒。
    wait_for_sessions(&state, WAIT_SESSIONS_READ).await;
    let driver = state.driver.read().await;
    let sessions: Vec<&Session> = driver
        .sessions
        .values()
        .filter(|s| {
            s.is_active()
                && (s.profile_id == profile || s.profile_label.as_deref() == Some(profile.as_str()))
        })
        .collect();
    if sessions.is_empty() {
        return Json(json!({ "ok": false, "error": format!("profile {profile} 未连接") }));
    }
    let profile_ids: HashSet<String> = sessions.iter().map(|s| s.profile_id.clone()).collect();
    if profile_ids.len() > 1 {
        let matches: Vec<Value> = sessions.iter().map(|s| session_lookup_json(s)).collect();
        return Json(json!({
            "ok": false,
            "error": format!("profile {profile:?} 存在歧义，匹配到多个 profile"),
            "matches": matches,
        }));
    }
    let first = sessions[0];
    let mut browser_ids: Vec<String> = sessions.iter().map(|s| s.browser_id.clone()).collect();
    browser_ids.sort();
    browser_ids.dedup();
    Json(json!({
        "ok": true,
        "result": {
            "profile_id": first.profile_id,
            "profile_label": first.profile_label,
            "browser_ids": browser_ids,
            "browser_count": browser_ids.len(),
            "tabs_count": sessions.len(),
            "tabs": sessions.iter().map(|s| session_lookup_json(s)).collect::<Vec<_>>(),
        }
    }))
}

fn session_lookup_json(session: &Session) -> Value {
    json!({
        "browser_id": session.browser_id,
        "profile_id": session.profile_id,
        "profile_label": session.profile_label,
        "tab_id": session.tab_id,
        "session_key": session.session_key,
        "title": session.info.title,
        "url": session.info.url,
    })
}

async fn set_profile_label(
    State(state): State<AppState>,
    Json(req): Json<ProfileLabelRequest>,
) -> Json<Value> {
    touch(&state).await;
    let result = update_profile_label(&state, req).await;
    Json(match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

async fn scan(State(state): State<AppState>, Json(req): Json<ScanRequest>) -> Json<Value> {
    touch(&state).await;
    let timeout = Duration::from_secs_f64(req.timeout.unwrap_or(30.0).clamp(5.0, 300.0));
    let result = scan_page(
        &state,
        req.tabs_only,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        req.text_only,
        req.fast,
        req.max_nodes,
        timeout,
    )
    .await;
    Json(match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

async fn exec(State(state): State<AppState>, Json(req): Json<ExecRequest>) -> Json<Value> {
    touch(&state).await;
    let combined_wait = req.wait_js.is_some() && !is_extension_json(&req.script);
    Json(match exec_value(&state, req, None).await {
        Ok(value) => json!({ "ok": true, "result": value, "combined_wait": combined_wait }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

async fn exec_value(
    state: &AppState,
    req: ExecRequest,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<Value> {
    let script = req.script.clone();
    let is_extension = is_extension_json(&script);
    // 普通 JS 统一走确定性包装：单行 `a(); return b` 在扩展侧会被自动补 return 的逻辑
    // 误判成 `return a();`，导致返回值错误。把语义收拢到 daemon 侧，避免依赖扩展版本。
    let page_script = if is_extension {
        script
    } else {
        wrap_user_script(&script)
    };
    let exec_timeout = Duration::from_secs_f64(req.timeout.unwrap_or(60.0).clamp(1.0, 600.0));
    let target_options = ExecTargetOptions {
        world: req.world.clone(),
        frame_id: req.frame_id,
        all_frames: req.all_frames,
    };
    let mut result = execute_page_js_with_target(
        state,
        &page_script,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        req.no_monitor,
        exec_timeout,
        &target_options,
    )
    .await?;
    // 注意：`--max-bytes` 是按**字符**计的上限（与 --max-chars 同口径），
    // 字段名沿用历史叫法只为保持线协议兼容。
    if let Some(max_chars) = req.max_bytes.filter(|value| *value > 0) {
        let truncated = truncate_json_value_chars(&mut result, max_chars);
        if truncated {
            result["truncated"] = json!(true);
        }
    }
    // 等待逻辑放在 daemon 侧轮询：后台标签页的 setTimeout 会被 Chrome 节流到分钟级，
    // 在页面里 sleep 会让 --wait-js 在隐藏标签页里几乎必然超时。
    if let Some(wait_js) = req.wait_js.as_deref().filter(|_| !is_extension) {
        let session_key = result
            .get("session_key")
            .and_then(Value::as_str)
            .map(str::to_string);
        let wait = wait_for_js_condition(
            state,
            session_key.as_deref(),
            wait_js,
            req.wait_timeout,
            req.wait_interval,
            cancel.as_deref(),
        )
        .await?;
        let main = result.get("js_return").cloned().unwrap_or(Value::Null);
        // 保持旧版输出形状，避免破坏已有 skill 提示词：js_return = { result, wait }
        result["js_return"] = json!({ "result": main, "wait": wait });
    }
    Ok(result)
}

async fn open_tab(State(state): State<AppState>, Json(req): Json<OpenRequest>) -> Json<Value> {
    touch(&state).await;
    let target = match resolve_request_target(req.target.as_deref(), req.url.as_deref()) {
        Ok(target) => target,
        Err(err) => return Json(open_target_error_json(err)),
    };
    let resolved = match resolve_open_target(target, req.cwd.as_deref()) {
        Ok(resolved) => resolved,
        Err(err) => return Json(open_target_error_json(err)),
    };
    let mut selector = SessionSelector::new(req.switch_tab_id, req.browser, req.profile);

    if resolved.kind == TargetKind::LocalResource
        && (!req.readiness_timeout.is_finite() || req.readiness_timeout <= 0.0)
    {
        return Json(json!({
            "ok": false,
            "error": "File readiness timeout must be a finite number greater than zero",
            "error_code": "invalid_readiness_timeout",
            "details": { "readiness_timeout": req.readiness_timeout }
        }));
    }

    if resolved.kind == TargetKind::LocalResource {
        let status_payload = json!({ "cmd": "status" }).to_string();
        let status_result = match execute_page_js(
            &state,
            &status_payload,
            selector.clone(),
            true,
            Duration::from_secs(20),
        )
        .await
        {
            Ok(value) => value,
            Err(err) => {
                return Json(json!({
                    "ok": false,
                    "error": format!("Could not query extension file capability: {err}"),
                    "error_code": "extension_upgrade_required",
                    "details": { "minimum_extension_version": "2.1" }
                }))
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
            return Json(json!({
                "ok": false,
                "error": format!("Agent Browser CLI Bridge {installed_version} does not support local files; version 2.1 or newer is required"),
                "error_code": "extension_upgrade_required",
                "details": {
                    "installed_extension_version": installed_version,
                    "minimum_extension_version": "2.1"
                }
            }));
        }
        if status.get("fileSchemeAccess").and_then(Value::as_bool) != Some(true) {
            return Json(json!({
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
            }));
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
        match execute_page_js(&state, &payload, selector, true, Duration::from_secs(30)).await {
            Ok(value) => value,
            Err(err) => return Json(json!({ "ok": false, "error": err.to_string() })),
        };
    let mut result = normalize_open_result(&state, value.clone()).await;
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
            wait_for_session_key(&state, key, req.readiness_timeout).await
        } else {
            false
        };
        if !ready {
            return Json(json!({
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
            }));
        }
    }

    // 新打开的标签页始终成为后续命令的默认目标，即使它是后台打开（不切前台）。
    // 扩展创建标签页后要等一次 tabs_update 才会变成可操作会话，这里短等一下，
    // 避免 open 之后紧接着的第一条命令打到旧标签页上。
    let mut ready = false;
    if let Some(key) = expected_session_key.as_deref() {
        ready = wait_for_session_key(&state, key, req.readiness_timeout.min(3.0)).await;
    }
    {
        let mut driver = state.driver.write().await;
        if let Some(key) = expected_session_key.clone() {
            if driver.sessions.get(&key).is_some_and(Session::is_active) {
                driver.default_session_key = Some(key);
                driver.preferred_default_session_key = None;
            } else {
                driver.preferred_default_session_key = Some(key);
            }
        }
    }
    if let Some(object) = result.as_object_mut() {
        object.insert("ready".to_string(), json!(ready));
    }
    Json(json!({ "ok": true, "result": result }))
}

fn resolve_request_target<'a>(
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

fn open_target_error_json(err: OpenTargetError) -> Value {
    json!({
        "ok": false,
        "error": err.message,
        "error_code": err.code,
        "details": err.details,
    })
}

fn version_at_least(installed: &str, minimum: &str) -> bool {
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
fn bundled_extension_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        serde_json::from_str::<Value>(include_str!("../assets/tmwd_cdp_bridge/manifest.json"))
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
fn extension_freshness(profiles: &[Value]) -> Value {
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

async fn wait_for_session_key(state: &AppState, session_key: &str, timeout: f64) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.1));
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

async fn normalize_open_result(state: &AppState, value: Value) -> Value {
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
        "group": opened.get("group").cloned().unwrap_or(Value::Null),
        "metadata": {
            "executor": {
                "tab_id": value.get("tab_id").cloned().unwrap_or(Value::Null),
                "session_key": value.get("session_key").cloned().unwrap_or(Value::Null)
            },
            "raw": opened
        }
    })
}

async fn find_unique_session_key_by_tab_id(state: &AppState, tab_id: &str) -> Option<String> {
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

fn derive_session_key(executor_session_key: &str, opened_tab_id: &str) -> Option<String> {
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

async fn close(State(state): State<AppState>, Json(req): Json<CloseRequest>) -> Json<Value> {
    touch(&state).await;
    let selector = SessionSelector::new(Some(req.tab_id), req.browser, req.profile);
    let session_key = match select_tab(&state, selector).await {
        Ok(value) => value,
        Err(err) => return Json(json!({ "ok": false, "error": err.to_string() })),
    };
    let physical_tab_id = match session_tab_id(&state, &session_key).await {
        Ok(value) => value,
        Err(err) => return Json(json!({ "ok": false, "error": err.to_string() })),
    };
    let physical_tab_number = match parse_tab_id(&physical_tab_id) {
        Ok(value) => value,
        Err(err) => return Json(json!({ "ok": false, "error": err.to_string() })),
    };
    let payload = json!({
        "cmd": "closeTab",
        "tabId": physical_tab_number,
    })
    .to_string();
    let result = execute_page_js(
        &state,
        &payload,
        SessionSelector::new(Some(session_key.clone()), None, None),
        true,
        Duration::from_secs(15),
    )
    .await;
    match result {
        Ok(value) => {
            // 先让挂在这个标签上的在途执行立刻失败，再回收会话级状态。
            {
                let mut execs = state.execs.lock().await;
                for pending in execs.take_session(&session_key) {
                    let _ = pending
                        .tx
                        .send(Err(anyhow!("tab_closed: 标签页已关闭，命令未完成")));
                }
            }
            forget_session(&state, &session_key).await;
            Json(json!({ "ok": true, "closed": true, "close_confirmed": true, "result": value }))
        }
        Err(err) => {
            // 失败 ≠ 已关闭。旧实现无条件标 `disconnected_at`，网络抖动/超时时
            // 用户看到「标签未连接」，浏览器里却明明开着；而标签真关掉时下一次
            // tabs_update 也会把不存在的会话标成 disconnected，所以这里保留 active
            // 不会漏掉真实关闭。只有扩展明确回报成功才改状态。
            Json(json!({
                "ok": false,
                "closed": false,
                "close_confirmed": false,
                "session_key": session_key,
                "error": err.to_string(),
                "hint": "关闭未被扩展确认（标签可能仍开着，也可能已关）；用 tabs 复核后再决定是否重试 close"
            }))
        }
    }
}

async fn shutdown(State(state): State<AppState>) -> Json<Value> {
    touch(&state).await;
    let cleanup = cleanup_on_shutdown(&state).await;
    let _ = state.shutdown.send(());
    Json(json!({
        "ok": true,
        "status": "shutdown_requested",
        "cleanup": cleanup
    }))
}

async fn cleanup_on_shutdown(state: &AppState) -> Value {
    let (snapshot_count, sessions) = {
        let mut driver = state.driver.write().await;
        let snapshot_count = driver.snapshots.len();
        let sessions: Vec<(String, String, tokio::sync::mpsc::UnboundedSender<String>)> = driver
            .sessions
            .iter()
            .filter(|(_, session)| session.is_active())
            .map(|(key, session)| (key.clone(), session.tab_id.clone(), session.sender.clone()))
            .collect();
        driver.snapshots.clear();
        (snapshot_count, sessions)
    };
    let (pending_count, active_exec_count) = {
        let mut execs = state.execs.lock().await;
        let pending_count = execs.pending.len();
        let active_exec_count = execs.active.len();
        execs.pending.clear();
        execs.active.clear();
        execs.acked.clear();
        (pending_count, active_exec_count)
    };

    let mut plugin_results = Vec::new();
    for (session_key, tab_id, sender) in sessions {
        let exec_id = uuid::Uuid::new_v4().to_string();
        // 关闭流程里不能因为一个坏 tab_id 中断其余标签的清理：记为 sent:false 并带上原因。
        let tab_id_number = match parse_tab_id(&tab_id) {
            Ok(value) => value,
            Err(err) => {
                plugin_results.push(json!({
                    "session_key": session_key,
                    "tab_id": tab_id,
                    "sent": false,
                    "error": err.to_string()
                }));
                continue;
            }
        };
        let payload = json!({
            "id": exec_id,
            "code": json!({ "cmd": "debugClearAll" }).to_string(),
            "tabId": tab_id_number
        })
        .to_string();
        let ok = sender.send(payload).is_ok();
        plugin_results.push(json!({ "session_key": session_key, "tab_id": tab_id, "sent": ok }));
    }

    let (job_total, job_aborted) = {
        let mut store = state.jobs.lock().await;
        let total = store.jobs.len();
        let finished_at = epoch_secs();
        let mut aborted = 0;
        for job in store.jobs.values_mut() {
            if job.status.is_running() {
                aborted += 1;
                job.status = JobStatus::Failed;
                job.finished_at = Some(finished_at);
                job.error = Some(json!({
                    "code": "daemon_shutdown",
                    "message": "daemon 在任务完成前退出"
                }));
            }
        }
        (total, aborted)
    };

    json!({
        "daemon": {
            "snapshots": snapshot_count,
            "pending": pending_count,
            "active_exec_sessions": active_exec_count
        },
        "jobs": {
            "total": job_total,
            "aborted": job_aborted
        },
        "plugin": {
            "sent": plugin_results
        }
    })
}

fn normalize_profile_label(label: Option<String>) -> Result<Option<String>> {
    let Some(label) = label else {
        return Ok(None);
    };
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > 40 {
        return Err(anyhow!("profile label 长度不能超过 40 个字符"));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(anyhow!("profile label 只能包含英文、数字、-、_、."));
    }
    Ok(Some(trimmed.to_string()))
}

async fn update_profile_label(state: &AppState, req: ProfileLabelRequest) -> Result<Value> {
    let label = normalize_profile_label(req.label)?;
    // 串行化「查重 → 通知扩展 → 写回内存」全过程：查重和写入之间隔着一次最长
    // 10s 的扩展往返，只靠读写锁两个并发请求都能通过查重，最后同一个 label 挂到
    // 两个 profile 上，之后按 label 选 profile 就开始歧义。
    let _label_guard = state.profile_label_lock.lock().await;
    let session_key = select_tab(
        state,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
    )
    .await?;
    let (profile_id, browser_id, tab_id, sender) = {
        let driver = state.driver.read().await;
        let session = driver
            .sessions
            .get(&session_key)
            .filter(|s| s.is_active())
            .ok_or_else(|| anyhow!("会话ID {session_key} 未连接"))?;
        if let Some(label) = label.as_deref() {
            let duplicate = driver.sessions.values().find(|s| {
                s.is_active()
                    && s.profile_id != session.profile_id
                    && s.profile_label.as_deref() == Some(label)
            });
            if let Some(duplicate) = duplicate {
                return Err(anyhow!(
                    "profile label {label:?} 已被 profile {}（浏览器 {}）使用",
                    duplicate.profile_id,
                    duplicate.browser_id
                ));
            }
        }
        (
            session.profile_id.clone(),
            session.browser_id.clone(),
            session.tab_id.clone(),
            session.sender.clone(),
        )
    };

    let exec_id = Uuid::new_v4().to_string();
    let payload = json!({
        "id": exec_id,
        "code": json!({ "cmd": "setProfileLabel", "label": label }).to_string(),
        "tabId": parse_tab_id(&tab_id)?
    })
    .to_string();
    let (tx, rx) = oneshot::channel();
    // 顺序必须是「注册 → send → send 失败时 take 回滚」：
    // send 之后扩展可能在下一个事件循环就回包，而回包由另一条任务处理；
    // 若那时还没注册，`take` 取不到，结果直接丢，这条命令只能等到超时。
    // 旧写法把 `?` 挂在 send 上，失败时登记留在表里，tx 永不发结果、条目永不清理。
    state.execs.lock().await.register(
        &exec_id,
        &session_key,
        crate::protocol::PendingExec {
            delivered_at: None,
            tx,
        },
    );
    if sender.send(payload).is_err() {
        state.execs.lock().await.take(&exec_id);
        return Err(anyhow!("浏览器扩展连接已断开"));
    }
    match tokio::time::timeout(Duration::from_secs(10), rx).await {
        Ok(Ok(Ok(_))) => {}
        Ok(Ok(Err(err))) => return Err(err),
        Ok(Err(_)) => return Err(anyhow!("执行结果通道已关闭")),
        Err(_) => {
            state.execs.lock().await.take(&exec_id);
            return Err(anyhow!("设置 profile label 超时"));
        }
    }

    let mut driver = state.driver.write().await;
    // 写入锁内重新查重：串行锁之外（例如扩展后续 tabs_update 把旧 label 带回来）
    // 仍可能已经出现同名 label，写入前必须再确认一次，冲突就不写并明确报错。
    if let Some(label) = label.as_deref() {
        let conflict = driver
            .sessions
            .values()
            .find(|session| {
                session.is_active()
                    && session.profile_id != profile_id
                    && session.profile_label.as_deref() == Some(label)
            })
            .map(|session| (session.browser_id.clone(), session.profile_id.clone()));
        if let Some((other_browser, other_profile)) = conflict {
            return Err(anyhow!(
                "profile label {label:?} 已被 profile {other_profile}（浏览器 {other_browser}）占用；请换一个 label"
            ));
        }
    }
    for session in driver.sessions.values_mut() {
        if session.profile_id == profile_id && session.browser_id == browser_id {
            session.profile_label = label.clone();
            session.info.profile_label = label.clone();
        }
    }
    Ok(json!({
        "status": "success",
        "profile_id": profile_id,
        "browser_id": browser_id,
        "profile_label": label,
    }))
}

async fn active_tabs(state: &AppState, wait_ready: bool) -> Vec<TabInfo> {
    active_tabs_filtered(state, wait_ready, None, None)
        .await
        .unwrap_or_default()
}

async fn active_tabs_filtered(
    state: &AppState,
    wait_ready: bool,
    browser: Option<&str>,
    profile: Option<&str>,
) -> Result<Vec<TabInfo>> {
    if wait_ready {
        wait_for_sessions(state, WAIT_SESSIONS_ACTION).await;
    }
    let driver = state.driver.read().await;
    if let Some(profile) = profile {
        let matched_profile_ids: HashSet<String> = driver
            .sessions
            .values()
            .filter(|s| s.is_active())
            .filter(|s| browser.map(|b| s.browser_id == b).unwrap_or(true))
            .filter(|s| s.profile_label.as_deref() == Some(profile))
            .map(|s| s.profile_id.clone())
            .collect();
        if matched_profile_ids.len() > 1 {
            let mut ids: Vec<String> = matched_profile_ids.into_iter().collect();
            ids.sort();
            return Err(anyhow!(
                "profile label {profile:?} 存在歧义，匹配到多个 profile: {}",
                ids.join(", ")
            ));
        }
    }
    Ok(driver
        .sessions
        .values()
        .filter(|s| s.is_active())
        .filter(|s| browser.map(|b| s.browser_id == b).unwrap_or(true))
        .filter(|s| {
            profile
                .map(|p| s.profile_id == p || s.profile_label.as_deref() == Some(p))
                .unwrap_or(true)
        })
        // 数据模型保留完整 URL：截断只发生在展示层（`tabs` 输出），
        // 否则 snapshot / @e 校验都要额外发一次 location.href 才能拿到真 URL。
        .map(|s| s.info.clone())
        .collect())
}

async fn extension_profile_capabilities(state: &AppState) -> Vec<Value> {
    let driver = state.driver.read().await;
    let mut profiles: HashMap<(String, String), Value> = HashMap::new();
    for session in driver
        .sessions
        .values()
        .filter(|session| session.is_active())
    {
        profiles
            .entry((session.browser_id.clone(), session.profile_id.clone()))
            .or_insert_with(|| {
                json!({
                    "browser_id": session.browser_id,
                    "profile_id": session.profile_id,
                    "profile_label": session.profile_label,
                    "extension_version": session.extension_version,
                    "file_scheme_access": session.file_scheme_access,
                })
            });
    }
    let mut values: Vec<Value> = profiles.into_values().collect();
    values.sort_by(|a, b| {
        a.get("profile_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("profile_id").and_then(Value::as_str).unwrap_or(""))
    });
    values
}

/// 扩展版本落后于 daemon 随包版本时，主动要求浏览器重载扩展。
///
/// Chrome 只执行加载时那份 `background.js`，改文件不会自动生效；把「重载」从
/// 用户操作变成协议行为，才不会出现「代码修好了、报错照旧」这种错配。
/// 同一个 browser_id + 版本只请求一次，避免版本没变时反复重载。
async fn request_extension_reload(
    state: &AppState,
    browser_id: &str,
    version: &str,
    tab_id: Option<String>,
    sender: mpsc::UnboundedSender<String>,
) {
    let key = format!("{browser_id}:{version}");
    {
        let mut sent = state.ext_reload_sent.lock().await;
        // 键会随浏览器/版本累积且旧版本永不回流；daemon 有 300s 自杀，泄漏有限，
        // 但仍给它一个上限：超了整体清空，最坏只是再请求一次重载（幂等）。
        if sent.len() >= EXT_RELOAD_TRACK_LIMIT {
            sent.clear();
        }
        if !sent.insert(key) {
            return;
        }
    }
    let payload = json!({
        "id": Uuid::new_v4().to_string(),
        "code": json!({ "cmd": "management", "method": "reload" }).to_string(),
        "tabId": tab_id.unwrap_or_default(),
    })
    .to_string();
    tokio::spawn(async move {
        // 让刚建立的连接先完成 tabs 同步，避免重载打断握手。
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ = sender.send(payload);
    });
    eprintln!(
        "[ext] 扩展版本 {version} 低于随包版本 {}，已请求浏览器重载扩展",
        bundled_extension_version()
    );
}

async fn has_extension_connection(state: &AppState) -> bool {
    let driver = state.driver.read().await;
    driver
        .sessions
        .values()
        .any(|session| session.info.tab_type == "ext_ws" && session.is_active())
}

async fn wait_for_sessions(state: &AppState, timeout: Duration) -> bool {
    if has_active_sessions(state).await {
        return true;
    }
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return has_active_sessions(state).await;
        }
        let remaining = deadline.saturating_duration_since(now);
        tokio::select! {
            _ = state.sessions_ready.notified() => {
                if has_active_sessions(state).await {
                    return true;
                }
            }
            _ = tokio::time::sleep(remaining.min(Duration::from_millis(200))) => {
                if has_active_sessions(state).await {
                    return true;
                }
            }
        }
    }
}

async fn has_active_sessions(state: &AppState) -> bool {
    let driver = state.driver.read().await;
    driver.sessions.values().any(|s| s.is_active())
}

/// 清理长时间断开的会话及其 snapshot 缓存，避免长跑 daemon 里 sessions/snapshots 只增不减。
fn prune_sessions(driver: &mut DriverState) -> Vec<String> {
    let now = Instant::now();
    let stale: Vec<String> = driver
        .sessions
        .iter()
        .filter(|(_, session)| {
            session
                .disconnected_at
                .is_some_and(|at| now.duration_since(at) > STALE_SESSION_TTL)
        })
        .map(|(key, _)| key.clone())
        .collect();
    if stale.is_empty() {
        return stale;
    }
    for key in &stale {
        driver.sessions.remove(key);
        driver.snapshots.remove(key);
        if driver.default_session_key.as_deref() == Some(key.as_str()) {
            driver.default_session_key = None;
        }
        if driver.latest_session_key.as_deref() == Some(key.as_str()) {
            driver.latest_session_key = None;
        }
        if driver.preferred_default_session_key.as_deref() == Some(key.as_str()) {
            driver.preferred_default_session_key = None;
        }
    }
    // 在途执行的清理由调用方用 execs 锁完成（避免两把锁嵌套在同步函数里）。
    stale
}

/// 会话销毁后统一回收它的全部会话级状态。
///
/// **前提**：调用方已确认该会话不会再被使用（连接已断 / 标签已关闭 / 被 prune）。
/// 只有在会话确实没了之后才能移除 `session_locks` 里的那把锁：否则若此时仍有
/// 旧锁持有者在跑同一会话，新命令会通过 `entry().or_insert_with()` 建出第二把锁，
/// 同一会话就被两个任务并发了。锁序：先 driver（读完即释放），再 session_locks。
async fn forget_session(state: &AppState, session_key: &str) {
    {
        let mut driver = state.driver.write().await;
        driver.sessions.remove(session_key);
        driver.snapshots.remove(session_key);
        if driver.default_session_key.as_deref() == Some(session_key) {
            driver.default_session_key = None;
        }
        if driver.latest_session_key.as_deref() == Some(session_key) {
            driver.latest_session_key = None;
        }
        if driver.preferred_default_session_key.as_deref() == Some(session_key) {
            driver.preferred_default_session_key = None;
        }
    }
    // 直接移除是安全的：仍持有 Arc 的任务继续用旧锁跑完，只是不会再有人加入它。
    state.session_locks.lock().await.remove(session_key);
    state.page_scale.lock().await.remove(session_key);
}

async fn scan_page(
    state: &AppState,
    tabs_only: bool,
    selector: SessionSelector,
    text_only: bool,
    fast: bool,
    max_nodes: Option<usize>,
    timeout: Duration,
) -> Result<Value> {
    let tabs = active_tabs(state, true).await;
    if tabs.is_empty() {
        return Err(anyhow!(
            "no_active_tabs: 没有可用的浏览器标签页，请先在 Chrome 打开一个 http/https 页面"
        ));
    }
    // 显式拿到 session_key 后再执行脚本，避免依赖全局默认路由（job 并发时会串台）。
    let session_key = select_tab(state, selector).await?;
    let mut result = json!({
        "status": "success",
        "metadata": {
            "tabs_count": tabs.len(),
            "tabs": tabs,
            "active_tab": session_key,
        }
    });
    if !tabs_only {
        let timeout_error = || {
            anyhow!(
                "scan_timeout: 页面内容抓取超过 {}s，可增大 --timeout",
                timeout.as_secs()
            )
        };
        if fast {
            // 快速模式：不克隆 DOM、不算 rect/style，只做一次带预算的文本抽取。
            // 十万级节点的页面上这是「秒级」与「几十毫秒」的差别。
            let opts = json!({
                "maxChars": 35000,
                "forms": false,
                "links": false,
                "maxNodes": max_nodes.unwrap_or(80_000),
                "budgetMs": 1500,
            });
            let script = format!("{}\nreturn __abcRead({});", html::js_page_read(), opts);
            let payload = tokio::time::timeout(
                timeout,
                execute_page_js(
                    state,
                    &script,
                    SessionSelector::new(Some(session_key.clone()), None, None),
                    true,
                    timeout,
                ),
            )
            .await
            .map_err(|_| timeout_error())??;
            let page = page_payload(payload, "page");
            let page = page.get("page").cloned().unwrap_or(Value::Null);
            result["content"] = json!(page.get("text").and_then(Value::as_str).unwrap_or_default());
            result["metadata"]["mode"] = json!("fast");
            result["metadata"]["fast"] = page.get("stats").cloned().unwrap_or(Value::Null);
            result["metadata"]["truncated"] =
                page.get("truncated").cloned().unwrap_or(Value::Bool(false));
        } else {
            let node_budget = max_nodes.unwrap_or(8_000);
            let content = tokio::time::timeout(
                timeout,
                get_html(
                    state,
                    &session_key,
                    true,
                    35000,
                    text_only,
                    timeout,
                    node_budget,
                ),
            )
            .await
            .map_err(|_| timeout_error())??;
            result["content"] = Value::String(content);
            result["metadata"]["mode"] = json!(if text_only { "text" } else { "html" });
            result["metadata"]["node_budget"] = json!(node_budget);
        }
    }
    Ok(result)
}

/// 把用户脚本包进「MutationObserver 记录 + 执行」的一次注入里。
///
/// 旧的 monitor 做法是执行前后各 `getHtml` 一次再比较字符串——大页面上光是
/// 两次序列化就要几十到上百毫秒。这里改成页面内观察，只回传计数和少量样本。
fn wrap_with_mutation_watch(script: &str) -> String {
    let source = serde_json::to_string(script).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"(async () => {{
  const __abcSource = {source};
  const __abcWatch = {{ changed: 0, added: 0, removed: 0, attributes: 0, text: 0, samples: [] }};
  let __abcObserver = null;
  // 回调必须自己累积记录：脚本里的 await 会让微任务先跑，
  // 回调消费掉记录后 takeRecords() 就只剩空数组。
  const __abcRecords = [];
  try {{
    __abcObserver = new MutationObserver((list) => {{ for (const item of list) __abcRecords.push(item); }});
    __abcObserver.observe(document.documentElement || document, {{
      childList: true, subtree: true, attributes: true, characterData: true
    }});
  }} catch (_) {{ __abcObserver = null; }}
  let __abcResult;
  let __abcError = null;
  try {{
    const __abcAsync = Object.getPrototypeOf(async function () {{}}).constructor;
    const trimmed = String(__abcSource || '').trim();
    if (/^return\b/.test(trimmed)) {{
      __abcResult = await (new __abcAsync(trimmed))();
    }} else {{
      try {{
        const value = eval(trimmed);
        __abcResult = value instanceof Promise ? await value : value;
      }} catch (e) {{
        __abcResult = await (new __abcAsync(trimmed))();
      }}
    }}
  }} catch (e) {{
    __abcError = (e && e.message) ? e.message : String(e);
  }} finally {{
    if (__abcObserver) {{
      const pending = __abcObserver.takeRecords();
      __abcObserver.disconnect();
      for (const record of __abcRecords.concat(pending)) {{
        __abcWatch.changed += 1;
        if (record.type === 'childList') {{
          __abcWatch.added += record.addedNodes.length;
          __abcWatch.removed += record.removedNodes.length;
        }} else if (record.type === 'attributes') {{
          __abcWatch.attributes += 1;
        }} else if (record.type === 'characterData') {{
          __abcWatch.text += 1;
        }}
        if (__abcWatch.samples.length < 8) {{
          const target = record.target && record.target.nodeType === 1
            ? (record.target.tagName.toLowerCase() + (record.target.id ? '#' + record.target.id : ''))
            : String((record.target && record.target.nodeName) || '').toLowerCase();
          const added = record.addedNodes && record.addedNodes[0];
          const detail = added
            ? ('+' + (added.nodeType === 1 ? added.tagName.toLowerCase() : '#text'))
            : (record.attributeName || '');
          __abcWatch.samples.push({{ type: record.type, target: target, detail: detail }});
        }}
      }}
    }}
  }}
  if (__abcError) return {{ __abc_watch: __abcWatch, error: __abcError }};
  return {{ __abc_watch: __abcWatch, result: __abcResult }};
}})()"#
    )
}

/// 拆掉 `wrap_with_mutation_watch` 的外壳：变更计数进 `change`，页面返回值回到 `js_return`。
fn unwrap_mutation_watch(result: &mut Value) {
    let Some(payload) = result.get("js_return").cloned() else {
        return;
    };
    let Some(watch) = payload.get("__abc_watch").cloned() else {
        return;
    };
    result["change"] = watch;
    result["js_return"] = payload.get("result").cloned().unwrap_or(Value::Null);
    if let Some(error) = payload.get("error") {
        result["error"] = error.clone();
        // 显式标记：这是**包装层捕获的脚本异常**，不是用户脚本正常返回里的业务字段。
        // 调用方据此把 `exec --monitor` 判成失败（否则 throw 也会 ok:true + 退出码 0）。
        result["script_error"] = json!(true);
    }
}

/// 两段式变更观察的第一段：只装 observer，立刻返回（不做任何序列化）。
///
/// `click/fill --monitor` 以前是「动作前后各 `get_html(9_999_999)` 一次 → 比字符串」，
/// 大页面上两次整页序列化就是几十到上百毫秒，而且回传的 `top_change` 是前 2000 字
/// 原始 HTML，没有任何消费者。改成动作前装 observer、动作后收计数，
/// 输出与 `js --monitor` 的 `change` 完全同构。
///
/// `monitor_id` 是每次观察的令牌：第二段据此判断「我这次观察还在不在」，
/// 从而把「导航丢了观察器」和「被后来的观察顶掉」区分开。
fn monitor_start_script(monitor_id: &str) -> String {
    format!(
        r#"const KEY = '__abcMonitor__';
const ID = {monitor_id};
const prev = window[KEY];
if (prev && prev.observer) {{ try {{ prev.observer.disconnect(); }} catch (_) {{}} }}
const state = {{ changed: 0, added: 0, removed: 0, attributes: 0, text: 0, samples: [] }};
const records = [];
let observer = null;
try {{
  observer = new MutationObserver(function (list) {{ for (const item of list) records.push(item); }});
  observer.observe(document.documentElement || document, {{
    childList: true, subtree: true, attributes: true, characterData: true
  }});
}} catch (_) {{ observer = null; }}
if (!observer) return {{ status: 'unsupported' }};
window[KEY] = {{ id: ID, observer: observer, state: state, records: records }};
return {{ status: 'watching' }};"#,
        monitor_id = serde_json::to_string(monitor_id).unwrap_or_else(|_| "\"\"".to_string())
    )
}

/// 第二段：取走记录、断开 observer，返回与 `wrap_with_mutation_watch` 相同的计数结构。
///
/// - `status: ok` —— 正常收到计数；
/// - `status: navigated` —— 观察器不见了（最常见是动作触发了导航，文档已换）；
/// - `status: superseded` —— 同一个标签上又开了新的观察，本次被顶掉（不动对方的观察器）。
fn monitor_stop_script(monitor_id: &str) -> String {
    format!(
        r#"const KEY = '__abcMonitor__';
const ID = {monitor_id};
const entry = window[KEY];
if (!entry) return {{ status: 'navigated' }};
if (entry.id !== ID) return {{ status: 'superseded' }};
window[KEY] = null;
const state = entry.state || {{ changed: 0, added: 0, removed: 0, attributes: 0, text: 0, samples: [] }};
if (entry.observer) {{
  const pending = entry.observer.takeRecords();
  entry.observer.disconnect();
  for (const record of (entry.records || []).concat(pending)) {{
    state.changed += 1;
    if (record.type === 'childList') {{
      state.added += record.addedNodes.length;
      state.removed += record.removedNodes.length;
    }} else if (record.type === 'attributes') {{
      state.attributes += 1;
    }} else if (record.type === 'characterData') {{
      state.text += 1;
    }}
    if (state.samples.length < 8) {{
      const target = record.target && record.target.nodeType === 1
        ? (record.target.tagName.toLowerCase() + (record.target.id ? '#' + record.target.id : ''))
        : String((record.target && record.target.nodeName) || '').toLowerCase();
      const added = record.addedNodes && record.addedNodes[0];
      const detail = added
        ? ('+' + (added.nodeType === 1 ? added.tagName.toLowerCase() : '#text'))
        : (record.attributeName || '');
      state.samples.push({{ type: record.type, target: target, detail: detail }});
    }}
  }}
}}
return {{ status: 'ok', watch: state }};"#,
        monitor_id = serde_json::to_string(monitor_id).unwrap_or_else(|_| "\"\"".to_string())
    )
}

/// 一次 `--monitor` 动作的观察状态，随 `finish_operation` 一路传下去。
enum MonitorState {
    /// 没开 `--monitor`。
    Off,
    /// 观察已装好，带着本次观察的令牌。
    Armed(String),
    /// 开了 `--monitor` 但装不上（CSP / 页面环境不支持）——要显式回给调用方，不能静默。
    Unsupported,
}

/// 装观察器；`None` 表示页面环境装不上（CSP 拦下 eval / MutationObserver 不可用）。
async fn start_mutation_watch(state: &AppState, session_key: &str) -> Option<String> {
    let monitor_id = Uuid::new_v4().simple().to_string();
    let script = monitor_start_script(&monitor_id);
    let execution =
        execute_raw_js_with_session(state, &script, Duration::from_secs(10), Some(session_key))
            .await
            .ok()?;
    let status = execution
        .result
        .data
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str);
    (status == Some("watching")).then_some(monitor_id)
}

enum MonitorOutcome {
    Collected(Value),
    Navigated,
    Superseded,
}

async fn stop_mutation_watch(
    state: &AppState,
    session_key: &str,
    monitor_id: &str,
) -> Result<MonitorOutcome> {
    let script = monitor_stop_script(monitor_id);
    let execution =
        execute_raw_js_with_session(state, &script, Duration::from_secs(10), Some(session_key))
            .await?;
    let data = execution.result.data.unwrap_or(Value::Null);
    match data.get("status").and_then(Value::as_str) {
        Some("ok") => Ok(MonitorOutcome::Collected(
            data.get("watch").cloned().unwrap_or(Value::Null),
        )),
        Some("superseded") => Ok(MonitorOutcome::Superseded),
        // 注入本身成功但 KEY 不见了：文档被换掉（导航）。
        _ => Ok(MonitorOutcome::Navigated),
    }
}

async fn execute_page_js(
    state: &AppState,
    script: &str,
    selector: SessionSelector,
    no_monitor: bool,
    timeout: Duration,
) -> Result<Value> {
    execute_page_js_with_target(
        state,
        script,
        selector,
        no_monitor,
        timeout,
        &ExecTargetOptions::default(),
    )
    .await
}

async fn execute_page_js_with_target(
    state: &AppState,
    script: &str,
    selector: SessionSelector,
    no_monitor: bool,
    timeout: Duration,
    options: &ExecTargetOptions,
) -> Result<Value> {
    let executor_session_key = select_tab(state, selector).await?;
    // --monitor 改为页面内 MutationObserver：脚本执行期间记录变更，
    // 不再"执行前后各抓一次整页 HTML 再 diff"（大页面上那两次序列化比脚本本身还贵）。
    let watched_script = if no_monitor || is_extension_json(script) {
        None
    } else {
        Some(wrap_with_mutation_watch(script))
    };
    let effective_script = watched_script.as_deref().unwrap_or(script);
    // 只有需要检测「脚本是否新开标签页」时才枚举全部会话。
    // 内部调用（read/find/action/wait）一律跳过，省掉一次全量会话序列化。
    let before_tabs: std::collections::HashSet<String> = if no_monitor {
        std::collections::HashSet::new()
    } else {
        active_tabs(state, true)
            .await
            .into_iter()
            .map(|t| t.session_key)
            .collect()
    };
    let execution = execute_raw_js_with_target(
        state,
        effective_script,
        timeout,
        Some(&executor_session_key),
        options,
    )
    .await?;
    let current_session_key = execution.session_key;
    let current_tab_id = execution.tab_id;
    let response = execution.result;
    let mut result = json!({
        "status": "success",
        "js_return": response.data.or(response.result).unwrap_or(Value::Null),
        "tab_id": current_tab_id,
        "session_key": current_session_key,
    });
    if let Some(tabs) = response.new_tabs {
        result["newTabs"] = tabs;
    } else if !no_monitor {
        let after_tabs = active_tabs(state, false).await;
        let new_tabs: Vec<_> = after_tabs
            .into_iter()
            .filter(|t| !before_tabs.contains(&t.session_key))
            .map(|t| json!({ "id": t.id, "tab_id": t.tab_id, "session_key": t.session_key, "url": t.url }))
            .collect();
        if !new_tabs.is_empty() {
            result["newTabs"] = json!(new_tabs);
        }
    }
    if watched_script.is_some() {
        unwrap_mutation_watch(&mut result);
        // 包装层捕获到的脚本异常必须变成失败。只有 `script_error` 标记（由
        // unwrap_mutation_watch 设置）才判错：用户脚本正常 `return {error: ...}`
        // 属于业务数据，不带这个标记，不会被误伤。
        if result.get("script_error").and_then(Value::as_bool) == Some(true) {
            let message = result
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown script error")
                .to_string();
            return Err(anyhow!("script_error: {message}"));
        }
    }
    Ok(result)
}

struct ExecWithSession {
    result: ExecResult,
    session_key: String,
    tab_id: String,
}

#[derive(Debug, Clone, Default)]
struct ExecTargetOptions {
    world: Option<String>,
    frame_id: Option<i64>,
    all_frames: bool,
}

async fn execute_raw_js_with_session(
    state: &AppState,
    code: &str,
    timeout: Duration,
    requested_session_key: Option<&str>,
) -> Result<ExecWithSession> {
    execute_raw_js_with_target(
        state,
        code,
        timeout,
        requested_session_key,
        &ExecTargetOptions::default(),
    )
    .await
}

/// 取（或创建）某个会话的串行锁。
async fn session_lock(state: &AppState, session_key: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut map = state.session_locks.lock().await;
    map.entry(session_key.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

async fn execute_raw_js_with_target(
    state: &AppState,
    code: &str,
    timeout: Duration,
    requested_session_key: Option<&str>,
    options: &ExecTargetOptions,
) -> Result<ExecWithSession> {
    execute_raw_js_inner(state, code, timeout, requested_session_key, options, true).await
}

/// 等待类命令用：消息发出去就放掉会话锁。
///
/// `wait --js` 现在是一次注入、在页面里等几十秒；锁要是跟着等，
/// 同一标签上的 read / click 会被一个「等待」堵死。
/// 注入顺序仍由页面的事件循环保证，串行性不受影响。
async fn execute_raw_js_releasing_lock(
    state: &AppState,
    code: &str,
    timeout: Duration,
    requested_session_key: Option<&str>,
    options: &ExecTargetOptions,
) -> Result<ExecWithSession> {
    execute_raw_js_inner(state, code, timeout, requested_session_key, options, false).await
}

async fn execute_raw_js_inner(
    state: &AppState,
    code: &str,
    timeout: Duration,
    requested_session_key: Option<&str>,
    options: &ExecTargetOptions,
    hold_lock: bool,
) -> Result<ExecWithSession> {
    let (session_key, tab_id, sender) = {
        wait_for_sessions(state, WAIT_SESSIONS_ACTION).await;
        let driver = state.driver.read().await;
        let session_key = if let Some(requested) = requested_session_key {
            driver
                .sessions
                .get(requested)
                .filter(|session| session.is_active())
                .map(|session| session.session_key.clone())
                .ok_or_else(|| anyhow!("会话ID {requested} 未连接"))?
        } else {
            driver
                .default_session_key
                .as_ref()
                .and_then(|key| {
                    driver
                        .sessions
                        .get(key)
                        .filter(|s| s.is_active())
                        .map(|s| s.session_key.clone())
                })
                .or_else(|| {
                    driver.latest_session_key.as_ref().and_then(|key| {
                        driver
                            .sessions
                            .get(key)
                            .filter(|s| s.is_active())
                            .map(|s| s.session_key.clone())
                    })
                })
                .or_else(|| {
                    driver
                        .sessions
                        .values()
                        .find(|s| s.is_active())
                        .map(|s| s.session_key.clone())
                })
                .ok_or_else(|| {
                    anyhow!("没有可用的浏览器标签页，请先在 Chrome 打开一个 http/https 页面")
                })?
        };
        let session = driver
            .sessions
            .get(&session_key)
            .filter(|s| s.is_active())
            .ok_or_else(|| anyhow!("会话ID {session_key} 未连接"))?;
        (session_key, session.tab_id.clone(), session.sender.clone())
    };
    // 同一标签内串行执行：避免同一标签上并发注入互相干扰，
    // 不同标签各自一把锁，跨标签仍然并发。
    let session_guard = session_lock(state, &session_key).await;
    // 上面确认会话存在之后、建锁之前，会话可能被 forget_session 删掉。
    // 那种情况下 `entry().or_insert_with()` 会为已消亡的会话新建锁条目，
    // forget 已经跑过、不会再来删它 → 永久残留。建锁后在同一读锁语义下再确认一次。
    {
        let driver = state.driver.read().await;
        let alive = driver
            .sessions
            .get(&session_key)
            .is_some_and(|session| session.is_active());
        drop(driver);
        if !alive {
            // 只有「表里 + 本地」两个引用时才移除：别人已经拿到同一把锁时留着，
            // 让那条路径自己复核，避免拆锁导致同一标签出现两把锁。
            let mut locks = state.session_locks.lock().await;
            if Arc::strong_count(&session_guard) == 2 {
                locks.remove(&session_key);
            }
            return Err(anyhow!("会话ID {session_key} 未连接"));
        }
    }
    let mut serial = Some(session_guard.lock().await);

    let exec_id = Uuid::new_v4().to_string();
    let mut payload_value = json!({
        "id": exec_id,
        "code": code,
        "tabId": parse_tab_id(&tab_id)?
    });
    if let Some(world) = options.world.as_deref() {
        payload_value["world"] = json!(world);
    }
    if let Some(frame_id) = options.frame_id {
        payload_value["frameId"] = json!(frame_id);
    }
    if options.all_frames {
        payload_value["allFrames"] = json!(true);
    }
    let payload = payload_value.to_string();
    let (tx, rx) = oneshot::channel();
    // 顺序必须是「注册 → send → send 失败时 take 回滚」：
    // send 之后扩展可能在下一个事件循环就回包，而回包由另一条任务处理；
    // 若那时还没注册，`take` 取不到，结果直接丢，这条命令只能等到超时。
    // 旧写法把 `?` 挂在 send 上，失败时登记留在表里，tx 永不发结果、条目永不清理。
    state.execs.lock().await.register(
        &exec_id,
        &session_key,
        crate::protocol::PendingExec {
            delivered_at: None,
            tx,
        },
    );
    if sender.send(payload).is_err() {
        state.execs.lock().await.take(&exec_id);
        return Err(anyhow!("浏览器扩展连接已断开"));
    }
    // 只要「注入」这一步串行就够了；等待结果期间没必要占着锁。
    if !hold_lock {
        drop(serial.take());
    }
    let result = match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => Err(anyhow!("执行结果通道已关闭")),
        Err(_) => {
            let mut execs = state.execs.lock().await;
            let acked = execs.acked.remove(&exec_id);
            execs.take(&exec_id);
            drop(execs);
            // 超时必须作为失败返回：旧行为返回 ok:true + 一段提示字符串，
            // 智能体会误判成功并继续下一步，页面状态已经错乱。
            let detail = if acked {
                "ACK received, script may still be running; 可增大 --timeout 或改用 --async"
            } else {
                "no ACK, script may not have been delivered"
            };
            Err(anyhow!(
                "exec_timeout: 页面在 {}s 内没有返回结果（{detail}）",
                timeout.as_secs()
            ))
        }
    }?;
    Ok(ExecWithSession {
        result,
        session_key,
        tab_id,
    })
}

/// 页面元素数（带 2 分钟缓存）：决定 scan 要不要走整页 HTML 序列化。
///
/// 缓存带 URL 校验：同一 session_key 导航到新页面后旧值必须失效，
/// 否则「小页面 → 大页面」的跳转会继续按小页面判断，降级路径永远不触发。
async fn page_element_count(
    state: &AppState,
    session_key: &str,
    timeout: Duration,
) -> Result<usize> {
    let current_url = session_url(state, session_key).await;
    {
        let cache = state.page_scale.lock().await;
        if let Some((count, at, url)) = cache.get(session_key) {
            if at.elapsed() < Duration::from_secs(120) && Some(url) == current_url.as_ref() {
                return Ok(*count);
            }
        }
    }
    let response = execute_raw_js_with_session(
        state,
        // 让页面自己回报采样时的 `location.href`：只靠 daemon 侧事后读
        // `session.info.url`，导航与 tabs_update 的时序会让「新 URL 配旧计数」
        // 进入缓存，最长 120s 内 scan 的降级判断都是错的。
        "return { count: document.getElementsByTagName('*').length, url: location.href };",
        timeout.min(Duration::from_secs(10)),
        Some(session_key),
    )
    .await?;
    let sampled = response
        .result
        .data
        .or(response.result.result)
        .unwrap_or(Value::Null);
    let count = sampled
        .get("count")
        .and_then(Value::as_u64)
        .or_else(|| sampled.as_u64())
        .unwrap_or_default() as usize;
    let sampled_url = sampled
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // 只有「页面自报的 URL」与 daemon 记录的当前 URL 一致时才写缓存：
    // 不一致说明采样期间正在导航，宁可不缓存也不要写错。
    let url = session_url(state, session_key).await.unwrap_or_default();
    if !sampled_url.is_empty() && sampled_url == url {
        let mut cache = state.page_scale.lock().await;
        cache.insert(
            session_key.to_string(),
            (count, Instant::now(), sampled_url),
        );
    }
    Ok(count)
}

/// 会话当前 URL（`tabs_update` 会持续更新 `session.info.url`）；会话不存在时 None。
async fn session_url(state: &AppState, session_key: &str) -> Option<String> {
    let driver = state.driver.read().await;
    driver
        .sessions
        .get(session_key)
        .map(|session| session.info.url.clone())
        .filter(|url| !url.is_empty())
}

async fn get_html(
    state: &AppState,
    session_key: &str,
    _cutlist: bool,
    maxchars: usize,
    text_only: bool,
    timeout: Duration,
    node_budget: usize,
) -> Result<String> {
    // 纯文本走 page_read 的单遍 TreeWalker：optHTML 的文本分支要为每个节点
    // 读 getComputedStyle / getBoundingClientRect，十万级页面上是秒级开销。
    if text_only {
        let opts = json!({
            "maxChars": maxchars,
            "forms": false,
            "links": false,
            "maxNodes": node_budget.max(1),
            "budgetMs": 1500,
        });
        let script = format!("{}\nreturn __abcRead({});", html::js_page_read(), opts);
        let response =
            execute_raw_js_with_session(state, &script, timeout, Some(session_key)).await?;
        let payload = response.result.data.unwrap_or(Value::Null);
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        return Ok(clean_text(&text));
    }
    // 十万级页面上，optHTML 的调用开销和 122KB 结果的跨进程传输都要 1~2 秒，
    // 而调用方真正要的是「页面里有什么」：直接走单遍文本抽取。
    // node_budget = 0 表示调用方不关心页面规模（monitor 的前后快照），跳过探测。
    const HUGE_PAGE_ELEMENTS: usize = 60_000;
    let element_count = if node_budget == 0 {
        0
    } else {
        page_element_count(state, session_key, timeout)
            .await
            .unwrap_or(0)
    };
    if element_count > HUGE_PAGE_ELEMENTS {
        let opts = json!({
            "maxChars": maxchars,
            "forms": false,
            "links": false,
            "maxNodes": 120_000,
            "budgetMs": 1500,
        });
        let script = format!("{}\nreturn __abcRead({});", html::js_page_read(), opts);
        let response =
            execute_raw_js_with_session(state, &script, timeout, Some(session_key)).await?;
        let text = response
            .result
            .data
            .unwrap_or(Value::Null)
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        return Ok(format!(
            "<!-- scan: {element_count} elements > {HUGE_PAGE_ELEMENTS}, returned text instead of HTML (use read / scan --fast) -->\n{}",
            clean_text(&text)
        ));
    }
    let opt = html::js_opt_html();
    let page_script = format!(
        "{opt}\nreturn optHTML(false, {});",
        json!({ "nodeBudget": node_budget, "tooLarge": if node_budget > 0 { 60_000 } else { 0 } })
    );
    let response =
        execute_raw_js_with_session(state, &page_script, timeout, Some(session_key)).await?;
    let mut page = response
        .result
        .data
        .unwrap_or(Value::Null)
        .as_str()
        .unwrap_or_default()
        .to_string();
    // 超大页面：页面侧拒绝序列化，这里改走单遍文本抽取（几十毫秒）。
    if let Some(count) = page.strip_prefix("__ABC_TOO_LARGE__") {
        let elements = count.trim().parse::<usize>().unwrap_or_default();
        let opts = json!({
            "maxChars": maxchars,
            "forms": false,
            "links": false,
            "maxNodes": 120_000,
            "budgetMs": 1500,
        });
        let script = format!("{}\nreturn __abcRead({});", html::js_page_read(), opts);
        let response =
            execute_raw_js_with_session(state, &script, timeout, Some(session_key)).await?;
        let text = response
            .result
            .data
            .unwrap_or(Value::Null)
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        return Ok(format!(
            "<!-- scan: {elements} elements > 60000, returned text instead of HTML (use read / scan --fast) -->\n{}",
            clean_text(&text)
        ));
    }
    page = html::optimize_html_for_tokens(&page);
    // 列表识别不在这里做：它需要对全站 querySelectorAll('*') 并对多个容器强制触发布局，
    // 是 scan 最大的无谓开销。若将来作为独立能力（scan --list）引入，必须把结果返回给调用方，
    // 而不是白跑一次。
    // `maxchars` 是字符预算，`String::len()` 是字节数：中文页按字节比会在远未超预算时
    // 就进 truncate，白跑一次 O(n) 计数（结果不变，纯浪费）。
    if page.chars().count() > maxchars {
        page = html::smart_truncate(page, maxchars);
    }
    Ok(page)
}

fn clean_text(input: &str) -> String {
    input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_extension_json(script: &str) -> bool {
    let trimmed = script.trim();
    trimmed.starts_with('{')
        && serde_json::from_str::<Value>(trimmed)
            .ok()
            .and_then(|v| v.get("cmd").cloned())
            .is_some()
}

/// 把用户脚本包成语义确定的 async IIFE。
///
/// 目的：不依赖扩展侧 `_air`（自动补 `return`）的启发式。
/// 例如 `a(); return b` 在扩展侧会被误改成 `return a();`，返回值变成 a 的结果。
/// 这里统一处理三种写法：`return x`、裸表达式、多语句 + `await`。
fn wrap_user_script(script: &str) -> String {
    format!(
        r#"(async () => {{
  const __abcCode = {code};
  const __abcTrimmed = String(__abcCode || '').trim();
  if (!__abcTrimmed) return undefined;
  const __abcAsync = Object.getPrototypeOf(async function () {{}}).constructor;
  if (/^return\b/.test(__abcTrimmed)) return await (new __abcAsync(__abcTrimmed))();
  try {{
    const __abcValue = eval(__abcTrimmed);
    return __abcValue instanceof Promise ? await __abcValue : __abcValue;
  }} catch (__abcError) {{
    if (__abcError instanceof SyntaxError && (/return/i.test(__abcError.message) || /await/i.test(__abcError.message))) {{
      return await (new __abcAsync(__abcTrimmed))();
    }}
    throw __abcError;
  }}
}})()"#,
        code = serde_json::to_string(script).unwrap_or_else(|_| "\"\"".to_string())
    )
}

async fn touch(state: &AppState) {
    *state.last_activity.lock().await = Instant::now();
}

#[derive(Debug, Clone, Serialize)]
struct CdpCallResult {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationKind {
    Click,
    Fill,
    MouseClick,
}

#[derive(Debug, Clone)]
struct OperationOptions {
    monitor: bool,
    wait_js: Option<String>,
    wait_timeout: f64,
    wait_interval: f64,
    timeout: f64,
    /// 指定 iframe：此时强制走页面内动作（CDP 的 backendNodeId 只对主框架有效）。
    frame_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AxNode {
    #[serde(rename = "nodeId")]
    node_id: String,
    #[serde(rename = "childIds", default)]
    child_ids: Vec<String>,
    #[serde(rename = "backendDOMNodeId")]
    backend_dom_node_id: Option<i64>,
    role: Option<AxValue>,
    name: Option<AxValue>,
    value: Option<AxValue>,
    description: Option<AxValue>,
}

#[derive(Debug, Deserialize)]
struct AxValue {
    value: Option<Value>,
    #[serde(rename = "type")]
    value_type: Option<String>,
}

async fn snapshot(State(state): State<AppState>, Json(req): Json<SnapshotRequest>) -> Json<Value> {
    touch(&state).await;
    Json(match snapshot_page(&state, req).await {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

async fn click(State(state): State<AppState>, Json(req): Json<TargetActionRequest>) -> Json<Value> {
    touch(&state).await;
    Json(json_result(
        target_operation_value(&state, OperationKind::Click, req).await,
    ))
}

async fn target_operation_value(
    state: &AppState,
    kind: OperationKind,
    req: TargetActionRequest,
) -> Result<Value> {
    let options = OperationOptions {
        monitor: req.monitor,
        wait_js: req.wait_js,
        wait_timeout: req.wait_timeout,
        wait_interval: req.wait_interval,
        timeout: req.timeout,
        frame_id: req.frame_id,
    };
    run_target_operation(
        state,
        kind,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        &req.target,
        None,
        options,
    )
    .await
}

fn json_result(result: Result<Value>) -> Value {
    match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    }
}

/// 用于「请求被处理了」不等于「业务成功」的命令：外层 `ok` 跟随内层的业务结论。
///
/// 以前 challenge / downloads wait 的超时走 `json_result(Ok(...))`，外层固定 `ok: true`，
/// HTTP 直调方看到 `ok:true` 会以为等待成功（N3-06、B-02）。
/// `result` 内部结构与字段名一律不动：CLI 依赖 `/result/passed`、`/result/cleared`、
/// `/result/ok`；只把外层 `ok` 修正为真实结论。
fn json_result_with_inner_ok(result: Result<Value>) -> Value {
    match result {
        Ok(value) => {
            // 没有 `ok` 字段的分支（status/detect）表示「识别本身成功」，按成功处理。
            let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(true);
            if ok {
                return json!({ "ok": true, "result": value });
            }
            let error = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("命令未达成目标")
                .to_string();
            let error_code = value.get("error_code").cloned();
            let mut out = json!({ "ok": false, "error": error, "result": value });
            if let Some(code) = error_code {
                out["error_code"] = code;
            }
            out
        }
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    }
}

async fn fill(State(state): State<AppState>, Json(req): Json<FillRequest>) -> Json<Value> {
    touch(&state).await;
    Json(json_result(fill_value(&state, req).await))
}

async fn fill_value(state: &AppState, req: FillRequest) -> Result<Value> {
    if req.clear && req.has_value {
        return Err(anyhow!("fill: --clear 不能和 value 同时使用"));
    }
    if req.clear && req.append {
        return Err(anyhow!("fill: --clear 不能和 --append 同时使用"));
    }
    if !req.clear && !req.has_value {
        return Err(anyhow!("fill: value is required unless --clear is used"));
    }
    let value = if req.clear { String::new() } else { req.value };
    let options = OperationOptions {
        monitor: req.monitor,
        wait_js: req.wait_js,
        wait_timeout: req.wait_timeout,
        wait_interval: req.wait_interval,
        timeout: req.timeout,
        frame_id: req.frame_id,
    };
    let payload = json!({ "value": value, "append": req.append, "clear": req.clear });
    run_target_operation(
        state,
        OperationKind::Fill,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
        &req.target,
        Some(payload),
        options,
    )
    .await
}

async fn mouse_click(
    State(state): State<AppState>,
    Json(req): Json<TargetActionRequest>,
) -> Json<Value> {
    touch(&state).await;
    Json(json_result(
        target_operation_value(&state, OperationKind::MouseClick, req).await,
    ))
}

async fn send_keys(State(state): State<AppState>, Json(req): Json<SendKeysRequest>) -> Json<Value> {
    touch(&state).await;
    Json(json_result(send_keys_value(&state, req).await))
}

async fn send_keys_value(state: &AppState, req: SendKeysRequest) -> Result<Value> {
    let options = OperationOptions {
        monitor: req.monitor,
        wait_js: req.wait_js,
        wait_timeout: req.wait_timeout,
        wait_interval: req.wait_interval,
        timeout: req.timeout,
        frame_id: req.frame_id,
    };
    let selector = SessionSelector::new(req.switch_tab_id, req.browser, req.profile);
    let payload = json!({ "keys": req.keys, "target": req.target });
    run_send_keys_operation(state, selector, payload, options).await
}

async fn cookies(State(state): State<AppState>, Json(req): Json<CookiesRequest>) -> Json<Value> {
    touch(&state).await;
    Json(json_result(cookies_value(&state, req).await))
}

async fn cookies_value(state: &AppState, req: CookiesRequest) -> Result<Value> {
    let session_key = select_tab(
        state,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
    )
    .await?;
    let mut payload = json!({});
    if let Some(url) = req.url {
        payload["url"] = json!(url);
    }
    extension_control(
        state,
        &session_key,
        "cookies",
        payload,
        DEFAULT_EXTENSION_CONTROL_TIMEOUT,
    )
    .await
}

async fn inject(State(state): State<AppState>, Json(req): Json<InjectRequest>) -> Json<Value> {
    touch(&state).await;
    Json(json_result(inject_value(&state, req).await))
}

async fn inject_value(state: &AppState, req: InjectRequest) -> Result<Value> {
    let action = req
        .action
        .clone()
        .unwrap_or_else(|| "run".to_string())
        .to_lowercase();
    if !["run", "persist", "remove", "list"].contains(&action.as_str()) {
        return Err(anyhow!(
            "inject: action 只能是 run / persist / remove / list"
        ));
    }
    if (action == "run" || action == "persist")
        && req.code.as_deref().unwrap_or_default().trim().is_empty()
    {
        return Err(anyhow!("inject: 需要脚本内容（位置参数或 --file）"));
    }
    if (action == "persist" || action == "remove")
        && req.name.as_deref().unwrap_or_default().trim().is_empty()
    {
        return Err(anyhow!("inject: persist/remove 需要 --name"));
    }
    let session_key = select_tab(
        state,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
    )
    .await?;
    let mut payload = json!({ "action": action });
    if let Some(code) = req.code {
        payload["code"] = json!(code);
    }
    if let Some(name) = req.name {
        payload["name"] = json!(name);
    }
    if let Some(world) = req.world {
        payload["world"] = json!(world.to_uppercase());
    }
    if let Some(matches) = req.matches {
        payload["matches"] = json!(matches);
    }
    if let Some(frame_id) = req.frame_id {
        payload["frameId"] = json!(frame_id);
    }
    if req.all_frames {
        payload["allFrames"] = json!(true);
    }
    let value = extension_control(
        state,
        &session_key,
        "inject",
        payload,
        DEFAULT_EXTENSION_CONTROL_TIMEOUT,
    )
    .await?;
    // 扩展回的是 {ok, data}；这里把 data 提到顶层，避免 CLI 出现 result.result.data 三层。
    Ok(value.get("data").cloned().unwrap_or(value))
}

/// `frames` 只需要定位标签：以前复用 `ReadRequest`（带 selector / max_chars /
/// links / budget_ms 等一堆与 iframe 枚举无关的字段），语义误导且校验不到。
/// 字段名与 `ReadRequest` 一致，线协议（CLI 发的 JSON）保持不变。
#[derive(Debug, Deserialize, Default)]
struct FramesRequest {
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
}

async fn frames(State(state): State<AppState>, Json(req): Json<FramesRequest>) -> Json<Value> {
    touch(&state).await;
    Json(json_result(frames_value(&state, req).await))
}

async fn frames_value(state: &AppState, req: FramesRequest) -> Result<Value> {
    let session_key = select_tab(
        state,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
    )
    .await?;
    let value = extension_control(
        state,
        &session_key,
        "frames",
        json!({}),
        DEFAULT_EXTENSION_CONTROL_TIMEOUT,
    )
    .await?;
    Ok(value.get("data").cloned().unwrap_or(value))
}

// ---------------------------------------------------------------- 下载监听

#[derive(Debug, Deserialize, Default)]
struct DownloadsRequest {
    /// list | start | wait | cancel | erase
    action: Option<String>,
    id: Option<i64>,
    url: Option<String>,
    filename: Option<String>,
    dir: Option<String>,
    state: Option<String>,
    limit: Option<usize>,
    /// wait 的超时（秒）与轮询间隔（秒）。
    wait_timeout: Option<f64>,
    interval: Option<f64>,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
}

async fn downloads(
    State(state): State<AppState>,
    Json(req): Json<DownloadsRequest>,
) -> Json<Value> {
    touch(&state).await;
    // wait 的超时 / interrupted 是业务失败，外层 ok 必须跟随内层（B-02）。
    Json(json_result_with_inner_ok(
        downloads_value(&state, req).await,
    ))
}

async fn downloads_value(state: &AppState, req: DownloadsRequest) -> Result<Value> {
    let action = req
        .action
        .clone()
        .unwrap_or_else(|| "list".to_string())
        .to_lowercase();
    let session_key = select_tab(
        state,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
    )
    .await?;

    if action != "wait" {
        let mut payload = json!({ "action": action });
        if let Some(id) = req.id {
            payload["id"] = json!(id);
        }
        if let Some(url) = req.url {
            payload["url"] = json!(url);
        }
        if let Some(filename) = req.filename {
            payload["filename"] = json!(filename);
        }
        if let Some(dir) = req.dir {
            payload["dir"] = json!(dir);
        }
        if let Some(state_filter) = req.state {
            payload["state"] = json!(state_filter);
        }
        if let Some(limit) = req.limit {
            payload["limit"] = json!(limit.clamp(1, 200));
        }
        let value = extension_control(
            state,
            &session_key,
            "downloads",
            payload,
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await?;
        return Ok(value.get("data").cloned().unwrap_or(value));
    }

    // wait：在 daemon 侧轮询，避免扩展 Service Worker 在长下载期间被回收。
    let timeout = req.wait_timeout.unwrap_or(120.0).clamp(1.0, 3600.0);
    let interval = req.interval.unwrap_or(0.5).clamp(0.05, 5.0);
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    let mut polls: u64 = 0;
    let mut last: Value = Value::Null;
    loop {
        polls += 1;
        let value = extension_control(
            state,
            &session_key,
            "downloads",
            json!({ "action": "list", "limit": 50, "id": req.id }),
            DEFAULT_EXTENSION_CONTROL_TIMEOUT,
        )
        .await?;
        let data = value.get("data").cloned().unwrap_or(value);
        let items = data
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let item = if let Some(id) = req.id {
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_i64) == Some(id))
                .cloned()
        } else {
            items.first().cloned()
        };
        if let Some(item) = item {
            let download_state = item
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if download_state == "complete" || download_state == "interrupted" {
                return Ok(json!({
                    "ok": download_state == "complete",
                    "matched": download_state == "complete",
                    "state": download_state,
                    "item": item,
                    "polls": polls,
                    "waited_ms": (polls.saturating_sub(1) as f64 * interval * 1000.0).round() as u64
                }));
            }
            last = item;
        }
        if Instant::now() >= deadline {
            return Ok(json!({
                "ok": false,
                "matched": false,
                "state": "timeout",
                "error": format!("download_wait_timeout: {timeout}s 内未完成"),
                "item": last,
                "polls": polls
            }));
        }
        tokio::time::sleep(Duration::from_secs_f64(interval)).await;
    }
}

// ---------------------------------------------------------------- 多浏览器编排

#[derive(Debug, Serialize, Deserialize, Default)]
struct BrowserTarget {
    browser_id: String,
    profile_id: String,
    profile_label: Option<String>,
    session_key: String,
    tab_id: String,
    url: String,
    title: String,
    extension_version: Option<String>,
}

/// 枚举当前所有 (browser, profile) 目标；每个组合取一个代表标签页。
async fn browser_targets(state: &AppState) -> Vec<BrowserTarget> {
    let driver = state.driver.read().await;
    let mut groups: HashMap<(String, String), Vec<&Session>> = HashMap::new();
    for session in driver.sessions.values().filter(|s| s.is_active()) {
        groups
            .entry((session.browser_id.clone(), session.profile_id.clone()))
            .or_default()
            .push(session);
    }
    let mut targets = Vec::new();
    for ((browser_id, profile_id), sessions) in groups {
        let default_key = driver.default_session_key.clone();
        let chosen = sessions
            .iter()
            .find(|s| default_key.as_deref() == Some(s.session_key.as_str()))
            .or_else(|| sessions.first())
            .copied();
        let Some(session) = chosen else { continue };
        targets.push(BrowserTarget {
            browser_id,
            profile_id,
            profile_label: session.profile_label.clone(),
            session_key: session.session_key.clone(),
            tab_id: session.tab_id.clone(),
            url: session.info.url.clone(),
            title: session.info.title.clone(),
            extension_version: session.extension_version.clone(),
        });
    }
    targets.sort_by(|a, b| {
        (a.browser_id.clone(), a.profile_id.clone())
            .cmp(&(b.browser_id.clone(), b.profile_id.clone()))
    });
    targets
}

async fn browsers(State(state): State<AppState>) -> Json<Value> {
    touch(&state).await;
    let targets = browser_targets(&state).await;
    let mut browsers: Vec<Value> = Vec::new();
    for target in &targets {
        let profile = json!({
            "profile_id": target.profile_id,
            "label": target.profile_label,
            "tab_id": target.tab_id,
            "session_key": target.session_key,
            "url": target.url,
            "title": target.title,
            "extension_version": target.extension_version,
        });
        match browsers.iter_mut().find(|item| {
            item.get("browser_id").and_then(Value::as_str) == Some(target.browser_id.as_str())
        }) {
            Some(existing) => {
                if let Some(profiles) = existing.get_mut("profiles").and_then(Value::as_array_mut) {
                    profiles.push(profile);
                }
            }
            None => browsers.push(json!({
                "browser_id": target.browser_id,
                "profiles": [profile],
            })),
        }
    }
    Json(json!({
        "ok": true,
        "result": {
            "count": targets.len(),
            "browsers": browsers,
            "targets": targets,
        }
    }))
}

#[derive(Debug, Deserialize)]
struct BroadcastRequest {
    op: String,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    browsers: Vec<String>,
    #[serde(default)]
    profiles: Vec<String>,
    concurrency: Option<usize>,
}

async fn broadcast(
    State(state): State<AppState>,
    Json(req): Json<BroadcastRequest>,
) -> Json<Value> {
    touch(&state).await;
    Json(json_result(broadcast_value(&state, req).await))
}

async fn broadcast_value(state: &AppState, req: BroadcastRequest) -> Result<Value> {
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
                "未找到浏览器 {}；可用 \"{}*\" 做前缀匹配，或用 agent-browser-cli browsers 查看完整 id",
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
    for target in targets {
        let state = state.clone();
        let op = req.op.clone();
        let base_args = req.args.clone();
        let semaphore = semaphore.clone();
        tasks.spawn(async move {
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
        });
    }

    let mut results = Vec::new();
    let mut failed = 0usize;
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((target, result, elapsed_ms)) => {
                let (ok, payload) = match result {
                    Ok(value) => (true, json!({ "result": value })),
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

#[derive(Debug, Deserialize, Default)]
struct NetRequest {
    /// fetch | hook | log | route | unroute | routes | reset
    action: Option<String>,
    url: Option<String>,
    method: Option<String>,
    headers: Option<Value>,
    body: Option<Value>,
    /// 响应体上限（字符）与返回形式 text | base64
    max_chars: Option<usize>,
    response_type: Option<String>,
    /// hook 选项
    capture_bodies: Option<bool>,
    max_log: Option<usize>,
    /// log / unroute 的过滤条件
    #[serde(rename = "match")]
    pattern: Option<String>,
    limit: Option<usize>,
    clear: Option<bool>,
    /// route 规则：{match, method, status, body, headers, abort, requestHeaders, requestBody, response}
    rule: Option<Value>,
    /// 拦截引擎：page（默认，包装 fetch/XHR）/ cdp（扩展侧 Fetch 域，能改二进制体）
    /// 对 `log` 动作还接受 `both`（合并页面侧与扩展侧，缺省行为）。
    engine: Option<String>,
    /// log：是否记录被规则放行（pass）的请求，供排查「为什么没命中 mock」。
    /// 线协议字段是 camelCase `debugPass`，扩展侧读的就是这个名字。
    #[serde(default)]
    debug_pass: Option<bool>,
    /// hook 是否持久化（导航后自动重装）
    #[serde(default)]
    persist: bool,
    /// 持久化的 URL 匹配模式（默认 <all_urls>）
    matches: Option<Vec<String>>,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    frame_id: Option<i64>,
    timeout: Option<f64>,
}

async fn net(State(state): State<AppState>, Json(req): Json<NetRequest>) -> Json<Value> {
    touch(&state).await;
    // 只有 reset 在顶层带 `ok`（页面级 reset 可能失败）；其它 action 没有该字段，
    // 外层仍按「请求被处理了」处理，语义不变。
    Json(json_result_with_inner_ok(net_value(&state, req).await))
}

async fn net_value(state: &AppState, req: NetRequest) -> Result<Value> {
    let action = req
        .action
        .clone()
        .unwrap_or_else(|| "fetch".to_string())
        .to_lowercase();
    let raw_engine = req.engine.clone().map(|value| value.to_lowercase());
    let engine = raw_engine.clone().unwrap_or_else(|| "page".to_string());
    // `log` 的 engine 是日志来源过滤（page / cdp / both，缺省 both=合并双源），
    // 其它动作的 engine 才是拦截引擎（page / cdp）。
    let log_source = if action == "log" {
        let source = raw_engine.clone().unwrap_or_else(|| "both".to_string());
        if !matches!(source.as_str(), "page" | "cdp" | "both") {
            return Err(anyhow!(
                "net log: 未知 engine {source}（可选 page / cdp / both）"
            ));
        }
        source
    } else {
        if !matches!(engine.as_str(), "page" | "cdp") {
            return Err(anyhow!("net: 未知 engine {engine}（可选 page / cdp）"));
        }
        // cdp 引擎只实现 route / mock / unroute；其它 action 实际走页面注入。
        // 以前放行后仍回报 engine:"cdp"，语义与结果都对不上（B-07）。
        if engine == "cdp" && !matches!(action.as_str(), "route" | "mock" | "unroute") {
            return Err(anyhow!(
                "net {action}: --engine cdp 只支持 route / mock / unroute（该 action 只支持页面引擎，请去掉 --engine cdp）"
            ));
        }
        "both".to_string()
    };
    let mut opts = json!({ "action": action });
    for (key, value) in [
        ("url", req.url.clone()),
        ("method", req.method.clone()),
        ("responseType", req.response_type.clone()),
        ("match", req.pattern.clone()),
    ] {
        if let Some(value) = value {
            opts[key] = json!(value);
        }
    }
    if let Some(headers) = req.headers.clone().filter(|value| !value.is_null()) {
        let object = if let Some(text) = headers.as_str() {
            // 解析失败不能再静默变 `{}`：请求会少一堆头，用户却看到成功（B-06）。
            serde_json::from_str::<Value>(text).map_err(|err| {
                let preview: String = text.chars().take(120).collect();
                anyhow!("net: --headers 不是合法的 JSON（{preview}）：{err}")
            })?
        } else {
            headers
        };
        opts["headers"] = object;
    }
    if let Some(body) = req.body.clone().filter(|value| !value.is_null()) {
        opts["body"] = body;
    }
    // max-chars 保留 clamp 兜住极端值，但实际值被改写时必须告诉调用方，
    // 否则 `--max-chars 200` 被抬到 1000 会表现为「我设的上限没生效」。
    let mut clamped_note: Option<Value> = None;
    if let Some(max_chars) = req.max_chars {
        let applied = max_chars.clamp(100, 2_000_000);
        if applied != max_chars {
            clamped_note = Some(json!({
                "field": "maxChars",
                "requested": max_chars,
                "applied": applied,
                "min": 100,
                "max": 2_000_000,
            }));
        }
        opts["maxChars"] = json!(applied);
    }
    if let Some(capture) = req.capture_bodies {
        opts["captureBodies"] = json!(capture);
    }
    if let Some(max_log) = req.max_log {
        opts["maxLog"] = json!(max_log.clamp(10, 5000));
    }
    if let Some(limit) = req.limit {
        opts["limit"] = json!(limit.clamp(1, 500));
    }
    if let Some(clear) = req.clear {
        opts["clear"] = json!(clear);
    }
    if let Some(rule) = req.rule.clone().filter(|value| !value.is_null()) {
        let rule = if let Some(text) = rule.as_str() {
            // 解析失败必须直接报错：以前写入 Value::Null，键存在但值为空，
            // 既绕过了下面的必填校验，又把错误推迟到 cdp 引擎（B-06）。
            serde_json::from_str::<Value>(text).map_err(|err| {
                let preview: String = text.chars().take(120).collect();
                anyhow!("net: --rule 不是合法的 JSON（{preview}）：{err}")
            })?
        } else {
            rule
        };
        opts["rule"] = rule;
    }
    if action == "fetch"
        && opts
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        return Err(anyhow!("net fetch: 需要 url"));
    }
    // 值为 Null 也算「没有 rule」：JSON 里的 null 不是可用的规则。
    if action == "route" && opts.get("rule").filter(|value| !value.is_null()).is_none() {
        return Err(anyhow!(
            "net route: 需要 --match / --url，以及可选的 --status / --body"
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
    // net 配置按 browser:profile 存：同一 profile 的所有标签共享同一套规则。
    let profile_key = net_profile_key(state, &session_key).await?;
    let mut config = {
        let driver = state.driver.read().await;
        driver
            .net_hooks
            .get(&profile_key)
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(net_config_default)
    };
    let timeout = Duration::from_secs_f64(req.timeout.unwrap_or(30.0).clamp(1.0, 120.0));

    // routes 是只读查询：顺带把网络层规则重新下发给该 profile 的每个标签
    // （新开的标签还没有 Fetch.enable，重新下发一次就补齐了）。
    if action == "routes" {
        let mut applied = Value::Null;
        // 只读命令只换规则：传 `rules` 而不是 `start`——后者会重置扩展侧统计/日志，
        // 还要重新走一遍 Fetch.disable + Fetch.enable（同一套 patterns 也会被判为需要重开）。
        // 同理第 4 个实参必须是 cdp 规则数组，整份 config 会被当成 CDP 规则解析。
        let cdp_rules = config.get("cdpRules").cloned().unwrap_or_else(|| json!([]));
        if config.get("cdpEnabled").and_then(Value::as_bool) == Some(true) {
            // 下发失败不能静默：旧实现 `if let Ok(value)` 吞错，`applied` 保持 null，
            // 调用方分不清「没有规则要下发」和「扩展根本没收到」（B-11）。
            applied =
                match net_cdp_broadcast(state, &profile_key, "rules", &cdp_rules, timeout).await {
                    Ok(value) => value,
                    Err(err) => json!({
                        "ok": false,
                        "applied": 0,
                        "error": humanize_extension_error(&err.to_string()),
                    }),
                };
        }
        let routes = config.get("routes").cloned().unwrap_or_else(|| json!([]));
        return Ok(json!({
            "tab_id": tab_id_of_session(&session_key),
            "session_key": session_key,
            "profile": profile_key,
            "engine": engine,
            "net": {
                "ok": true,
                "action": "routes",
                "count": routes.as_array().map(Vec::len).unwrap_or(0),
                "routes": routes,
                "persist": config.get("persist").cloned().unwrap_or(json!(false)),
                "matches": config.get("matches").cloned().unwrap_or_else(|| json!(["<all_urls>"])),
                "cdp": {
                    "enabled": config.get("cdpEnabled").cloned().unwrap_or(json!(false)),
                    "count": cdp_rules.as_array().map(Vec::len).unwrap_or(0),
                    "rules": cdp_rules,
                    "applied": applied,
                }
            }
        }));
    }

    // reset 语义统一：清空该 profile 的页面级规则与网络层规则。
    if action == "reset" {
        let had_page = config.get("persist").and_then(Value::as_bool) == Some(true);
        let had_cdp = config.get("cdpEnabled").and_then(Value::as_bool) == Some(true);
        // reset 要连持久化条目一起删，否则 daemon 重启后旧规则会「复活」。
        remove_net_config(state, &profile_key).await;
        if had_page {
            remove_net_hook(state, &session_key, &profile_key).await?;
        }
        if had_cdp {
            let _ = net_cdp_broadcast(state, &profile_key, "stop", &json!([]), timeout).await;
        }
        // 页面级 reset 失败必须如实回报：以前 `unwrap_or_else` 把它伪造成 ok:true，
        // 错误信息完全丢失（B-05）。reset 仍是尽力而为：CDP 停止在上面已执行，
        // 这里只负责把页面侧的结论带回去。
        let (mut payload, page_error) = match net_page_call(
            state,
            &session_key,
            &json!({ "action": "reset" }),
            timeout,
            req.frame_id,
        )
        .await
        {
            Ok(value) => (value, None),
            Err(err) => {
                let message = err.to_string();
                (
                    json!({ "ok": false, "action": "reset", "error": message.clone() }),
                    Some(message),
                )
            }
        };
        payload["cdp_stopped"] = json!(had_cdp);
        let reset_ok = page_error.is_none();
        let mut out = json!({
            "ok": reset_ok,
            "tab_id": tab_id_of_session(&session_key),
            "session_key": session_key,
            "profile": profile_key,
            "engine": engine,
            "net": payload,
            "page_reset": { "ok": reset_ok, "error": page_error.clone() },
            "cdp_stopped": had_cdp,
        });
        if let Some(message) = page_error {
            out["error"] = json!(message);
        }
        return Ok(out);
    }

    // 网络层拦截：规则存在 profile 上，逐标签下发（Fetch 是 per-tab 的）。
    if engine == "cdp" && matches!(action.as_str(), "route" | "mock" | "unroute") {
        let payload =
            net_cdp_action(state, &profile_key, &action, &opts, &mut config, timeout).await?;
        store_net_config(state, &profile_key, &config).await;
        return Ok(json!({
            "tab_id": tab_id_of_session(&session_key),
            "session_key": session_key,
            "profile": profile_key,
            "engine": engine,
            "net": payload,
        }));
    }

    // 页面级：持久化模式下，hook 需要带上已有规则，导航后才不会丢。
    if action == "hook" && req.persist {
        opts["routes"] = config.get("routes").cloned().unwrap_or_else(|| json!([]));
    }
    // log 要合并页面侧与 CDP 侧两份记录，页面侧先多取一些，合并排序后再截断，
    // 否则会先被页面脚本按 limit 砍掉一半，合并结果不完整。
    let log_limit = req.limit.unwrap_or(50).clamp(1, 500);
    if action == "log" {
        opts["limit"] = json!((log_limit * 2).min(500));
        // 线协议字段是 camelCase：扩展侧/CDP 日志读的是 `debugPass`。
        opts["debugPass"] = json!(req.debug_pass.unwrap_or(false));
        // engine=cdp 只读扩展侧，别顺手把页面侧日志清掉。
        if log_source == "cdp" {
            if let Some(object) = opts.as_object_mut() {
                object.remove("clear");
            }
        }
        // CDP 侧的 pass 计数由扩展的 per-tab 开关控制，必须在查 status 之前
        // 同步过去，否则 `net log --debug-pass` 打开后仍然一条 pass 都看不到。
        if log_source != "page"
            && req.debug_pass == Some(true)
            && config.get("cdpEnabled").and_then(Value::as_bool) == Some(true)
        {
            net_cdp_set_debug_pass(state, &profile_key, &config, timeout).await;
        }
    }
    let page_payload = if action == "log" && log_source == "cdp" {
        json!({ "ok": true, "action": "log", "items": [] })
    } else {
        net_page_call(state, &session_key, &opts, timeout, req.frame_id).await?
    };
    let payload = if action == "log" {
        merge_net_log(
            state,
            &session_key,
            &config,
            page_payload,
            log_limit,
            req.clear,
            req.pattern.as_deref(),
            &log_source,
            timeout,
        )
        .await
    } else {
        page_payload
    };

    // 持久化：记录配置，并把「页面脚本 + 当前规则」注册成 document_start 注入。
    // matches 为空时不注册（fail-closed），并把原因带回给调用方（B-10）。
    let mut persist_warning: Option<String> = None;
    if action == "hook" && req.persist {
        // matches 收窄：显式 --match 就按它来；否则默认只覆盖当前页面所属站点，
        // 而不是给所有网站注册 content script。已有配置取并集，跨站点累积。
        let matches = if let Some(explicit) = req.matches.clone().filter(|list| !list.is_empty()) {
            explicit
        } else {
            let mut merged: Vec<String> = config
                .get("matches")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .filter(|item| *item != "<all_urls>")
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if let Some(pattern) = current_site_match_pattern(state, &session_key).await {
                if !merged.contains(&pattern) {
                    merged.push(pattern);
                }
            }
            // 不再退回 <all_urls>：提取不到站点就保持空列表，由下面跳过注册。
            merged
        };
        let matches_available = !matches.is_empty();
        if !matches_available {
            persist_warning = Some(
                "无法从当前页面提取站点（只支持 http/https），未注册持久化注入；\
                 不会退回 <all_urls>，请用 --match 显式指定要覆盖的站点"
                    .to_string(),
            );
        }
        // 没有可用 matches 时 persist 必须是 false，否则 daemon 显示「已持久化」
        // 而扩展侧根本没有注入，daemon / 磁盘 / 扩展三方状态会再次失同步。
        config["persist"] = json!(matches_available);
        config["matches"] = json!(matches);
        config["captureBodies"] = json!(req.capture_bodies.unwrap_or(false));
        config["maxLog"] = json!(req.max_log.unwrap_or(200));
        // hook 分支的回包里 `routes` 是「条数」（历史字段，兼容旧调用方），
        // 真实规则在 `routes_list`。把数字存进 config 会在下次 install_net_hook 时
        // 被塞回 `opts.routes`，页面侧 `Array.isArray` 判断失败 → 规则静默丢失。
        config["routes"] = payload
            .get("routes_list")
            .or_else(|| payload.get("routes").filter(|value| value.is_array()))
            .cloned()
            .unwrap_or_else(|| json!([]));
        store_net_config(state, &profile_key, &config).await;
        if matches_available {
            let installed = install_net_hook(state, &session_key, &profile_key, &config).await?;
            if let Some(warning) = installed.get("warning").and_then(Value::as_str) {
                persist_warning = Some(warning.to_string());
            }
        }
    } else if matches!(action.as_str(), "route" | "unroute" | "mock") {
        if let Some(all) = payload.get("all") {
            config["routes"] = all.clone();
            store_net_config(state, &profile_key, &config).await;
        }
        // 规则变了就同步到持久化注入，刷新后仍然是同一套规则。
        if config.get("persist").and_then(Value::as_bool) == Some(true) {
            let installed = install_net_hook(state, &session_key, &profile_key, &config).await?;
            if let Some(warning) = installed.get("warning").and_then(Value::as_str) {
                persist_warning = Some(warning.to_string());
            }
        }
    }

    let mut result = json!({
        "tab_id": tab_id_of_session(&session_key),
        "session_key": session_key,
        "profile": profile_key,
        // log 时回报实际使用的日志来源（page / cdp / both），其它动作回报拦截引擎。
        "engine": if action == "log" { log_source.as_str() } else { engine.as_str() },
        "net": payload,
    });
    if let Some(note) = clamped_note {
        result["clamped"] = note;
    }
    if let Some(warning) = persist_warning {
        result["warning"] = json!(warning);
    }
    Ok(result)
}

/// 当前标签所属站点的 content-script 匹配模式：`*://host/*`。
///
/// 只在没有显式 `--match` 时用来收窄持久化注入，避免 `net hook --persist`
/// 把脚本注册到所有网站。
async fn current_site_match_pattern(state: &AppState, session_key: &str) -> Option<String> {
    let driver = state.driver.read().await;
    let url = driver
        .sessions
        .get(session_key)
        .map(|session| session.info.url.clone())
        .unwrap_or_default();
    drop(driver);
    site_match_pattern(&url)
}

/// 从 URL 提取 content-script match pattern；非 http(s) 页面返回 `None`。
///
/// 以前手写切分有两个 bug（B-10）：`https://example.com?q=1` 会把 query 当成 host，
/// 生成非法 pattern；`host.starts_with("chrom")` 又会误杀 `chromium.org`。
/// 现在交给 `url` crate 解析，过滤条件改为按 scheme 判断。
fn site_match_pattern(raw_url: &str) -> Option<String> {
    let parsed = url::Url::parse(raw_url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    // Chrome 的 match pattern 不允许带端口（`host_str` 已剥掉端口）；
    // IPv6 必须保留方括号，`host_str` 是否带括号随 url 版本而变，这里统一兜住。
    let host = if host.starts_with('[') || !host.contains(':') {
        host
    } else {
        format!("[{host}]")
    };
    Some(format!("*://{host}/*"))
}

/// net 配置的键：同一 profile 下的所有标签共用一套规则。
async fn net_profile_key(state: &AppState, session_key: &str) -> Result<String> {
    let driver = state.driver.read().await;
    let session = driver
        .sessions
        .get(session_key)
        .ok_or_else(|| anyhow!("会话ID {session_key} 未连接"))?;
    Ok(format!("{}:{}", session.browser_id, session.profile_id))
}

fn net_config_default() -> Value {
    json!({
        "persist": false,
        "matches": ["<all_urls>"],
        "captureBodies": false,
        "maxLog": 200,
        "routes": [],
        "cdpRules": [],
        "cdpEnabled": false,
    })
}

/// 把某个 profile 的 net 配置写入内存并落盘。
///
/// 落盘只带当前 profile 的条目，其余 profile 由后台线程从磁盘读回后合并：
/// 旧实现克隆**所有** profile（mock body 可达 2MB）再同步写盘，全部发生在
/// async worker 上，配置一多就会卡住整个 runtime（B-13）。
async fn store_net_config(state: &AppState, profile_key: &str, config: &Value) {
    {
        let mut driver = state.driver.write().await;
        driver
            .net_hooks
            .insert(profile_key.to_string(), config.clone());
    }
    let entry = config.clone();
    let key = profile_key.to_string();
    // 读-改-写必须串行：两个 profile 并发保存时，后写的那份不能把先写的覆盖掉。
    let _guard = state.net_persist_lock.lock().await;
    let outcome = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut map = config::load_net_hooks()?;
        map.insert(key, entry);
        config::save_net_hooks(&map)
    })
    .await;
    // 落盘失败不能让命令失败：net 规则不是安全配置，只提示一行。
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            eprintln!("net: 保存 net-hooks.json 失败（本次配置仍生效，daemon 重启后会丢）: {err}");
        }
        Err(err) => eprintln!("net: 保存 net-hooks.json 的后台任务失败: {err}"),
    }
}

/// 删除某个 profile 的 net 规则（内存 + 磁盘），供 `net reset` 使用。
async fn remove_net_config(state: &AppState, profile_key: &str) {
    {
        let mut driver = state.driver.write().await;
        driver.net_hooks.remove(profile_key);
    }
    let key = profile_key.to_string();
    let _guard = state.net_persist_lock.lock().await;
    let outcome = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut map = config::load_net_hooks()?;
        map.remove(&key);
        config::save_net_hooks(&map)
    })
    .await;
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            eprintln!("net: 保存 net-hooks.json 失败（reset 后重启可能读到旧规则）: {err}");
        }
        Err(err) => eprintln!("net: 保存 net-hooks.json 的后台任务失败: {err}"),
    }
}

/// 合并两份日志：页面 hook 记的（fetch/XHR）与扩展 CDP 侧记的（网络层拦截）。
///
/// 两边字段不完全一致，这里只做「打 source 标签 + 按 at 排序 + 统一过滤/截断」，
/// 条目本身的字段原样保留，方便排查。
// 参数数量确实偏多，但每一个都是独立的过滤/合并维度（会话、配置、两份日志、
// 条数、是否清空、关键字），打包成结构体只会把"这里传什么"藏到结构体定义里，
// 反而更难读。保留签名，显式豁免。
#[allow(clippy::too_many_arguments)]
async fn merge_net_log(
    state: &AppState,
    session_key: &str,
    config: &Value,
    page_payload: Value,
    limit: usize,
    clear: Option<bool>,
    needle: Option<&str>,
    log_source: &str,
    timeout: Duration,
) -> Value {
    let mut items: Vec<Value> = Vec::new();
    let mut page_count = 0usize;
    let mut cdp_count = 0usize;
    // CDP 侧取日志失败时不能静默：`sources.cdp=0` 与「确实没有日志」无法区分（B-12）。
    let mut cdp_error: Option<String> = None;
    let include_page = log_source != "cdp";
    let include_cdp = log_source != "page";
    if include_page {
        if let Some(list) = page_payload.get("items").and_then(Value::as_array) {
            for item in list {
                let mut copy = item.clone();
                if let Some(object) = copy.as_object_mut() {
                    object.insert("source".to_string(), json!("page"));
                }
                items.push(copy);
                page_count += 1;
            }
        }
    }
    let cdp_enabled =
        include_cdp && config.get("cdpEnabled").and_then(Value::as_bool) == Some(true);
    // 扩展回报的 debugPass 开关：带出来才能确认 `--debug-pass` 真的生效，
    // 否则用户只能看到「没有 pass 条目」，分不清是没放行过还是开关没打开。
    let mut cdp_debug_pass: Option<Value> = None;
    if cdp_enabled {
        match extension_control(
            state,
            session_key,
            "netIntercept",
            json!({ "action": "status" }),
            timeout,
        )
        .await
        {
            Ok(status) => {
                if let Some(flag) = status.get("debugPass") {
                    cdp_debug_pass = Some(flag.clone());
                }
                if let Some(list) = status.get("log").and_then(Value::as_array) {
                    for item in list {
                        let mut copy = item.clone();
                        if let Some(object) = copy.as_object_mut() {
                            object.insert("source".to_string(), json!("cdp"));
                        }
                        items.push(copy);
                        cdp_count += 1;
                    }
                }
            }
            Err(err) => cdp_error = Some(humanize_extension_error(&err.to_string())),
        }
    }
    if let Some(needle) = needle
        .map(|value| value.to_lowercase())
        .filter(|value| !value.is_empty())
    {
        items.retain(|item| {
            item.get("url")
                .and_then(Value::as_str)
                .map(|url| url.to_lowercase().contains(&needle))
                .unwrap_or(false)
        });
    }
    // 两边的 at 都是 Date.now() 毫秒，可直接排序。
    items.sort_by_key(|item| {
        item.get("at")
            .or_else(|| item.get("time"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    });
    let total = items.len();
    let skip = total.saturating_sub(limit);
    let tail: Vec<Value> = items.into_iter().skip(skip).collect();
    if clear == Some(true) && cdp_enabled {
        let _ = extension_control(
            state,
            session_key,
            "netIntercept",
            json!({ "action": "clearlog" }),
            timeout,
        )
        .await;
    }
    let mut body = json!({
        "ok": true,
        "action": "log",
        "installed": page_payload.get("installed").cloned().unwrap_or(json!(false)),
        "count": total,
        "sources": { "page": page_count, "cdp": cdp_count },
        "items": tail,
    });
    if let Some(err) = cdp_error {
        body["sources"]["cdp_error"] = json!(err);
    }
    // 只在 CDP 侧参与查询时出现；页面侧没有这个概念。
    if let Some(flag) = cdp_debug_pass {
        body["debug_pass"] = flag;
    }
    body
}

async fn net_page_call(
    state: &AppState,
    session_key: &str,
    opts: &Value,
    timeout: Duration,
    frame_id: Option<i64>,
) -> Result<Value> {
    let script = format!("{}\nreturn __abcNet({});", html::js_page_net(), opts);
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
        Ok(result) => Ok(page_payload(result, "net")
            .get("net")
            .cloned()
            .unwrap_or(Value::Null)),
        Err(err) if is_csp_error(&err.to_string()) => {
            page_op_via_extension(state, session_key, "net", opts.clone(), frame_id, timeout).await
        }
        Err(err) => Err(err),
    }
}

/// 网络层增删改：只改 profile 上的规则，然后逐标签下发到扩展。
///
/// 返回体里的 `applied` 已经说明下发明细；旧签名多带一个恒为 `true` 的 bool，
/// 调用方全部丢弃，属于会误导人的死信号，已收成单个 `Value`。
async fn net_cdp_action(
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
            rules.retain(|item| {
                let same_match =
                    item.get("match").and_then(Value::as_str) == Some(pattern.as_str());
                let same_method = item
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("*")
                    .to_uppercase()
                    == method;
                !(same_match && same_method)
            });
            rules.push(rule.clone());
            let enabled = config.get("cdpEnabled").and_then(Value::as_bool) == Some(true);
            let apply = net_cdp_broadcast(
                state,
                profile_key,
                if enabled { "rules" } else { "start" },
                &json!(rules),
                timeout,
            )
            .await?;
            config["cdpRules"] = json!(rules);
            config["cdpEnabled"] = json!(true);
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
            let apply = if config.get("cdpEnabled").and_then(Value::as_bool) == Some(true) {
                net_cdp_broadcast(state, profile_key, "rules", &json!(rules), timeout).await?
            } else {
                Value::Null
            };
            config["cdpRules"] = json!(rules);
            Ok(json!({
                "ok": true, "action": "unroute", "engine": "cdp",
                "removed": removed, "routes": rules.len(), "all": rules, "applied": apply
            }))
        }
        _ => Err(anyhow!("net: engine=cdp 不支持 action {action}")),
    }
}

/// 该 profile 下所有活跃标签的会话 key。
async fn profile_sessions(state: &AppState, profile_key: &str) -> Vec<String> {
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
async fn net_cdp_set_debug_pass(
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
async fn net_cdp_broadcast(
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

/// 旧版本（未按 profile 隔离）使用的固定注入名，仅用于清理残留（B-03）。
const LEGACY_NET_HOOK_NAME: &str = "__net_hook";

/// 持久化注入名：扩展侧 `chrome.storage.local.persistInjections` 按字符串键全局存，
/// 不分 browser/profile。名字必须带 profile 摘要，否则 B 的 `net hook --persist`
/// 会覆盖 A 的条目，A 的 `net reset` 又会删掉 B 的共享条目（B-03）。
///
/// 用 FNV-1a 而不是 `DefaultHasher`：后者不保证跨 Rust 版本稳定，而这个名字会写进
/// 扩展的持久存储，必须保证 daemon 重启/升级后同一个 profile 还能算出同一个名字。
fn net_hook_name(profile_key: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in profile_key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("__net_hook_{hash:016x}")
}

/// 把「页面网络库 + 当前规则」注册成持久注入，导航后由 persist_boot 自动重装。
async fn install_net_hook(
    state: &AppState,
    session_key: &str,
    profile_key: &str,
    config: &Value,
) -> Result<Value> {
    let matches: Vec<String> = config
        .get("matches")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // fail-closed：没有 match pattern 就不注册，绝不退回 <all_urls>（B-10）。
    if matches.is_empty() {
        return Ok(json!({
            "ok": false,
            "action": "persist",
            "skipped": true,
            "name": net_hook_name(profile_key),
            "warning": "没有可用的 match pattern，未注册持久化注入（不会退回 <all_urls>）",
        }));
    }
    let code = format!(
        "{}\n;__abcNet({});",
        html::js_page_net(),
        json!({
            "action": "hook",
            "routes": config.get("routes").cloned().unwrap_or_else(|| json!([])),
            "captureBodies": config.get("captureBodies").cloned().unwrap_or(json!(false)),
            "maxLog": config.get("maxLog").cloned().unwrap_or(json!(200)),
        })
    );
    inject_value(
        state,
        InjectRequest {
            action: Some("persist".to_string()),
            code: Some(code),
            name: Some(net_hook_name(profile_key)),
            world: Some("MAIN".to_string()),
            matches: Some(matches),
            frame_id: None,
            all_frames: false,
            switch_tab_id: Some(session_key.to_string()),
            browser: None,
            profile: None,
        },
    )
    .await
}

async fn remove_injection_named(state: &AppState, session_key: &str, name: &str) -> Result<Value> {
    inject_value(
        state,
        InjectRequest {
            action: Some("remove".to_string()),
            code: None,
            name: Some(name.to_string()),
            world: None,
            matches: None,
            frame_id: None,
            all_frames: false,
            switch_tab_id: Some(session_key.to_string()),
            browser: None,
            profile: None,
        },
    )
    .await
}

async fn remove_net_hook(state: &AppState, session_key: &str, profile_key: &str) -> Result<Value> {
    // 迁移：旧版本所有 profile 共用 `__net_hook`，新代码不会再写入这个名字。
    // 不清理的话它会一直留在 chrome.storage 里，导航后继续重装一个共享 hook，
    // 继续污染别的 profile。删除是 best-effort，失败不影响本次 reset 的结论。
    let _ = remove_injection_named(state, session_key, LEGACY_NET_HOOK_NAME).await;
    remove_injection_named(state, session_key, &net_hook_name(profile_key)).await
}

async fn read_page(State(state): State<AppState>, Json(req): Json<ReadRequest>) -> Json<Value> {
    touch(&state).await;
    Json(json_result(read_value(&state, req).await))
}

/// 结构化读取页面：一次注入返回正文、标题、元信息、表单与计数。
async fn read_value(state: &AppState, req: ReadRequest) -> Result<Value> {
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
    let opts = json!({
        "selector": req.selector,
        "maxChars": applied_max_chars,
        "links": req.links,
        "forms": req.forms,
        "includeHidden": include_hidden,
        "budgetMs": req.budget_ms,
        "maxNodes": req.max_nodes,
    });
    let script = format!("{}\nreturn __abcRead({});", html::js_page_read(), opts);
    let timeout = Duration::from_secs_f64(req.timeout.unwrap_or(30.0).clamp(1.0, 120.0));
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
        Ok(result) => {
            let mut payload = page_payload(result, "page");
            if let Some(note) = &clamped_note {
                payload["clamped"] = note.clone();
            }
            attach_challenge_if_blocked(state, &mut payload).await;
            Ok(payload)
        }
        Err(err) if is_csp_error(&err.to_string()) => {
            let session_key = select_tab(
                state,
                SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
            )
            .await?;
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
            if let Some(note) = &clamped_note {
                payload["clamped"] = note.clone();
            }
            attach_challenge_if_blocked(state, &mut payload).await;
            Ok(payload)
        }
        Err(err) => Err(err),
    }
}

/// 只在「看起来像挑战页」时才多注入一次做精确识别。
///
/// 正常页面不该为验证码检测多付一次往返；而 CF 拦截页的标题/正文特征很稳定，
/// 用几十字节的文本判断就够，命中后再让页面脚本给结构化结果。
async fn attach_challenge_if_blocked(state: &AppState, payload: &mut Value) {
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
    let title_hit = [
        "just a moment",
        "attention required",
        "checking your browser",
        "verifying you are human",
        "请稍候",
        "安全验证",
        "人机验证",
        "access denied",
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

async fn find_page(State(state): State<AppState>, Json(req): Json<FindRequest>) -> Json<Value> {
    touch(&state).await;
    Json(json_result(find_value(&state, req).await))
}

/// 元素查找：CSS / 文本 / XPath，返回可操作信息与简短 CSS 路径。
async fn find_value(state: &AppState, req: FindRequest) -> Result<Value> {
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
    let timeout = Duration::from_secs_f64(req.timeout.unwrap_or(20.0).clamp(1.0, 120.0));
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

async fn action(State(state): State<AppState>, Json(req): Json<ActionRequest>) -> Json<Value> {
    touch(&state).await;
    Json(json_result(action_value(&state, req).await))
}

/// 单往返动作：hover / click / focus / select / check / uncheck / scroll / setValue。
async fn action_value(state: &AppState, req: ActionRequest) -> Result<Value> {
    let params = if req.params.is_null() {
        json!({})
    } else {
        req.params
    };
    let timeout = Duration::from_secs_f64(req.timeout.unwrap_or(15.0).clamp(1.0, 120.0));
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
async fn run_page_action(
    state: &AppState,
    session_key: &str,
    op: &str,
    target: Value,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    run_page_action_in_frame(state, session_key, op, target, params, timeout, None).await
}

async fn run_page_action_in_frame(
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

async fn wait_page(State(state): State<AppState>, Json(req): Json<WaitRequest>) -> Json<Value> {
    touch(&state).await;
    Json(json_result(wait_value(&state, req, None).await))
}

#[derive(Debug, Deserialize)]
struct ChallengeRequest {
    /// status（默认）| wait | focus
    action: Option<String>,
    #[serde(default)]
    timeout: Option<f64>,
    #[serde(default)]
    interval: Option<f64>,
    /// 等多久还没过就把标签页带到前台交给人工（秒，0 表示不聚焦）
    #[serde(default)]
    focus_after: Option<f64>,
    #[serde(default)]
    focus: Option<bool>,
    switch_tab_id: Option<String>,
    browser: Option<String>,
    profile: Option<String>,
    frame_id: Option<i64>,
}

async fn challenge_page(
    State(state): State<AppState>,
    Json(req): Json<ChallengeRequest>,
) -> Json<Value> {
    touch(&state).await;
    // 用「跟随内层 ok」的包装：超时/未通过时外层也是 ok:false。
    Json(json_result_with_inner_ok(
        challenge_value(&state, req).await,
    ))
}

/// 挑战页 / 验证码：识别、等待、转人工。不自动破解。
///
/// `wait` 在页面里用 MutationObserver 等（一次注入一片，默认 4 秒一片），
/// 通过挑战后页面通常会跳转、注入上下文被销毁——那不是失败，重新注入再确认即可。
async fn challenge_value(state: &AppState, req: ChallengeRequest) -> Result<Value> {
    let action = req
        .action
        .clone()
        .unwrap_or_else(|| "status".to_string())
        .to_lowercase();
    if !matches!(
        action.as_str(),
        "status" | "detect" | "wait" | "solve" | "focus"
    ) {
        return Err(anyhow!(
            "challenge: 未知 action {action}（可选 status / wait / solve / focus）"
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
            json!({ "action": "status" }),
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

    // wait / solve
    let is_solve = action == "solve";
    let timeout = req
        .timeout
        .unwrap_or(if is_solve { 120.0 } else { 30.0 })
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

    // solve 先看一眼现场：没挑战 / 已经有 token 就不用进循环。
    if is_solve {
        if let Ok(status) = challenge_page_call(
            state,
            &session_key,
            json!({ "action": "status" }),
            req.frame_id,
            Duration::from_secs(10),
        )
        .await
        {
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
            if !blocking && (token_ready || !interactive) {
                return Ok(json!({
                    "tab_id": tab_id_of_session(&session_key),
                    "session_key": session_key,
                    "action": "solve",
                    "ok": true,
                    "passed": true,
                    "via": if token_ready { "token" } else { "no-challenge" },
                    "focused": focused,
                    "injections": 0,
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                    "challenge": status,
                }));
            }
            last = status;
        }
    }

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
        let opts = json!({
            "action": "wait",
            "timeoutMs": slice_ms,
            "intervalMs": interval_ms,
            // solve 还要等「验证码通过令牌」，wait 只看页面是否还被拦。
            "waitToken": is_solve,
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
        });
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
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "challenge": payload,
                    });
                    if is_solve {
                        result["passed"] = json!(true);
                        result["via"] = json!(via);
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
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "challenge": last,
    });
    if is_solve {
        // 没等到：把球交回给调用方，不要伪装成通过。
        result["passed"] = json!(false);
        result["reason"] = json!("timeout");
    }
    Ok(result)
}

async fn challenge_page_call(
    state: &AppState,
    session_key: &str,
    opts: Value,
    frame_id: Option<i64>,
    timeout: Duration,
) -> Result<Value> {
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

/// 等待条件成立：js / selector / text 三选一，轮询在 daemon 侧（后台标签页定时器会被节流）。
///
/// `cancel` 由异步任务传入：`--js` 轮询每轮开始前检查，selector/text 的观察器注入
/// 返回后再检查（页面里的观察器本身无法中断，只能等这一步做完）。
async fn wait_value(
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
        format!(
            "return ((document.body && document.body.innerText) || '').includes({});",
            serde_json::to_string(text)?
        )
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
        let timeout = Duration::from_secs_f64(req.timeout.max(0.1) + 5.0);
        let target = ExecTargetOptions {
            frame_id: req.frame_id,
            ..Default::default()
        };
        let executed = execute_page_js_with_target(
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

/// 树形 JSON 递归下钻的深度上限。
///
/// 正常页面/无障碍树的深度远小于这个值（实测都在 50 层以内）；限制只为防御
/// 极端嵌套（生成式或恶意 DOM）导致的栈溢出：命中阈值时保留当前层节点、丢弃子树。
const MAX_TREE_DEPTH: usize = 256;

/// 只保留「自身有 ref」或「子孙里有 ref」的节点，把纯结构性容器折叠掉。
fn prune_tree_keep_refs(nodes: Vec<Value>) -> Vec<Value> {
    prune_tree_keep_refs_at(nodes, 0)
}

fn prune_tree_keep_refs_at(nodes: Vec<Value>, depth: usize) -> Vec<Value> {
    let mut kept = Vec::new();
    for mut node in nodes {
        // 用 take 把子树「搬」出来，不要 `children.clone()`：深层子树 clone 本身就是
        // 一次无上限递归拷贝，2000 层就能把栈打爆，等于绕过了下面的深度保护。
        let children = match node.get_mut("children").and_then(Value::as_array_mut) {
            Some(array) => std::mem::take(array),
            None => Vec::new(),
        };
        // 超深：不再下钻（丢弃子树），当前层节点照常参与 keep 判断。
        let kept_children = if depth >= MAX_TREE_DEPTH || children.is_empty() {
            Vec::new()
        } else {
            prune_tree_keep_refs_at(children, depth + 1)
        };
        let has_ref = node.get("ref").is_some();
        if !kept_children.is_empty() {
            if let Some(object) = node.as_object_mut() {
                object.insert("children".to_string(), json!(kept_children));
            }
        } else if let Some(object) = node.as_object_mut() {
            object.remove("children");
        }
        if has_ref || node.get("children").is_some() {
            kept.push(node);
        }
    }
    kept
}

/// 按深度优先顺序截断到 `max_nodes` 个节点（0 表示不限制）。
fn fold_tree(nodes: Vec<Value>, max_nodes: usize) -> Vec<Value> {
    if max_nodes == 0 {
        return nodes;
    }
    let mut budget = max_nodes;
    fold_tree_inner(nodes, &mut budget, 0)
}

fn fold_tree_inner(nodes: Vec<Value>, budget: &mut usize, depth: usize) -> Vec<Value> {
    let mut kept = Vec::new();
    for mut node in nodes {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        // 同 prune：take 而不是 clone，避免深层子树递归拷贝绕过深度上限。
        let children = match node.get_mut("children").and_then(Value::as_array_mut) {
            Some(array) => std::mem::take(array),
            None => Vec::new(),
        };
        if !children.is_empty() {
            if depth >= MAX_TREE_DEPTH {
                // 超深：保留节点本身，丢掉子树。
                if let Some(object) = node.as_object_mut() {
                    object.remove("children");
                }
            } else {
                let kept_children = fold_tree_inner(children, budget, depth + 1);
                if let Some(object) = node.as_object_mut() {
                    if kept_children.is_empty() {
                        object.remove("children");
                    } else {
                        object.insert("children".to_string(), json!(kept_children));
                    }
                }
            }
        }
        kept.push(node);
    }
    kept
}

/// 把超过上限的返回值换成「截断预览」，避免一条命令把上下文塞满。
///
/// 上限按**字符**计（与 `--max-chars` 同口径）。历史字段 `--max-bytes` 名字容易
/// 误解成字节，实际一直都是字符；这里把函数名和判断都统一到字符语义。
fn truncate_json_value_chars(result: &mut Value, max_chars: usize) -> bool {
    let Some(slot) = result.get_mut("js_return") else {
        return false;
    };
    let serialized = match serde_json::to_string(slot) {
        Ok(text) => text,
        Err(_) => return false,
    };
    // UTF-8 里字节数恒 >= 字符数：字节数都没超就必然没超，先做 O(1) 快速放行，
    // 避免为了判断一次截断而遍历整个字符串两遍。
    if serialized.len() <= max_chars {
        return false;
    }
    let total = serialized.chars().count();
    if total <= max_chars {
        return false;
    }
    let preview: String = serialized.chars().take(max_chars).collect();
    *slot = json!({
        "__truncated": true,
        "total_chars": total,
        "preview": preview,
    });
    true
}

/// 把 `execute_page_js` 的信封拆开：页面返回值放到 `key` 下，保留定位信息。
fn page_payload(mut result: Value, key: &str) -> Value {
    let payload = result
        .get_mut("js_return")
        .map(|value| value.take())
        .unwrap_or(Value::Null);
    json!({
        "tab_id": result.get("tab_id").cloned().unwrap_or(Value::Null),
        "session_key": result.get("session_key").cloned().unwrap_or(Value::Null),
        key: payload,
    })
}

async fn upload(State(state): State<AppState>, Json(req): Json<UploadRequest>) -> Json<Value> {
    touch(&state).await;
    Json(json_result(upload_value(&state, req).await))
}

async fn upload_value(state: &AppState, req: UploadRequest) -> Result<Value> {
    if req.files.is_empty() {
        return Err(anyhow!("upload: 至少需要一个文件路径"));
    }
    for file in &req.files {
        if !Path::new(file).is_file() {
            return Err(anyhow!("upload: 文件不存在或不是普通文件: {file}"));
        }
    }
    let session_key = select_tab(
        state,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
    )
    .await?;
    let timeout = Duration::from_secs_f64(req.timeout.clamp(1.0, 300.0));
    let object_id = resolve_target_object(state, &session_key, &req.target, timeout).await?;
    dom_set_files(state, &session_key, &object_id, &req.files, timeout).await
}

/// 用 CDP 的 DOM.setFileInputFiles 设置文件，比 DataTransfer 更接近真实用户选择。
async fn dom_set_files(
    state: &AppState,
    tab_id: &str,
    object_id: &str,
    files: &[String],
    timeout: Duration,
) -> Result<Value> {
    if let Some(raw) = object_id.strip_prefix("__backend:") {
        let backend: i64 = raw
            .parse()
            .map_err(|_| anyhow!("upload: backendNodeId 解析失败: {raw}"))?;
        batch_cdp_call(
            state,
            tab_id,
            vec![json!({
                "cmd": "cdp",
                "method": "DOM.setFileInputFiles",
                "params": { "backendNodeId": backend, "files": files }
            })],
            timeout,
        )
        .await?;
    } else if let Some(selector) = object_id.strip_prefix("__selector:") {
        let expression = format!(
            "document.querySelector({})",
            serde_json::to_string(selector)?
        );
        batch_cdp_call(
            state,
            tab_id,
            vec![
                json!({ "cmd": "cdp", "method": "Runtime.evaluate", "params": { "expression": expression, "returnByValue": false } }),
                json!({ "cmd": "cdp", "method": "DOM.setFileInputFiles", "params": { "objectId": "$0.result.objectId", "files": files } }),
            ],
            timeout,
        )
        .await?;
    } else {
        return Err(anyhow!("upload: 只支持 CSS selector 或 @e 引用"));
    }
    Ok(json!({ "status": "success", "files": files, "count": files.len() }))
}

/// 分页窗口右端（`offset + limit`）。
///
/// `offset` 来自用户 JSON 且没有上限：debug 构建下 `usize::MAX + 1` 会 panic，
/// release 会回绕，导致 `has_more` / `next_offset` 全错（B-14）。
/// 这里用 `checked_add`，并保留一个宽松上限把明显越界的请求直接拒掉。
const MAX_SNAPSHOT_OFFSET: usize = 1_000_000;

fn page_window(offset: usize, limit: usize) -> Result<usize> {
    if offset > MAX_SNAPSHOT_OFFSET {
        return Err(anyhow!(
            "offset 过大（{offset}），上限 {MAX_SNAPSHOT_OFFSET}"
        ));
    }
    offset
        .checked_add(limit)
        .ok_or_else(|| anyhow!("offset 与 limit 之和过大（{offset} + {limit}）"))
}

async fn snapshot_page(state: &AppState, req: SnapshotRequest) -> Result<Value> {
    let limit = req.limit.clamp(1, 1000);
    // 先算窗口右端：两条路径（CDP / JS）都用它，越界或溢出在这里就失败。
    let end = page_window(req.offset, limit)?;
    if req.js {
        return snapshot_via_js(state, &req).await;
    }
    let tab_id = select_tab(
        state,
        SessionSelector::new(
            req.switch_tab_id.clone(),
            req.browser.clone(),
            req.profile.clone(),
        ),
    )
    .await?;
    let timeout = Duration::from_secs_f64(req.timeout.max(0.1));
    let tabs = active_tabs(state, true).await;
    let tab = tabs
        .iter()
        .find(|tab| tab.session_key == tab_id)
        .cloned()
        .ok_or_else(|| anyhow!("snapshot: tab {tab_id} is not active"))?;
    let ax = cdp_call(
        state,
        &tab_id,
        "Accessibility.getFullAXTree",
        json!({}),
        timeout,
    )
    .await;
    let ax = match ax {
        Ok(value) if value.ok => value,
        other => {
            let reason = match other {
                Ok(value) => value
                    .error
                    .unwrap_or_else(|| "Accessibility.getFullAXTree failed".to_string()),
                Err(err) => err.to_string(),
            };
            return snapshot_fallback_to_js(state, &req, reason).await;
        }
    };
    let nodes_value = match ax.data.and_then(|v| v.get("nodes").cloned()) {
        Some(value) => value,
        None => {
            return snapshot_fallback_to_js(state, &req, "CDP response missing nodes".to_string())
                .await
        }
    };
    let nodes: Vec<AxNode> = match serde_json::from_value(nodes_value) {
        Ok(nodes) => nodes,
        Err(err) => {
            return snapshot_fallback_to_js(state, &req, format!("parse AX nodes: {err}")).await
        }
    };
    let mut builder = SnapshotBuilder::new(req.offset, limit, req.details);
    let tree = builder.build(nodes);
    let refs_for_details: Vec<_> = builder
        .refs
        .values()
        .filter(|entry| entry.index >= req.offset && entry.index < end)
        .cloned()
        .collect();
    let mut all_refs = builder.refs;
    let detail_map =
        collect_dom_details(state, &tab_id, &refs_for_details, req.details, timeout).await;
    for (ref_id, detail) in detail_map {
        if let Some(entry) = all_refs.get_mut(&ref_id) {
            entry.dom = Some(detail);
        }
    }
    let tree = if req.details {
        attach_details_to_tree(tree, &all_refs)
    } else {
        tree
    };
    let tree = if req.interactive_only {
        prune_tree_keep_refs(tree)
    } else {
        tree
    };
    let tree = fold_tree(tree, req.max_nodes);
    let total = all_refs.len();
    let returned = all_refs
        .values()
        .filter(|entry| entry.index >= req.offset && entry.index < end)
        .count();
    // `active_tabs` 返回的是完整 URL，直接用它做快照基准，不再额外发一次 location.href 往返。
    let snapshot_url = tab.url.clone();
    {
        let mut driver = state.driver.write().await;
        let generation = driver
            .snapshots
            .get(&tab_id)
            .map(|cache| cache.generation + 1)
            .unwrap_or(1);
        driver.snapshots.insert(
            tab_id.clone(),
            SnapshotCache {
                generation,
                url: snapshot_url,
                refs: all_refs,
            },
        );
    }
    let has_more = end < total;
    Ok(json!({
        "status": "success",
        "mode": "cdp",
        "url": tab.url,
        "title": tab.title,
        "tab_id": tab.tab_id,
        "session_key": tab_id,
        "tree": tree,
        "pagination": {
            "total_operable": total,
            "offset": req.offset,
            "limit": limit,
            "returned": returned,
            "has_more": has_more,
            "next_offset": if has_more { Some(end) } else { None::<usize> }
        }
    }))
}

/// CDP 不可用时的回退：仍然产出 `@e` 引用，但引用带 CSS 选择器，
/// 后续动作全部走页面内执行，不再依赖 debugger。
async fn snapshot_fallback_to_js(
    state: &AppState,
    req: &SnapshotRequest,
    reason: String,
) -> Result<Value> {
    let reason = humanize_error(&reason);
    let mut value = match snapshot_via_js(state, req).await {
        Ok(value) => value,
        Err(err) => {
            return Err(anyhow!(
                "snapshot 失败：CDP 不可用（{reason}）；DOM 回退也失败（{}）。该页面同时禁用了 debugger 与 eval，请关闭在页面里注入 iframe 的其他扩展后再试",
                humanize_error(&err.to_string())
            ))
        }
    };
    value["fallback_reason"] = json!(reason);
    value["hint"] = json!("CDP 不可用（常见原因：页面内嵌其他扩展的 iframe），已回退到 DOM 快照；@e 可直接用于 click/fill/hover/select/check/scroll");
    Ok(value)
}

/// 扩展返回的错误经常是嵌套 JSON 字符串（`{"error":{"message":...}}`）。
/// 剥出人类可读的 message，避免把整段 JSON 塞进提示里。
fn humanize_error(input: &str) -> String {
    let mut text = input.trim().to_string();
    for _ in 0..3 {
        if !text.starts_with('{') {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            break;
        };
        let next = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| value.get("message").and_then(Value::as_str))
            .or_else(|| value.get("error").and_then(Value::as_str))
            .map(str::to_string);
        match next {
            Some(inner) => text = inner,
            None => break,
        }
    }
    text
}

/// 页面 CSP 禁止 eval 时扩展返回的错误特征。
///
/// 只认 CSP 相关的**精确短语**：裸子串 `csp` 会命中 `#csp-banner not found`、
/// `/csp-report`、`csp.example.com` 这类普通失败。一旦误判成 CSP 限制，
/// `page_action_with_fallback` / `run_target_operation` / `*_page_call` 会换通道
/// **再执行一次**，提交类按钮就被点了两次。
fn is_csp_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    const NEEDLES: [&str; 6] = [
        "unsafe-eval",
        "content security policy",
        "refused to evaluate",
        "refused to compile",
        "eval is disabled",
        // `EvalError`（小写后为 evalerror）：CSP 关闭 eval 时 Chrome 抛出的名字。
        "evalerror",
    ];
    NEEDLES.iter().any(|needle| lower.contains(needle))
}

/// 无 eval 通道：让扩展注入固定的页面脚本文件并调用其全局函数。
/// 页面 CSP 禁止 eval 时，read / find / snapshot(js) / action 仍可用（DOM 不受 CSP 限制）。
async fn page_op_via_extension(
    state: &AppState,
    session_key: &str,
    op: &str,
    args: Value,
    frame_id: Option<i64>,
    timeout: Duration,
) -> Result<Value> {
    let mut body = json!({ "op": op, "args": args });
    // 扩展侧 buildScriptTarget 认 frameId：不传就会在整个页面的主框架里执行。
    if let Some(frame_id) = frame_id {
        body["frameId"] = json!(frame_id);
    }
    let value = extension_control(state, session_key, "pageOp", body, timeout).await?;
    let data = value.get("data").cloned().unwrap_or(value);
    let payload = data.get("value").cloned().unwrap_or(Value::Null);
    // wait 的「未命中」是正常结果（ok:false + matched:false），不能当成命令失败。
    let has_match_field = payload.get("matched").is_some();
    if !has_match_field && payload.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(anyhow!(
            "pageOp({op}): {}",
            payload
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("page script failed")
        ));
    }
    Ok(payload)
}

/// 把会话的 `tab_id` 转成扩展协议里的数字 `tabId`。
///
/// 空字符串是历史合法值（表示"这条命令不需要 tabId"），保持 0；
/// 非空但解析不出正整数的**不能**静默降级成 0——扩展会回 "tabId is required"，
/// 用户看到的归因变成"扩展要求 tabId"，而不是"本地会话的 tab_id 坏了"。
fn parse_tab_id(raw: &str) -> Result<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    match trimmed.parse::<i64>() {
        Ok(id) if id > 0 => Ok(id),
        _ => Err(anyhow!(
            "会话的 tab_id 非法（{raw}），请 refresh 或重新 open"
        )),
    }
}

fn tab_id_of_session(session_key: &str) -> Value {
    json!(session_key.rsplit(':').next().unwrap_or_default())
}

async fn snapshot_via_js(state: &AppState, req: &SnapshotRequest) -> Result<Value> {
    let limit = req.limit.clamp(1, 1000);
    let offset = req.offset;
    let session_key = select_tab(
        state,
        SessionSelector::new(
            req.switch_tab_id.clone(),
            req.browser.clone(),
            req.profile.clone(),
        ),
    )
    .await?;
    let timeout = Duration::from_secs_f64(req.timeout.max(0.1));
    let end = page_window(offset, limit)?;
    let opts = json!({
        "limit": end.min(1000),
        "interactiveOnly": req.interactive_only
    });
    let script = format!(
        "{}\nreturn __abcSnapshot({});",
        html::js_page_snapshot(),
        opts
    );
    let executed = execute_page_js(
        state,
        &script,
        SessionSelector::new(Some(session_key.clone()), None, None),
        true,
        timeout,
    )
    .await;
    let (payload, ref_tab_id) = match executed {
        Ok(result) => (
            result.get("js_return").cloned().unwrap_or(Value::Null),
            result.get("tab_id").cloned().unwrap_or(Value::Null),
        ),
        Err(err) if is_csp_error(&err.to_string()) => {
            let payload =
                page_op_via_extension(state, &session_key, "snapshot", opts.clone(), None, timeout)
                    .await?;
            (payload, tab_id_of_session(&session_key))
        }
        Err(err) => return Err(err),
    };
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(anyhow!(
            "snapshot(js): {}",
            humanize_error(
                payload
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("page script failed")
            )
        ));
    }
    let items = payload
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = items.len();
    let mut refs: HashMap<String, ElementRef> = HashMap::new();
    let mut tree: Vec<Value> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let ref_id = item
            .get("ref")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if ref_id.is_empty() {
            continue;
        }
        let role = item
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("generic")
            .to_string();
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let selector = item
            .get("selector")
            .and_then(Value::as_str)
            .map(str::to_string);
        refs.insert(
            ref_id.clone(),
            ElementRef {
                ref_id: ref_id.clone(),
                backend_dom_node_id: 0,
                selector: selector.clone(),
                index,
                role: role.clone(),
                name: name.clone(),
                value: item
                    .get("value")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                description: None,
                dom: None,
            },
        );
        if index < offset || index >= end {
            continue;
        }
        let mut node = serde_json::Map::new();
        node.insert("ref".to_string(), json!(ref_id));
        node.insert("role".to_string(), json!(role));
        if !name.is_empty() {
            node.insert("name".to_string(), json!(name));
        }
        for key in [
            "value", "href", "checked", "disabled", "level", "tag", "rect",
        ] {
            if let Some(existing) = item.get(key) {
                if !existing.is_null() {
                    node.insert(key.to_string(), existing.clone());
                }
            }
        }
        node.insert("selector".to_string(), json!(selector));
        tree.push(Value::Object(node));
    }
    let mut tree = fold_tree(tree, req.max_nodes);
    let returned = tree.len();
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // `--js --details`：JS 快照本身没有 DOM 详情。按 ref 上的 CSS 选择器在页面内
    // 批量补一次（一次注入描述全部元素），输出形状与 CDP 模式一致：详情直接挂到
    // 树节点的同名字段上（tag/text/placeholder/rect/visible...）。
    let mut details_state = Value::Null;
    if req.details {
        let targets: Vec<ElementRef> = refs
            .values()
            .filter(|entry| entry.index >= offset && entry.index < end)
            .cloned()
            .collect();
        match collect_dom_details_via_js(state, &session_key, &targets, timeout).await {
            Ok(map) => {
                for (ref_id, detail) in map {
                    if let Some(entry) = refs.get_mut(&ref_id) {
                        entry.dom = Some(detail);
                    }
                }
                tree = attach_details_to_tree(tree, &refs);
            }
            // 补详情失败不该让整条快照失败：标注清楚，让调用方知道「没有详情」。
            Err(err) => {
                details_state = json!({
                    "details_supported": false,
                    "hint": format!(
                        "--js 模式下补详情失败（{}）；可去掉 --js 改走 CDP 快照",
                        humanize_error(&err.to_string())
                    )
                });
            }
        }
    }
    let in_window = refs
        .values()
        .filter(|entry| entry.index >= offset && entry.index < end)
        .count();
    {
        let mut driver = state.driver.write().await;
        let generation = driver
            .snapshots
            .get(&session_key)
            .map(|cache| cache.generation + 1)
            .unwrap_or(1);
        driver.snapshots.insert(
            session_key.clone(),
            SnapshotCache {
                generation,
                url: url.clone(),
                refs,
            },
        );
    }
    // 页面脚本只回传「抓到多少条」（受 limit 截断），并没有全页真实总数。
    // 以前把 items.len() 当成 total_operable，翻页时 has_more 会凭这个假总数判断；
    // 现在如实标注 scanned + total_known=false，has_more 只信页面给的 truncated。
    let truncated = payload
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let has_more = truncated || offset.saturating_add(in_window) < total;
    let mut pagination = json!({
        "scanned": total,
        "total_known": false,
        "offset": offset,
        "limit": limit,
        "returned": returned,
        "truncated": truncated,
        "has_more": has_more,
        "next_offset": if has_more { Some(end) } else { None::<usize> }
    });
    if !details_state.is_null() {
        pagination["details"] = details_state;
    }
    Ok(json!({
        "status": "success",
        "mode": "js",
        "url": url,
        "title": payload.get("title").cloned().unwrap_or(Value::Null),
        "tab_id": ref_tab_id,
        "session_key": session_key,
        "tree": tree,
        "pagination": pagination
    }))
}

struct SnapshotBuilder {
    offset: usize,
    limit: usize,
    operable_seen: usize,
    refs: HashMap<String, ElementRef>,
    nodes: HashMap<String, AxNode>,
}

impl SnapshotBuilder {
    fn new(offset: usize, limit: usize, _details: bool) -> Self {
        Self {
            offset,
            limit,
            operable_seen: 0,
            refs: HashMap::new(),
            nodes: HashMap::new(),
        }
    }

    fn build(&mut self, nodes: Vec<AxNode>) -> Vec<Value> {
        let mut child_ids = std::collections::HashSet::new();
        for node in &nodes {
            for child_id in &node.child_ids {
                child_ids.insert(child_id.clone());
            }
        }
        let mut roots: Vec<String> = nodes
            .iter()
            .filter(|node| !child_ids.contains(&node.node_id))
            .map(|node| node.node_id.clone())
            .collect();
        if roots.is_empty() {
            if let Some(first) = nodes.first() {
                roots.push(first.node_id.clone());
            }
        }
        self.nodes = nodes
            .into_iter()
            .map(|node| (node.node_id.clone(), node))
            .collect();
        roots
            .iter()
            .filter_map(|id| self.format_node(id, 0))
            .flat_map(|value| match value {
                Value::Array(items) => items,
                other => vec![other],
            })
            .collect()
    }

    fn format_node(&mut self, id: &str, depth: usize) -> Option<Value> {
        let (role, name, value, description, backend_dom_node_id, child_ids) = {
            let node = self.nodes.get(id)?;
            (
                ax_string(&node.role),
                ax_string(&node.name),
                ax_string(&node.value),
                ax_string(&node.description),
                node.backend_dom_node_id,
                node.child_ids.clone(),
            )
        };
        let mut children = Vec::new();
        // 深度上限：极端嵌套（或异常 AX 树里 child_ids 成环）时不继续下钻，
        // 只保留当前节点，避免无限递归把栈打爆。
        if depth < MAX_TREE_DEPTH {
            for child_id in child_ids {
                if let Some(child) = self.format_node(&child_id, depth + 1) {
                    match child {
                        Value::Array(items) => children.extend(items),
                        other => children.push(other),
                    }
                }
            }
        }
        let role_str = role.unwrap_or_default();
        let skip_role = role_str.is_empty() || role_str == "none" || role_str == "generic";
        let operable = is_operable_role(&role_str) && backend_dom_node_id.is_some();
        let mut obj = serde_json::Map::new();
        obj.insert("role".to_string(), json!(role_str));
        if let Some(name) = non_empty(name) {
            obj.insert("name".to_string(), json!(truncate_text(&name, 160)));
        }
        if let Some(value) = non_empty(value) {
            obj.insert("value".to_string(), json!(truncate_text(&value, 160)));
        }
        if let Some(description) = non_empty(description.clone()) {
            obj.insert(
                "description".to_string(),
                json!(truncate_text(&description, 160)),
            );
        }
        let mut include_self = false;
        if operable {
            self.operable_seen += 1;
            let index = self.operable_seen - 1;
            let ref_id = format!("@e{}", self.operable_seen);
            self.refs.insert(
                ref_id.clone(),
                ElementRef {
                    ref_id: ref_id.clone(),
                    backend_dom_node_id: backend_dom_node_id.unwrap_or_default(),
                    selector: None,
                    index,
                    role: role_str.clone(),
                    name: obj
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    value: obj.get("value").and_then(Value::as_str).map(str::to_string),
                    description,
                    dom: None,
                },
            );
            if index >= self.offset && index < self.offset.saturating_add(self.limit) {
                include_self = true;
                obj.insert("ref".to_string(), json!(ref_id));
            }
        }
        if skip_role {
            if children.is_empty() {
                None
            } else if children.len() == 1 {
                children.into_iter().next()
            } else {
                Some(Value::Array(children))
            }
        } else {
            if !children.is_empty() {
                obj.insert("children".to_string(), Value::Array(children));
            }
            if include_self || obj.contains_key("children") || role_is_context(&role_str) {
                Some(Value::Object(obj))
            } else {
                None
            }
        }
    }
}

fn attach_details_to_tree(mut tree: Vec<Value>, refs: &HashMap<String, ElementRef>) -> Vec<Value> {
    for item in &mut tree {
        attach_details_to_node(item, refs, 0);
    }
    tree
}

fn attach_details_to_node(node: &mut Value, refs: &HashMap<String, ElementRef>, depth: usize) {
    let Some(obj) = node.as_object_mut() else {
        return;
    };
    if let Some(ref_id) = obj.get("ref").and_then(Value::as_str) {
        if let Some(entry) = refs.get(ref_id).and_then(|entry| entry.dom.as_ref()) {
            if let Ok(Value::Object(map)) = serde_json::to_value(entry) {
                for (key, value) in map {
                    if !value.is_null() {
                        obj.insert(key, value);
                    }
                }
            }
        }
    }
    // 深度上限：超过阈值不再下钻（详情挂在更深的节点上就放弃，不影响树本身）。
    if depth >= MAX_TREE_DEPTH {
        return;
    }
    if let Some(children) = obj.get_mut("children").and_then(Value::as_array_mut) {
        for child in children {
            attach_details_to_node(child, refs, depth + 1);
        }
    }
}

/// 一次 batch 里最多合并多少个元素详情请求（每个元素 2 条 CDP 命令）。
const DETAIL_BATCH_SIZE: usize = 40;

async fn collect_dom_details(
    state: &AppState,
    tab_id: &str,
    refs: &[ElementRef],
    include_errors: bool,
    timeout: Duration,
) -> HashMap<String, ElementDomInfo> {
    let mut map = HashMap::with_capacity(refs.len());
    for chunk in refs.chunks(DETAIL_BATCH_SIZE) {
        match batch_dom_details(state, tab_id, chunk, timeout).await {
            Ok(details) => {
                for (entry, detail) in chunk.iter().zip(details) {
                    map.insert(entry.ref_id.clone(), detail);
                }
            }
            Err(err) => {
                let message = include_errors.then(|| err.to_string());
                for entry in chunk {
                    map.insert(entry.ref_id.clone(), empty_dom_info(message.clone()));
                }
            }
        }
    }
    map
}

/// 把 N 个元素的 `resolveNode + callFunctionOn` 合成一条 batch：
/// 原来每个元素一次 WS 往返，`--details --limit 200` 就是 200 次串行往返。
async fn batch_dom_details(
    state: &AppState,
    tab_id: &str,
    refs: &[ElementRef],
    timeout: Duration,
) -> Result<Vec<ElementDomInfo>> {
    let mut commands: Vec<Value> = Vec::with_capacity(refs.len() * 2);
    for entry in refs {
        let resolve_index = commands.len();
        commands.push(json!({
            "cmd": "cdp",
            "method": "DOM.resolveNode",
            "params": { "backendNodeId": entry.backend_dom_node_id }
        }));
        commands.push(json!({
            "cmd": "cdp",
            "method": "Runtime.callFunctionOn",
            "params": {
                "objectId": format!("${resolve_index}.object.objectId"),
                "functionDeclaration": ELEMENT_DOM_SCRIPT,
                "returnByValue": true
            }
        }));
    }
    let value = batch_cdp_call(state, tab_id, commands, timeout).await?;
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("snapshot: batch detail response missing results"))?;
    let mut details = Vec::with_capacity(refs.len());
    for index in 0..refs.len() {
        let payload = results
            .get(index * 2 + 1)
            .and_then(|item| item.get("result"))
            .and_then(|result| result.get("value"));
        match payload {
            Some(value) => details.push(parse_dom_info(value)),
            None => details.push(empty_dom_info(Some(
                "snapshot: DOM detail missing value".to_string(),
            ))),
        }
    }
    Ok(details)
}

/// `snapshot --js --details` 的详情来源。
///
/// JS 快照的 ref 只有 CSS 选择器、没有 `backendNodeId`（`DOM.resolveNode` 用不了），
/// 所以在页面内按选择器一次注入描述全部元素：一次往返，不依赖 CDP。
async fn collect_dom_details_via_js(
    state: &AppState,
    session_key: &str,
    refs: &[ElementRef],
    timeout: Duration,
) -> Result<HashMap<String, ElementDomInfo>> {
    let targets: Vec<Value> = refs
        .iter()
        .filter_map(|entry| {
            entry
                .selector
                .clone()
                .map(|selector| json!([entry.ref_id, selector]))
        })
        .collect();
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let script = format!(
        r#"const __abcDescribe = {describe};
const __abcTargets = {targets};
const __abcOut = {{}};
for (const __abcPair of __abcTargets) {{
  let __abcEl = null;
  try {{ __abcEl = document.querySelector(__abcPair[1]); }} catch (__abcErr) {{ __abcEl = null; }}
  if (!__abcEl) {{ __abcOut[__abcPair[0]] = {{ dom_error: 'element not found' }}; continue; }}
  try {{ __abcOut[__abcPair[0]] = __abcDescribe.call(__abcEl); }}
  catch (__abcErr) {{ __abcOut[__abcPair[0]] = {{ dom_error: String((__abcErr && __abcErr.message) || __abcErr) }}; }}
}}
return {{ ok: true, details: __abcOut }};"#,
        describe = ELEMENT_DOM_SCRIPT,
        targets = serde_json::to_string(&targets)?,
    );
    let executed = execute_page_js(
        state,
        &script,
        SessionSelector::new(Some(session_key.to_string()), None, None),
        true,
        timeout,
    )
    .await?;
    let payload = executed.get("js_return").cloned().unwrap_or(Value::Null);
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(anyhow!(
            "{}",
            humanize_error(
                payload
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("page script failed")
            )
        ));
    }
    let mut map = HashMap::new();
    if let Some(details) = payload.get("details").and_then(Value::as_object) {
        for (ref_id, value) in details {
            let info = match value.get("dom_error").and_then(Value::as_str) {
                Some(err) => empty_dom_info(Some(err.to_string())),
                None => parse_dom_info(value),
            };
            map.insert(ref_id.clone(), info);
        }
    }
    Ok(map)
}

fn empty_dom_info(dom_error: Option<String>) -> ElementDomInfo {
    ElementDomInfo {
        tag: None,
        text: None,
        placeholder: None,
        input_type: None,
        href: None,
        disabled: None,
        readonly: None,
        checked: None,
        selected: None,
        rect: None,
        selector: None,
        visible: None,
        dom_error,
    }
}

fn parse_dom_info(value: &Value) -> ElementDomInfo {
    let rect = value
        .get("rect")
        .and_then(|rect| serde_json::from_value::<RectInfo>(rect.clone()).ok());
    ElementDomInfo {
        tag: value.get("tag").and_then(Value::as_str).map(str::to_string),
        text: value
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string),
        placeholder: value
            .get("placeholder")
            .and_then(Value::as_str)
            .map(str::to_string),
        input_type: value
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string),
        href: value
            .get("href")
            .and_then(Value::as_str)
            .map(str::to_string),
        disabled: value.get("disabled").and_then(Value::as_bool),
        readonly: value.get("readonly").and_then(Value::as_bool),
        checked: value.get("checked").and_then(Value::as_bool),
        selected: value.get("selected").and_then(Value::as_bool),
        rect,
        selector: value
            .get("selector")
            .and_then(Value::as_str)
            .map(str::to_string),
        visible: value.get("visible").and_then(Value::as_bool),
        dom_error: None,
    }
}

const ELEMENT_DOM_SCRIPT: &str = r#"function() {
      const el = this;
      const rect = el.getBoundingClientRect ? el.getBoundingClientRect() : null;
      const style = el.ownerDocument.defaultView.getComputedStyle(el);
      const visible = !!rect && rect.width > 0 && rect.height > 0 && style.visibility !== 'hidden' && style.display !== 'none';
      function cssPath(node) {
        if (!node || node.nodeType !== 1) return '';
        if (node.id) return '#' + CSS.escape(node.id);
        const parts = [];
        let cur = node;
        while (cur && cur.nodeType === 1 && parts.length < 5) {
          let part = cur.localName;
          if (cur.classList && cur.classList.length) part += '.' + [...cur.classList].slice(0, 2).map(c => CSS.escape(c)).join('.');
          const parent = cur.parentElement;
          if (parent) {
            const same = [...parent.children].filter(c => c.localName === cur.localName);
            if (same.length > 1) part += `:nth-of-type(${same.indexOf(cur) + 1})`;
          }
          parts.unshift(part);
          cur = parent;
        }
        return parts.join(' > ');
      }
      const text = (el.innerText || el.textContent || '').trim().slice(0, 160);
      return {
        tag: el.tagName || '', text,
        placeholder: el.getAttribute && el.getAttribute('placeholder') || undefined,
        type: el.getAttribute && (el.getAttribute('type') || undefined),
        href: el.href ? String(el.href).slice(0, 300) : undefined,
        disabled: !!el.disabled,
        readonly: !!el.readOnly,
        checked: typeof el.checked === 'boolean' ? el.checked : undefined,
        selected: typeof el.selected === 'boolean' ? el.selected : undefined,
        rect: rect ? { x: rect.x, y: rect.y, width: rect.width, height: rect.height } : undefined,
        selector: cssPath(el), visible
      };
    }"#;

async fn run_target_operation(
    state: &AppState,
    kind: OperationKind,
    selector: SessionSelector,
    target: &str,
    payload: Option<Value>,
    options: OperationOptions,
) -> Result<Value> {
    let tab_id = select_tab(state, selector).await?;
    // `--monitor`：动作前在页面内装 MutationObserver（一次轻量注入），
    // 不再序列化整页 HTML 做前后 diff。装不上时要显式告诉调用方，不能静默。
    let monitor = if options.monitor {
        match start_mutation_watch(state, &tab_id).await {
            Some(monitor_id) => MonitorState::Armed(monitor_id),
            None => MonitorState::Unsupported,
        }
    } else {
        MonitorState::Off
    };
    // 动作体拆成独立函数：内部任何 `?` 提前返回都只会变成这里的 Err，
    // 由 `finish_operation` 统一收掉观察器。旧写法把回收只写在
    // `finish_operation` 的 Armed 分支里，动作链上任何一步提前报错
    // （页面动作 ok:false、wait-js 超时）都会把 observer 留在页面里，
    // 动画/轮询页面会一直累积 mutation 记录。
    let action = run_target_action(state, kind, &tab_id, target, payload, &options).await;
    finish_operation(state, tab_id, action, monitor, options).await
}

/// 真正的动作链：只负责「做什么」，不负责收尾。
///
/// 所有失败都以 `Err` 返回而不是就地 `return Err(...)` 之后跳过清理——
/// 观察器的回收统一由 `finish_operation` 处理，成功失败都会走到。
async fn run_target_action(
    state: &AppState,
    kind: OperationKind,
    tab_id: &str,
    target: &str,
    payload: Option<Value>,
    options: &OperationOptions,
) -> Result<Value> {
    let timeout = Duration::from_secs_f64(options.timeout.max(0.1));
    // 指定 frame 时只能用页面内动作：CDP 的 DOM.resolveNode 与 Input 事件永远作用于主框架。
    if let Some(frame_id) = options.frame_id {
        if matches!(kind, OperationKind::MouseClick) {
            return Err(anyhow!(
                "mouse-click 不支持 --frame：真实鼠标事件只能打在主框架的当前视口，页面内没有 frame 级实现"
            ));
        }
        if !is_ref_target(target) {
            let script = if matches!(kind, OperationKind::Fill) {
                "setValue"
            } else {
                "click"
            };
            let operation = page_action_with_fallback(
                state,
                tab_id,
                script,
                json!(target),
                payload.unwrap_or_else(|| json!({})),
                timeout,
                Some(frame_id),
            )
            .await?;
            ensure_page_action_ok(&operation)?;
            return Ok(operation);
        }
        // @e 引用：CDP 的 backendNodeId 只对主框架有效，必须换成 CSS 选择器走页面内动作。
        // 没有选择器时不能退回 CDP 分支，否则会点到主框架里同 id 的另一个节点。
        let lookup = resolve_ref_lookup(state, tab_id, target, timeout).await?;
        let relocated = lookup.as_ref().is_some_and(|value| value.relocated);
        let Some(selector) = lookup.as_ref().and_then(|value| value.selector.clone()) else {
            return Err(anyhow!(
                "该 @e 引用没有可用的 CSS 选择器，不支持跨 frame 操作；请先 snapshot（会生成 selector）或改用 CSS 选择器"
            ));
        };
        let script = if matches!(kind, OperationKind::Fill) {
            "setValue"
        } else {
            "click"
        };
        let operation = page_action_with_fallback(
            state,
            tab_id,
            script,
            json!(selector.clone()),
            payload.unwrap_or_else(|| json!({})),
            timeout,
            Some(frame_id),
        )
        .await?;
        ensure_page_action_ok(&operation)?;
        let mut operation = operation;
        if let Value::Object(map) = &mut operation {
            map.insert("ref_selector".to_string(), json!(selector));
            map.insert("frame_scoped".to_string(), json!(true));
            if relocated {
                map.insert("ref_relocated".to_string(), json!(true));
            }
        }
        return Ok(operation);
    }
    let lookup = resolve_ref_lookup(state, tab_id, target, timeout).await?;
    let relocated = lookup.as_ref().is_some_and(|value| value.relocated);
    // JS 快照产生的 @e 带 CSS 选择器：直接走页面内动作，避免为了一个 ref
    // 去调 CDP（在部分页面上 CDP 根本 attach 不上）。
    if let Some(selector) = lookup.as_ref().and_then(|value| value.selector.clone()) {
        let mut operation =
            page_action_for(kind, state, tab_id, selector.clone(), payload, timeout).await?;
        ensure_page_action_ok(&operation)?;
        if relocated {
            if let Value::Object(map) = &mut operation {
                map.insert("ref_relocated".to_string(), json!(true));
                map.insert("ref_selector".to_string(), json!(selector));
            }
        }
        return Ok(operation);
    }
    let object_id = match lookup {
        Some(value) => format!("__backend:{}", value.backend_dom_node_id),
        None => format!("__selector:{target}"),
    };
    let operation = match kind {
        OperationKind::Click => dom_click(state, tab_id, &object_id, timeout).await,
        OperationKind::Fill => {
            dom_fill(
                state,
                tab_id,
                &object_id,
                payload.clone().unwrap_or_else(|| json!({})),
                timeout,
            )
            .await
        }
        OperationKind::MouseClick => dom_mouse_click(state, tab_id, &object_id, timeout).await,
    };
    let operation = match operation {
        Ok(value) => value,
        Err(err) => {
            // SPA 重渲染后 backendNodeId 会失效；这里再试一次语义重定位，
            // 让「页面没换但节点被重建」的场景不必重新 snapshot。
            if let Some(selector) = retry_relocate_selector(state, tab_id, target, timeout).await {
                let mut value =
                    page_action_for(kind, state, tab_id, selector.clone(), payload, timeout)
                        .await?;
                ensure_page_action_ok(&value)?;
                if let Value::Object(map) = &mut value {
                    // `ref_relocated` 是布尔标记，新的选择器放 `ref_selector`。
                    map.insert("ref_relocated".to_string(), json!(true));
                    map.insert("ref_selector".to_string(), json!(selector));
                    map.insert("relocated_after_error".to_string(), json!(err.to_string()));
                }
                return Ok(value);
            }
            // CDP 不可用（其他扩展 iframe）且页面禁 eval 时，至少把 CSS 选择器类操作救回来。
            if !is_ref_target(target) {
                let (op, params) = match kind {
                    OperationKind::Fill => {
                        ("setValue", payload.clone().unwrap_or_else(|| json!({})))
                    }
                    _ => ("click", json!({})),
                };
                if let Ok(mut value) = page_action_with_fallback(
                    state,
                    tab_id,
                    op,
                    json!(target),
                    params,
                    timeout,
                    None,
                )
                .await
                {
                    if let Value::Object(map) = &mut value {
                        map.insert("via".to_string(), json!("no-eval"));
                        map.insert("cdp_error".to_string(), json!(err.to_string()));
                    }
                    // 页面脚本自己报 ok:false（target not found 等）时不能当成成功。
                    // 这是本函数第 5 条恢复路径，前 4 条（frame / ref / selector / 重定位）
                    // 都检查了，唯独这条曾经漏掉。
                    ensure_page_action_ok(&value)?;
                    return Ok(value);
                }
            }
            return Err(err);
        }
    };
    let mut operation = operation;
    if relocated {
        if let Value::Object(map) = &mut operation {
            map.insert("ref_relocated".to_string(), json!(true));
        }
    }
    Ok(operation)
}

/// 页面动作自己报告失败（例如 target not found）时，命令必须也失败，
/// 否则调用方会以为"点了/填了"，实际什么都没发生。
fn ensure_page_action_ok(operation: &Value) -> Result<()> {
    if operation.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(anyhow!(
            "{}",
            operation
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("page action failed")
        ));
    }
    Ok(())
}

async fn page_action_for(
    kind: OperationKind,
    state: &AppState,
    tab_id: &str,
    selector: String,
    payload: Option<Value>,
    timeout: Duration,
) -> Result<Value> {
    let (op, params) = match kind {
        OperationKind::Fill => ("setValue", payload.unwrap_or_else(|| json!({}))),
        _ => ("click", json!({})),
    };
    page_action_with_fallback(state, tab_id, op, json!(selector), params, timeout, None).await
}

/// 页面动作 + 无 eval 回退：先走「eval 一次页面脚本」，CSP 拦下时改走扩展注入固定文件。
async fn page_action_with_fallback(
    state: &AppState,
    session_key: &str,
    op: &str,
    target: Value,
    params: Value,
    timeout: Duration,
    frame_id: Option<i64>,
) -> Result<Value> {
    let executed = run_page_action_in_frame(
        state,
        session_key,
        op,
        target.clone(),
        params.clone(),
        timeout,
        frame_id,
    )
    .await;
    match executed {
        Ok(value) => Ok(value),
        Err(err) if is_csp_error(&err.to_string()) => {
            let payload = json!({ "op": op, "target": target, "params": params });
            page_op_via_extension(state, session_key, "action", payload, frame_id, timeout).await
        }
        Err(err) => Err(err),
    }
}

/// 引用在快照里，但 CDP/页面动作失败时，按 role+name 再定位一次。
async fn retry_relocate_selector(
    state: &AppState,
    session_key: &str,
    target: &str,
    timeout: Duration,
) -> Option<String> {
    if !is_ref_target(target) {
        return None;
    }
    let entry = {
        let driver = state.driver.read().await;
        driver.snapshots.get(session_key)?.refs.get(target)?.clone()
    };
    let selector = relocate_ref_selector(state, session_key, &entry, timeout).await?;
    let mut driver = state.driver.write().await;
    if let Some(cache) = driver.snapshots.get_mut(session_key) {
        if let Some(slot) = cache.refs.get_mut(target) {
            slot.selector = Some(selector.clone());
            slot.backend_dom_node_id = 0;
        }
    }
    Some(selector)
}

async fn run_send_keys_operation(
    state: &AppState,
    selector: SessionSelector,
    payload: Value,
    options: OperationOptions,
) -> Result<Value> {
    let tab_id = select_tab(state, selector).await?;
    // `--monitor`：动作前在页面内装 MutationObserver（一次轻量注入），
    // 不再序列化整页 HTML 做前后 diff。装不上时要显式告诉调用方，不能静默。
    let monitor = if options.monitor {
        match start_mutation_watch(state, &tab_id).await {
            Some(monitor_id) => MonitorState::Armed(monitor_id),
            None => MonitorState::Unsupported,
        }
    } else {
        MonitorState::Off
    };
    let frame_id = options.frame_id;
    // 与 run_target_operation 相同：动作体内部任何提前返回都交给 finish_operation
    // 统一收观察器，focus 失败也不会把 observer 留在页面里。
    let action = run_send_keys_action(state, &tab_id, payload, &options).await;
    let mut result = finish_operation(state, tab_id, action, monitor, options).await?;
    if frame_id.is_some() {
        result["frame_scoped"] = json!(true);
    }
    Ok(result)
}

async fn run_send_keys_action(
    state: &AppState,
    tab_id: &str,
    payload: Value,
    options: &OperationOptions,
) -> Result<Value> {
    let timeout = Duration::from_secs_f64(options.timeout.max(0.1));
    let frame_id = options.frame_id;
    if let Some(frame_id) = frame_id {
        // CDP 的 Input.dispatchKeyEvent 只会打到「当前聚焦文档」，所以必须先把焦点
        // 送进目标子 frame；没有 --target 就没有可控的落点，直接报错而不是静默打到主框架。
        let target = payload
            .get("target")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow!("send-keys: --frame 需配合 --target 才能把焦点送进子 frame（CDP 按键作用于当前聚焦文档）")
            })?;
        let selector = match resolve_ref_lookup(state, tab_id, target, timeout).await? {
            Some(lookup) => lookup.selector,
            None => None,
        };
        let focus_target = match selector {
            Some(selector) => selector,
            None if is_ref_target(target) => {
                return Err(anyhow!(
                    "该 @e 引用没有可用的 CSS 选择器，不支持跨 frame 操作；请先 snapshot（会生成 selector）或改用 CSS 选择器"
                ))
            }
            None => target.to_string(),
        };
        let focused = page_action_with_fallback(
            state,
            tab_id,
            "focus",
            json!(focus_target),
            json!({}),
            timeout,
            Some(frame_id),
        )
        .await?;
        ensure_page_action_ok(&focused)?;
    } else if let Some(target) = payload.get("target").and_then(Value::as_str) {
        let mut focused = None;
        if let Some(lookup) = resolve_ref_lookup(state, tab_id, target, timeout).await? {
            if let Some(selector) = lookup.selector {
                focused = Some(selector);
            }
        }
        match focused {
            Some(selector) => {
                let focused =
                    run_page_action(state, tab_id, "focus", json!(selector), json!({}), timeout)
                        .await?;
                // 页面动作可能以 ok:false 返回而不是 Err；焦点没进去就继续发键，
                // 键会落到当前实际聚焦的元素（常常是 body），命令却报成功。
                ensure_page_action_ok(&focused).map_err(|err| {
                    anyhow!("无法把焦点移到目标元素（{selector}），按键未发送：{err}")
                })?;
            }
            None => {
                let object_id = resolve_target_object(state, tab_id, target, timeout).await?;
                focus_object(state, tab_id, &object_id, timeout).await?;
            }
        }
    }
    dispatch_keys(
        state,
        tab_id,
        payload
            .get("keys")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        timeout,
    )
    .await
}

/// 收尾：等 wait-js、收 `--monitor` 观察器、组装返回体。
///
/// `action` 是动作链的结果：**失败也走这里**，先把观察器收掉再原样返回错误。
/// 这是的核心——旧实现只在成功路径回收，任何提前 `return Err`
/// （页面动作 ok:false、wait-js 超时）都会让 observer 与 mutation 缓冲
/// 跟着页面一直活着，动画/轮询页面会持续吃内存。
async fn finish_operation(
    state: &AppState,
    session_key: String,
    action: Result<Value>,
    monitor: MonitorState,
    options: OperationOptions,
) -> Result<Value> {
    let operation = match action {
        Ok(value) => value,
        Err(err) => {
            // 丢弃观察数据：动作已经失败，调用方要的是错误本身。
            let _ = collect_mutation_watch(state, &session_key, monitor).await;
            return Err(err);
        }
    };
    let physical_tab_id = session_tab_id(state, &session_key)
        .await
        .unwrap_or_default();
    let mut result = json!({ "status": "success", "tab_id": physical_tab_id, "session_key": session_key, "operation": operation });
    if let Some(wait_js) = options.wait_js.as_deref() {
        let wait = match wait_for_js_condition(
            state,
            Some(session_key.as_str()),
            wait_js,
            options.wait_timeout,
            options.wait_interval,
            None,
        )
        .await
        {
            Ok(value) => value,
            Err(err) => {
                // 等待本身失败属于错误路径，同样要回收观察器。
                let _ = collect_mutation_watch(state, &session_key, monitor).await;
                return Err(err);
            }
        };
        if !wait.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            let _ = collect_mutation_watch(state, &session_key, monitor).await;
            return Err(anyhow!(json!({ "error": format!("wait-js timeout after {}s", options.wait_timeout), "operation": result["operation"].clone(), "wait": wait }).to_string()));
        }
        result["wait"] = wait;
    }
    let (change, monitor_note) = collect_mutation_watch(state, &session_key, monitor).await;
    if let Some(change) = change {
        result["change"] = change;
    }
    if let Some(note) = monitor_note {
        result["monitor"] = note;
    }
    Ok(result)
}

/// 收掉一次 `--monitor` 观察器，返回 `(change, 降级说明)`。
///
/// 变更观察的输出结构与 `js --monitor` 的 `change` 同构
/// （changed/added/removed/attributes/text/samples），不回传原始 HTML 片段。
/// 三种降级都用 `monitor.reason` 给出机器可读原因，别让调用方去猜中文。
async fn collect_mutation_watch(
    state: &AppState,
    session_key: &str,
    monitor: MonitorState,
) -> (Option<Value>, Option<Value>) {
    match monitor {
        MonitorState::Off => (None, None),
        MonitorState::Unsupported => (
            None,
            Some(json!({
                "ok": false,
                "reason": "unsupported",
                "error": "CSP 或页面环境不支持变更观察（动作本身已执行）"
            })),
        ),
        MonitorState::Armed(monitor_id) => {
            match stop_mutation_watch(state, session_key, &monitor_id).await {
                Ok(MonitorOutcome::Collected(watch)) => (Some(watch), None),
                Ok(MonitorOutcome::Navigated) => {
                    // 导航本身就是「页面变了」：这个结论不需要观察器证明。
                    // 旧实现（前后整页对比）在这种情况下也是 changed:1。
                    // 零值键必须补齐：`change` 恒定有 6 个键，调用方才能无条件读
                    // `change.samples.length` / `added` 而不抛错（导航时没有明细）。
                    (
                        Some(json!({
                            "changed": 1,
                            "navigated": true,
                            "added": 0,
                            "removed": 0,
                            "attributes": 0,
                            "text": 0,
                            "samples": [],
                        })),
                        Some(json!({
                            "ok": false,
                            "reason": "navigated",
                            "error": "观察期间页面发生导航，变更计数以导航为准（无样本）"
                        })),
                    )
                }
                Ok(MonitorOutcome::Superseded) => (
                    None,
                    Some(json!({
                        "ok": false,
                        "reason": "superseded",
                        "error": "同一标签上有更新的 --monitor 动作覆盖了本次观察"
                    })),
                ),
                // 收数本身失败（注入被拒 / 上下文已销毁）：动作已经执行完，
                // 不能把整条命令判失败，但必须让调用方看出「观察没拿到」。
                Err(err) => (
                    None,
                    Some(json!({
                        "ok": false,
                        "reason": "injection_failed",
                        "error": err.to_string()
                    })),
                ),
            }
        }
    }
}

/// 在 daemon 侧轮询等待页面条件成立。
///
/// 不在页面里 `setTimeout` 循环：Chrome 会把隐藏（后台）标签页的定时器节流到
/// 1 秒甚至 1 分钟一次，页面内 sleep 会让后台任务几乎必然等到超时。
/// 每次轮询是一次独立脚本注入，不受标签页可见性影响。
async fn wait_for_js_condition(
    state: &AppState,
    session_key: Option<&str>,
    wait_js: &str,
    timeout: f64,
    interval: f64,
    cancel: Option<&AtomicBool>,
) -> Result<Value> {
    wait_for_js_condition_with_target(
        state,
        session_key,
        wait_js,
        timeout,
        interval,
        cancel,
        &ExecTargetOptions::default(),
    )
    .await
}

async fn wait_for_js_condition_with_target(
    state: &AppState,
    session_key: Option<&str>,
    wait_js: &str,
    timeout: f64,
    interval: f64,
    cancel: Option<&AtomicBool>,
    target: &ExecTargetOptions,
) -> Result<Value> {
    let timeout = timeout.max(0.0);
    let interval_ms = (interval.max(0.05) * 1000.0).round().max(1.0) as u64;
    let deadline = Instant::now() + Duration::from_millis((timeout * 1000.0).max(1.0) as u64);
    let mut injections: u64 = 0;
    let mut checks_total: u64 = 0;
    let mut last_value = Value::Null;
    let mut last_error: Option<String> = None;
    let mut repeated_error: u32 = 0;
    // 页面侧「几乎立刻返回但没命中」的连续次数（见下方 slice 逻辑）。
    let mut quick_returns: u32 = 0;

    // 谓词只在页面里编译一次、反复求值：DOM 变化由 MutationObserver 立即触发，
    // 非 DOM 条件由定时器兜底。daemon 这边正常只有一次注入（一次往返），
    // 只有换页/上下文销毁这类「注入本身失败」才按剩余时间重试。
    loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Ok(json!({
                "ok": false, "matched": false, "cancelled": true,
                "value": last_value, "error": "cancelled", "polls": checks_total.max(injections),
                "mode": "js-observer", "injections": injections
            }));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        // 可取消时按 ≤1s 切片：每次注入都会在页面里阻塞到该片的超时为止，
        // 若直接把剩余时间整段注入（20s 的 wait 就是一次 20s 的往返），
        // 顶部的 cancel 检查要等整段返回才轮到 —— 取消看起来「没反应」。
        // 同步调用（cancel 为 None）保持整段注入，避免多付往返。
        let slice_ms = if cancel.is_some() {
            (remaining.as_millis() as u64).clamp(1, 1_000)
        } else {
            (remaining.as_millis() as u64).max(1)
        };
        injections += 1;
        let opts = json!({
            "js": wait_js,
            "timeoutMs": slice_ms,
            "intervalMs": interval_ms,
        });
        let script = format!(
            "{}\nreturn await __abcWait({});",
            html::js_page_wait(),
            opts
        );
        let exec_timeout = Duration::from_millis(slice_ms + 3_000);
        match execute_raw_js_releasing_lock(state, &script, exec_timeout, session_key, target).await
        {
            Ok(execution) => {
                let payload = execution
                    .result
                    .data
                    .or(execution.result.result)
                    .unwrap_or(Value::Null);
                let checks = payload.get("checks").and_then(Value::as_u64).unwrap_or(0);
                checks_total += checks.max(1);
                if payload
                    .get("matched")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return Ok(json!({
                        "ok": true,
                        "matched": true,
                        "value": payload.get("value").cloned().unwrap_or(Value::Null),
                        "error": Value::Null,
                        "polls": checks_total,
                        "checks": checks_total,
                        "mode": payload.get("mode").cloned().unwrap_or(json!("js-observer")),
                        "injections": injections,
                        "elapsed_ms": payload.get("elapsed_ms").cloned().unwrap_or(Value::Null),
                    }));
                }
                // 页面侧已经等满本片的 timeoutMs 仍未命中。
                // 可取消时每片只有 1s，所以「本片超时」不等于「整体超时」：
                // 只要整体 deadline 还没到就继续下一片，让取消能在片间生效。
                let page_elapsed_ms = payload
                    .get("elapsed_ms")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                if let Some(value) = payload.get("value") {
                    last_value = value.clone();
                }
                if let Some(err) = payload.get("error").and_then(Value::as_str) {
                    last_error = Some(err.to_string());
                }
                if Instant::now() >= deadline {
                    break;
                }
                // 同步调用是「整段注入」，本片就等于整体，未命中即可判超时（保持旧行为）。
                if cancel.is_none() {
                    break;
                }
                // 可取消路径：本片只等了 1s，继续下一片，让取消在片间生效。
                // 但页面几乎没等就返回（观察器没装起来 / 谓词编译失败之类）时
                // 继续循环会热转，连续 3 次就放弃，避免空烧 CPU 到 deadline。
                let quick = slice_ms >= 200 && page_elapsed_ms < (slice_ms as f64) * 0.5;
                quick_returns = if quick { quick_returns + 1 } else { 0 };
                if quick_returns >= 3 {
                    break;
                }
            }
            Err(err) => {
                // 换页 / 执行上下文销毁 / 注入被拒：换页后条件可能已经成立，
                // 不能直接判失败，按剩余时间重试。
                let message = err.to_string();
                repeated_error = if last_error.as_deref() == Some(message.as_str()) {
                    repeated_error + 1
                } else {
                    1
                };
                last_error = Some(message);
                // 同一错误连续出现，多半是稳定失败（CSP 双通道都不可用等），
                // 没必要把超时时间全花在重试上。
                if repeated_error >= 3 {
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                tokio::time::sleep(remaining.min(Duration::from_millis(150))).await;
            }
        }
    }

    Ok(json!({
        "ok": false,
        "matched": false,
        "value": last_value,
        "error": last_error,
        "polls": checks_total.max(injections),
        "checks": checks_total,
        "mode": "js-observer",
        "injections": injections
    }))
}

async fn select_tab(state: &AppState, selector: SessionSelector) -> Result<String> {
    wait_for_sessions(state, WAIT_SESSIONS_ACTION).await;
    // 绝大多数命令都打在同一个标签上，默认会话早已指向它；这种情况下只拿读锁，
    // 不再为了「把同一个值再写一遍」抢排他锁，多个命令就能并发通过选标签这一步。
    let (session_key, needs_write, clear_preferred) = {
        let driver = state.driver.read().await;
        let (key, clear_preferred) = resolve_session(&driver, &selector)?;
        let needs_write = driver.default_session_key.as_deref() != Some(key.as_str())
            || (clear_preferred && driver.preferred_default_session_key.is_some());
        (key, needs_write, clear_preferred)
    };
    if needs_write {
        let mut driver = state.driver.write().await;
        driver.default_session_key = Some(session_key.clone());
        if clear_preferred {
            driver.preferred_default_session_key = None;
        }
    }
    Ok(session_key)
}

/// 纯计算版本的选择逻辑：不修改状态，只返回（会话 ID, 是否需要清掉 preferred）。
fn resolve_session(driver: &DriverState, selector: &SessionSelector) -> Result<(String, bool)> {
    let mut matches: Vec<&Session> = driver
        .sessions
        .values()
        .filter(|s| s.is_active())
        .filter(|s| {
            selector
                .browser
                .as_deref()
                .map(|b| s.browser_id == b)
                .unwrap_or(true)
        })
        .filter(|s| {
            selector
                .profile
                .as_deref()
                .map(|p| s.profile_id == p || s.profile_label.as_deref() == Some(p))
                .unwrap_or(true)
        })
        .collect();

    if let Some(tab_id) = selector.tab_id.as_deref() {
        matches.retain(|s| s.tab_id == tab_id || s.session_key == tab_id);
        if matches.is_empty() {
            return Err(anyhow!("会话ID {tab_id} 未连接"));
        }
        if matches.len() > 1 {
            return Err(anyhow!(
                "ambiguous tab {tab_id}, matched {} sessions; please specify --profile or --browser",
                matches.len()
            ));
        }
        return Ok((matches[0].session_key.clone(), false));
    }

    if let Some(profile) = selector.profile.as_deref() {
        let matched_profile_ids: HashSet<String> = matches
            .iter()
            .filter(|s| s.profile_label.as_deref() == Some(profile))
            .map(|s| s.profile_id.clone())
            .collect();
        if matched_profile_ids.len() > 1 {
            let mut ids: Vec<String> = matched_profile_ids.into_iter().collect();
            ids.sort();
            return Err(anyhow!(
                "profile label {profile:?} 存在歧义，匹配到多个 profile: {}",
                ids.join(", ")
            ));
        }
    }

    if selector.browser.is_some() || selector.profile.is_some() {
        // 选择必须是确定的：`matches` 来自 HashMap::values()，旧的 `matches.first()`
        // 会随迭代顺序变化——同一 profile 开多个标签时，同一条命令在 daemon 重启
        // 前后可能打到不同标签，而且这个随机结果还会被写成 default 让错误「粘住」。
        // 固定顺序：default_session_key → latest_session_key
        //          → connected_at 最大（最近连接）→ session_key 字典序最小。
        let preferred = driver
            .default_session_key
            .as_ref()
            .and_then(|key| matches.iter().find(|s| &s.session_key == key))
            .or_else(|| {
                driver
                    .latest_session_key
                    .as_ref()
                    .and_then(|key| matches.iter().find(|s| &s.session_key == key))
            })
            .or_else(|| {
                matches.iter().max_by(|left, right| {
                    let left_at = left.info.connected_at.unwrap_or(f64::NEG_INFINITY);
                    let right_at = right.info.connected_at.unwrap_or(f64::NEG_INFINITY);
                    left_at
                        .partial_cmp(&right_at)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        // 平手时取 session_key 字典序最小的：`max_by` 返回最后一个
                        // 最大值，所以这里反向比较字符串。
                        .then_with(|| right.session_key.cmp(&left.session_key))
                })
            });
        let session_key = preferred
            .map(|s| s.session_key.clone())
            .ok_or_else(|| anyhow!("没有匹配 --browser/--profile 的可用浏览器标签页"))?;
        return Ok((session_key, false));
    }

    // 优先使用“刚刚打开、尚未完成注册”的待生效会话，避免 open 后第一条命令打错标签页。
    if let Some(preferred) = driver.preferred_default_session_key.clone() {
        let active = driver
            .sessions
            .get(&preferred)
            .filter(|session| session.is_active())
            .map(|session| session.session_key.clone());
        if let Some(session_key) = active {
            return Ok((session_key, true));
        }
    }

    let session_key = driver
        .default_session_key
        .as_ref()
        .and_then(|key| {
            driver
                .sessions
                .get(key)
                .filter(|s| s.is_active())
                .map(|s| s.session_key.clone())
        })
        .or_else(|| {
            driver.latest_session_key.as_ref().and_then(|key| {
                driver
                    .sessions
                    .get(key)
                    .filter(|s| s.is_active())
                    .map(|s| s.session_key.clone())
            })
        })
        .or_else(|| {
            driver
                .sessions
                .values()
                .find(|s| s.is_active())
                .map(|s| s.session_key.clone())
        })
        .ok_or_else(|| anyhow!("没有可用的浏览器标签页"))?;
    Ok((session_key, false))
}

async fn session_tab_id(state: &AppState, session_key: &str) -> Result<String> {
    let driver = state.driver.read().await;
    driver
        .sessions
        .get(session_key)
        .filter(|s| s.is_active())
        .map(|s| s.tab_id.clone())
        .ok_or_else(|| anyhow!("会话ID {session_key} 未连接"))
}

async fn cdp_call(
    state: &AppState,
    session_key: &str,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<CdpCallResult> {
    let tab_id = session_tab_id(state, session_key).await?;
    let script = json!({ "cmd": "cdp", "tabId": parse_tab_id(&tab_id)?, "method": method, "params": params }).to_string();
    let response = execute_raw_js_on_tab(state, &script, session_key, timeout).await?;
    let value = response.data.or(response.result).unwrap_or(Value::Null);
    match value.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(CdpCallResult {
            ok: true,
            data: value.get("data").cloned(),
            error: None,
        }),
        Some(false) => Ok(CdpCallResult {
            ok: false,
            data: value.get("data").cloned(),
            error: value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| Some(value.to_string())),
        }),
        None => Ok(CdpCallResult {
            ok: true,
            data: Some(value),
            error: None,
        }),
    }
}

async fn execute_raw_js_on_tab(
    state: &AppState,
    code: &str,
    session_key: &str,
    timeout: Duration,
) -> Result<ExecResult> {
    // 显式携带 session_key，不再「临时改写全局 default_session_key → 执行 → 还原」。
    // 旧写法在 job 并发时会互相覆盖路由，导致命令打到别的标签页。
    Ok(
        execute_raw_js_with_session(state, code, timeout, Some(session_key))
            .await?
            .result,
    )
}

async fn batch_cdp_call(
    state: &AppState,
    session_key: &str,
    commands: Vec<Value>,
    timeout: Duration,
) -> Result<Value> {
    let tab_id = session_tab_id(state, session_key).await?;
    let script = json!({
        "cmd": "batch",
        "tabId": parse_tab_id(&tab_id)?,
        "commands": commands,
    })
    .to_string();
    let response = execute_raw_js_on_tab(state, &script, session_key, timeout).await?;
    let value = response.data.or(response.result).unwrap_or(Value::Null);
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(anyhow!(value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("batch cdp failed")
            .to_string()));
    }
    Ok(value)
}

async fn resolve_target_object(
    state: &AppState,
    tab_id: &str,
    target: &str,
    timeout: Duration,
) -> Result<String> {
    match resolve_ref_lookup(state, tab_id, target, timeout).await? {
        Some(lookup) => {
            if let Some(selector) = lookup.selector {
                Ok(format!("__selector:{selector}"))
            } else {
                Ok(format!("__backend:{}", lookup.backend_dom_node_id))
            }
        }
        None => Ok(format!("__selector:{target}")),
    }
}

/// `@e` 引用的解析结果：有 selector 时走页面内动作，否则退回 CDP backendNodeId。
struct RefLookup {
    selector: Option<String>,
    backend_dom_node_id: i64,
    /// true 表示这个引用是「按 role/name 在新页面上重新找到的」，不是快照原始节点。
    relocated: bool,
}

/// 解析 `@e` 引用，必要时自动重定位。
///
/// 三条路径：
/// 1. URL 未变 + 有 selector → 直接返回（零额外往返）
/// 2. URL 未变 + CDP 引用 → 返回 backendNodeId
/// 3. URL 变了（导航 / SPA 换页）→ 旧节点已失效，用 role+name 在当前页面重新定位
async fn resolve_ref_lookup(
    state: &AppState,
    session_key: &str,
    target: &str,
    timeout: Duration,
) -> Result<Option<RefLookup>> {
    if !is_ref_target(target) {
        return Ok(None);
    }
    let (entry, snapshot_url, current_url) = {
        let driver = state.driver.read().await;
        let cache = driver.snapshots.get(session_key).ok_or_else(|| {
            anyhow!("unknown_ref: {target} 不在 tab {session_key} 的快照里，先执行 snapshot --tab {session_key}")
        })?;
        let entry = cache.refs.get(target).cloned().ok_or_else(|| {
            anyhow!(
                "unknown_ref: {target} 不在 tab {session_key} 的快照里（快照共 {} 个引用），先重新 snapshot",
                cache.refs.len()
            )
        })?;
        (
            entry,
            cache.url.clone(),
            driver
                .sessions
                .get(session_key)
                .map(|session| session.info.url.clone())
                .unwrap_or_default(),
        )
    };

    let url_changed =
        !snapshot_url.is_empty() && !current_url.is_empty() && current_url != snapshot_url;
    if !url_changed {
        return Ok(Some(RefLookup {
            selector: entry.selector.clone(),
            backend_dom_node_id: entry.backend_dom_node_id,
            relocated: false,
        }));
    }

    // 页面已导航：backendNodeId 和旧 selector 都可能失效，按语义重新找。
    let relocated = relocate_ref_selector(state, session_key, &entry, timeout).await;
    match relocated {
        Some(selector) => {
            {
                let mut driver = state.driver.write().await;
                if let Some(cache) = driver.snapshots.get_mut(session_key) {
                    if let Some(slot) = cache.refs.get_mut(target) {
                        slot.selector = Some(selector.clone());
                        slot.backend_dom_node_id = 0;
                    }
                }
            }
            Ok(Some(RefLookup {
                selector: Some(selector),
                backend_dom_node_id: 0,
                relocated: true,
            }))
        }
        None => Err(anyhow!(
            "ref_expired: {target}（{}「{}」）属于旧页面 {snapshot_url}，当前页面是 {current_url}，且按 role/name 未能在新页面找到同名元素；请重新执行 snapshot",
            entry.role,
            entry.name
        )),
    }
}

/// 在当前页面按 role + name 重新定位引用，返回新的 CSS 选择器。
async fn relocate_ref_selector(
    state: &AppState,
    session_key: &str,
    entry: &ElementRef,
    timeout: Duration,
) -> Option<String> {
    let want_name = normalize_ref_name(&entry.name);
    if want_name.is_empty() {
        return None;
    }
    let opts = json!({ "limit": 600, "interactiveOnly": false });
    let script = format!(
        "{}\nreturn __abcSnapshot({});",
        html::js_page_snapshot(),
        opts
    );
    let result = execute_page_js(
        state,
        &script,
        SessionSelector::new(Some(session_key.to_string()), None, None),
        true,
        timeout,
    )
    .await
    .ok()?;
    let payload = result.get("js_return")?.clone();
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let items = payload.get("items")?.as_array()?;
    let want_role = entry.role.to_lowercase();
    let mut fallback: Option<String> = None;
    for item in items {
        let role = item
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        let name = normalize_ref_name(item.get("name").and_then(Value::as_str).unwrap_or(""));
        if name != want_name {
            continue;
        }
        let selector = item
            .get("selector")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if selector.is_empty() {
            continue;
        }
        if role == want_role {
            return Some(selector.to_string());
        }
        if fallback.is_none() {
            fallback = Some(selector.to_string());
        }
    }
    fallback
}

fn normalize_ref_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_ref_target(target: &str) -> bool {
    let Some(rest) = target.strip_prefix("@e") else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

async fn call_function_resolved(
    state: &AppState,
    tab_id: &str,
    object_id: &str,
    function_declaration: &str,
    timeout: Duration,
) -> Result<Value> {
    let value = if let Some(raw) = object_id.strip_prefix("__backend:") {
        let backend: i64 = raw.parse()?;
        batch_cdp_call(
            state,
            tab_id,
            vec![
                json!({ "cmd": "cdp", "method": "DOM.resolveNode", "params": { "backendNodeId": backend } }),
                json!({ "cmd": "cdp", "method": "Runtime.callFunctionOn", "params": { "objectId": "$0.object.objectId", "functionDeclaration": function_declaration, "returnByValue": true } }),
            ],
            timeout,
        )
        .await?
    } else if let Some(selector) = object_id.strip_prefix("__selector:") {
        let expression = format!(
            "document.querySelector({})",
            serde_json::to_string(selector)?
        );
        batch_cdp_call(
            state,
            tab_id,
            vec![
                json!({ "cmd": "cdp", "method": "Runtime.evaluate", "params": { "expression": expression, "returnByValue": false } }),
                json!({ "cmd": "cdp", "method": "Runtime.callFunctionOn", "params": { "objectId": "$0.result.objectId", "functionDeclaration": function_declaration, "returnByValue": true } }),
            ],
            timeout,
        )
        .await?
    } else {
        return call_function_value(state, tab_id, object_id, function_declaration, timeout).await;
    };
    value
        .get("results")
        .and_then(Value::as_array)
        .and_then(|items| items.get(1))
        .or_else(|| value.as_array().and_then(|items| items.get(1)))
        .and_then(|data| data.get("result").and_then(|r| r.get("value")).cloned())
        .ok_or_else(|| anyhow!("Runtime.callFunctionOn response missing value"))
}

async fn dom_click(
    state: &AppState,
    tab_id: &str,
    object_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let script = r#"function() {
      this.scrollIntoView({ block: 'center', inline: 'center' });
      this.click();
      return { success: true, tag: this.tagName, text: (this.textContent || '').slice(0, 100) };
    }"#;
    call_function_resolved(state, tab_id, object_id, script, timeout).await
}

async fn dom_fill(
    state: &AppState,
    tab_id: &str,
    object_id: &str,
    payload: Value,
    timeout: Duration,
) -> Result<Value> {
    let value = payload
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let append = payload
        .get("append")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let script = format!(
        r#"function() {{
      const __value = {value_json};
      const __append = {append};
      const el = this;
      el.focus();
      const role = el.getAttribute && (el.getAttribute('role') || '').toLowerCase();
      const tag = (el.tagName || '').toLowerCase();
      const editable = el.isContentEditable || tag === 'input' || tag === 'textarea' || role === 'textbox' || role === 'searchbox';
      if (!editable) return {{ error: 'fill target is not editable' }};
      if (el.isContentEditable) {{
        if (!__append) el.textContent = '';
        const sel = window.getSelection();
        if (sel) {{ const range = document.createRange(); range.selectNodeContents(el); range.collapse(false); sel.removeAllRanges(); sel.addRange(range); }}
        let inserted = false;
        try {{ inserted = document.execCommand('insertText', false, __value); }} catch (_) {{ inserted = false; }}
        if (!inserted) {{ el.textContent = (__append ? el.textContent : '') + __value; el.dispatchEvent(new InputEvent('input', {{ inputType: 'insertText', data: __value, bubbles: true }})); }}
        el.dispatchEvent(new Event('change', {{ bubbles: true }}));
        return {{ success: true, tag: el.tagName, mode: 'contenteditable', value_length: __value.length }};
      }}
      if (tag === 'input' || tag === 'textarea') {{
        const setter = Object.getOwnPropertyDescriptor(tag === 'textarea' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype, 'value')?.set;
        const next = __append ? (el.value || '') + __value : __value;
        if (setter) setter.call(el, next); else el.value = next;
        el.dispatchEvent(new Event('input', {{ bubbles: true }}));
        el.dispatchEvent(new Event('change', {{ bubbles: true }}));
        return {{ success: true, tag: el.tagName, mode: 'value', value_length: __value.length }};
      }}
      try {{ document.execCommand('insertText', false, __value); }} catch (_) {{}}
      el.dispatchEvent(new Event('input', {{ bubbles: true }}));
      el.dispatchEvent(new Event('change', {{ bubbles: true }}));
      return {{ success: true, tag: el.tagName, mode: 'role-textbox', value_length: __value.length }};
    }}"#,
        value_json = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
        append = append,
    );
    let value = call_function_resolved(state, tab_id, object_id, &script, timeout).await?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(anyhow!(error.to_string()));
    }
    Ok(value)
}

async fn dom_mouse_click(
    state: &AppState,
    tab_id: &str,
    object_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let box_data = if let Some(raw) = object_id.strip_prefix("__backend:") {
        let backend: i64 = raw.parse()?;
        batch_cdp_call(
            state,
            tab_id,
            vec![
                json!({ "cmd": "cdp", "method": "DOM.resolveNode", "params": { "backendNodeId": backend } }),
                json!({ "cmd": "cdp", "method": "Runtime.callFunctionOn", "params": { "objectId": "$0.object.objectId", "functionDeclaration": "function() { this.scrollIntoView({ block: 'center', inline: 'center' }); }", "returnByValue": true } }),
                json!({ "cmd": "cdp", "method": "Runtime.callFunctionOn", "params": { "objectId": "$0.object.objectId", "functionDeclaration": "function() { const r = this.getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; }", "returnByValue": true } }),
            ],
            timeout,
        )
        .await?
    } else if let Some(selector) = object_id.strip_prefix("__selector:") {
        let expression = format!(
            "document.querySelector({})",
            serde_json::to_string(selector)?
        );
        batch_cdp_call(
            state,
            tab_id,
            vec![
                json!({ "cmd": "cdp", "method": "Runtime.evaluate", "params": { "expression": expression, "returnByValue": false } }),
                json!({ "cmd": "cdp", "method": "Runtime.callFunctionOn", "params": { "objectId": "$0.result.objectId", "functionDeclaration": "function() { this.scrollIntoView({ block: 'center', inline: 'center' }); }", "returnByValue": true } }),
                json!({ "cmd": "cdp", "method": "Runtime.callFunctionOn", "params": { "objectId": "$0.result.objectId", "functionDeclaration": "function() { const r = this.getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; }", "returnByValue": true } }),
            ],
            timeout,
        )
        .await?
    } else {
        focus_scroll(state, tab_id, object_id, timeout).await?;
        let result = cdp_call(
            state,
            tab_id,
            "DOM.getBoxModel",
            json!({ "objectId": object_id }),
            timeout,
        )
        .await?;
        json!({ "results": [null, null, result.data.unwrap_or(Value::Null)] })
    };
    let rect = box_data
        .get("results")
        .and_then(Value::as_array)
        .and_then(|items| items.get(2))
        .and_then(|data| data.get("result").and_then(|r| r.get("value")).cloned())
        .or_else(|| {
            box_data
                .as_array()
                .and_then(|items| items.get(2))
                .and_then(|data| data.get("result").and_then(|r| r.get("value")).cloned())
        });
    let (x, y) = if let Some(rect) = rect {
        let width = rect
            .get("width")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        let height = rect
            .get("height")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        if width <= 0.0 || height <= 0.0 {
            return Err(anyhow!("mouse-click: element has no layout box (display:none / detached / zero-size). Use click for DOM-level fallback."));
        }
        (
            rect.get("x").and_then(Value::as_f64).unwrap_or_default() + width / 2.0,
            rect.get("y").and_then(Value::as_f64).unwrap_or_default() + height / 2.0,
        )
    } else {
        let content = box_data
            .get("results")
            .and_then(Value::as_array)
            .and_then(|items| items.get(2))
            .and_then(|data| data.get("model").and_then(|m| m.get("content")).cloned())
            .and_then(|v| v.as_array().cloned())
            .ok_or_else(|| anyhow!("mouse-click: element has no layout box (display:none / detached / zero-size). Use click for DOM-level fallback."))?;
        if content.len() < 8 {
            return Err(anyhow!("mouse-click: element has no layout box (display:none / detached / zero-size). Use click for DOM-level fallback."));
        }
        let n = |idx: usize| content.get(idx).and_then(Value::as_f64).unwrap_or_default();
        (
            (n(0) + n(2) + n(4) + n(6)) / 4.0,
            (n(1) + n(3) + n(5) + n(7)) / 4.0,
        )
    };
    // 三次事件合并成一条 batch：原来每次一个 WS 往返（还各带一次 alarm 抖动）。
    // 同时补 pointerType，避免部分站点依赖 pointer 事件判定真实用户操作。
    let commands = vec![
        json!({ "cmd": "cdp", "method": "Input.dispatchMouseEvent", "params": { "type": "mouseMoved", "x": x, "y": y, "button": "none", "buttons": 0, "pointerType": "mouse" } }),
        json!({ "cmd": "cdp", "method": "Input.dispatchMouseEvent", "params": { "type": "mousePressed", "x": x, "y": y, "button": "left", "buttons": 1, "clickCount": 1, "pointerType": "mouse" } }),
        json!({ "cmd": "cdp", "method": "Input.dispatchMouseEvent", "params": { "type": "mouseReleased", "x": x, "y": y, "button": "left", "buttons": 0, "clickCount": 1, "pointerType": "mouse" } }),
    ];
    batch_cdp_call(state, tab_id, commands, timeout).await?;
    Ok(json!({ "success": true, "x": x.round(), "y": y.round() }))
}

async fn focus_object(
    state: &AppState,
    tab_id: &str,
    object_id: &str,
    timeout: Duration,
) -> Result<()> {
    let script = "function() { this.focus && this.focus(); return true; }";
    call_function_resolved(state, tab_id, object_id, script, timeout).await?;
    Ok(())
}

async fn focus_scroll(
    state: &AppState,
    tab_id: &str,
    object_id: &str,
    timeout: Duration,
) -> Result<()> {
    let script =
        "function() { this.scrollIntoView({ block: 'center', inline: 'center' }); return true; }";
    call_function_resolved(state, tab_id, object_id, script, timeout).await?;
    Ok(())
}

async fn call_function_value(
    state: &AppState,
    tab_id: &str,
    object_id: &str,
    function_declaration: &str,
    timeout: Duration,
) -> Result<Value> {
    let result = cdp_call(
        state,
        tab_id,
        "Runtime.callFunctionOn",
        json!({ "objectId": object_id, "functionDeclaration": function_declaration, "returnByValue": true }),
        timeout,
    )
    .await?;
    if !result.ok {
        return Err(anyhow!(result
            .error
            .unwrap_or_else(|| "Runtime.callFunctionOn failed".to_string())));
    }
    let data = result.data.unwrap_or(Value::Null);
    if let Some(details) = data.get("exceptionDetails") {
        return Err(anyhow!(details.to_string()));
    }
    Ok(data
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(Value::Null))
}

async fn dispatch_keys(
    state: &AppState,
    tab_id: &str,
    keys: &str,
    timeout: Duration,
) -> Result<Value> {
    let platform = if cfg!(target_os = "macos") {
        "mac"
    } else {
        "other"
    };
    let segments: Vec<&str> = keys.split_whitespace().collect();
    if segments.is_empty() {
        return Err(anyhow!("send-keys: keys is required"));
    }
    let mut dispatched = 0;
    for segment in segments {
        let key = parse_key_segment(segment, platform)?;
        dispatch_key_segment(state, tab_id, key, timeout).await?;
        dispatched += 1;
    }
    Ok(json!({ "success": true, "dispatched": dispatched }))
}

#[derive(Debug, Clone)]
struct KeySegment {
    modifiers: Vec<KeySpec>,
    modifier_bits: i64,
    key: KeySpec,
}

#[derive(Debug, Clone)]
struct KeySpec {
    bit: i64,
    key: String,
    code: String,
    vkc: i64,
    text: Option<String>,
}

fn parse_key_segment(segment: &str, platform: &str) -> Result<KeySegment> {
    let parts: Vec<&str> = segment
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return Err(anyhow!("send-keys: empty segment"));
    }
    let mut modifier_bits = 0;
    let mut modifiers = Vec::new();
    for part in &parts[..parts.len().saturating_sub(1)] {
        let spec = modifier_spec(part, platform)
            .ok_or_else(|| anyhow!("send-keys: {part} is not a modifier"))?;
        modifier_bits |= spec.bit;
        modifiers.push(spec);
    }
    let mut key = key_spec(parts[parts.len() - 1])?;
    if modifier_bits & 8 != 0
        && key.key.len() == 1
        && key.key.chars().all(|c| c.is_ascii_lowercase())
    {
        key.key = key.key.to_uppercase();
        key.text = key.text.as_ref().map(|_| key.key.clone());
    }
    Ok(KeySegment {
        modifiers,
        modifier_bits,
        key,
    })
}

fn modifier_spec(input: &str, platform: &str) -> Option<KeySpec> {
    let name = input.to_ascii_lowercase();
    let (bit, key, code, vkc) = match name.as_str() {
        "alt" => (1, "Alt", "AltLeft", 18),
        "ctrl" | "control" => (2, "Control", "ControlLeft", 17),
        "cmd" | "meta" => (4, "Meta", "MetaLeft", 91),
        "shift" => (8, "Shift", "ShiftLeft", 16),
        "mod" if platform == "mac" => (4, "Meta", "MetaLeft", 91),
        "mod" => (2, "Control", "ControlLeft", 17),
        _ => return None,
    };
    Some(KeySpec {
        bit,
        key: key.to_string(),
        code: code.to_string(),
        vkc,
        text: None,
    })
}

fn key_spec(input: &str) -> Result<KeySpec> {
    let name = input.to_ascii_lowercase();
    let (key, code, vkc, text) = match name.as_str() {
        "enter" | "return" => (
            "Enter".to_string(),
            "Enter".to_string(),
            13,
            Some("\r".to_string()),
        ),
        "escape" | "esc" => ("Escape".to_string(), "Escape".to_string(), 27, None),
        "tab" => ("Tab".to_string(), "Tab".to_string(), 9, None),
        "backspace" => ("Backspace".to_string(), "Backspace".to_string(), 8, None),
        "delete" => ("Delete".to_string(), "Delete".to_string(), 46, None),
        "space" => (
            " ".to_string(),
            "Space".to_string(),
            32,
            Some(" ".to_string()),
        ),
        "arrowup" | "up" => ("ArrowUp".to_string(), "ArrowUp".to_string(), 38, None),
        "arrowdown" | "down" => ("ArrowDown".to_string(), "ArrowDown".to_string(), 40, None),
        "arrowleft" | "left" => ("ArrowLeft".to_string(), "ArrowLeft".to_string(), 37, None),
        "arrowright" | "right" => ("ArrowRight".to_string(), "ArrowRight".to_string(), 39, None),
        "home" => ("Home".to_string(), "Home".to_string(), 36, None),
        "end" => ("End".to_string(), "End".to_string(), 35, None),
        _ if input.len() == 1 && input.chars().all(|c| c.is_ascii_alphabetic()) => {
            let lower = input.to_ascii_lowercase();
            let upper = input.to_ascii_uppercase();
            (
                lower.clone(),
                format!("Key{upper}"),
                upper.as_bytes()[0] as i64,
                Some(lower),
            )
        }
        _ if input.len() == 1 && input.chars().all(|c| c.is_ascii_digit()) => (
            input.to_string(),
            format!("Digit{input}"),
            input.as_bytes()[0] as i64,
            Some(input.to_string()),
        ),
        _ => return Err(anyhow!("send-keys: unknown key {input}")),
    };
    Ok(KeySpec {
        bit: 0,
        key,
        code,
        vkc,
        text,
    })
}

async fn dispatch_key_segment(
    state: &AppState,
    tab_id: &str,
    segment: KeySegment,
    timeout: Duration,
) -> Result<()> {
    let KeySegment {
        modifiers,
        modifier_bits,
        key,
    } = segment;
    // 修饰键的 keyUp 必须无条件执行：CDP 的按键状态是浏览器级的，漏抬一次
    // Ctrl/Shift/Meta，此后该标签的所有操作都等同按住修饰键。
    // 所以「按下」逐条记录，「抬起」统一收尾，中间任何一步失败都不跳过释放。
    let mut active_bits = 0;
    let mut pressed: Vec<KeySpec> = Vec::with_capacity(modifiers.len());
    let mut failure: Option<anyhow::Error> = None;

    for modifier in &modifiers {
        active_bits |= modifier.bit;
        match dispatch_key_event(
            state,
            tab_id,
            "keyDown",
            active_bits,
            modifier,
            None,
            timeout,
        )
        .await
        {
            Ok(()) => pressed.push(modifier.clone()),
            Err(err) => {
                failure = Some(err);
                break;
            }
        }
    }

    if failure.is_none() {
        let text = if modifier_bits & !8 == 0 {
            key.text.as_deref()
        } else {
            None
        };
        match dispatch_key_event(state, tab_id, "keyDown", modifier_bits, &key, text, timeout).await
        {
            Ok(()) => {
                if let Err(err) =
                    dispatch_key_event(state, tab_id, "keyUp", modifier_bits, &key, None, timeout)
                        .await
                {
                    failure = Some(err);
                }
            }
            Err(err) => failure = Some(err),
        }
    }

    // 收尾：按相反顺序抬起所有已经按下的修饰键，best-effort，不覆盖首个错误。
    let mut release_failed = false;
    for modifier in pressed.iter().rev() {
        active_bits &= !modifier.bit;
        if dispatch_key_event(state, tab_id, "keyUp", active_bits, modifier, None, timeout)
            .await
            .is_err()
        {
            release_failed = true;
        }
    }

    match failure {
        None => Ok(()),
        Some(err) if release_failed => Err(anyhow!(
            "{err}（已尝试释放修饰键，但释放失败，修饰键状态可能残留）"
        )),
        Some(err) => Err(anyhow!("{err}（已释放已按下的修饰键）")),
    }
}

async fn dispatch_key_event(
    state: &AppState,
    tab_id: &str,
    event_type: &str,
    modifiers: i64,
    spec: &KeySpec,
    text: Option<&str>,
    timeout: Duration,
) -> Result<()> {
    let mut params = json!({ "type": event_type, "modifiers": modifiers, "key": spec.key, "code": spec.code, "windowsVirtualKeyCode": spec.vkc });
    if let Some(text) = text {
        params["text"] = json!(text);
    }
    let result = cdp_call(state, tab_id, "Input.dispatchKeyEvent", params, timeout).await?;
    if !result.ok {
        return Err(anyhow!(result
            .error
            .unwrap_or_else(|| "send-keys failed".to_string())));
    }
    Ok(())
}

fn ax_string(value: &Option<AxValue>) -> Option<String> {
    value.as_ref().and_then(|v| {
        let value = v.value.as_ref()?;
        let is_string_like = matches!(
            v.value_type.as_deref(),
            Some("string")
                | Some("computedString")
                | Some("token")
                | Some("internalRole")
                | Some("role")
        );
        Some(if is_string_like {
            value.as_str().unwrap_or_default().to_string()
        } else if let Some(s) = value.as_str() {
            s.to_string()
        } else if let Some(n) = value.as_i64() {
            n.to_string()
        } else if let Some(n) = value.as_u64() {
            n.to_string()
        } else if let Some(n) = value.as_f64() {
            n.to_string()
        } else if let Some(b) = value.as_bool() {
            b.to_string()
        } else {
            value.to_string().trim_matches('"').to_string()
        })
    })
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn truncate_text(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        format!("{}...", value.chars().take(max).collect::<String>())
    }
}

fn is_operable_role(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "link"
            | "textbox"
            | "searchbox"
            | "checkbox"
            | "radio"
            | "combobox"
            | "listbox"
            | "menuitem"
            | "menuitemcheckbox"
            | "menuitemradio"
            | "option"
            | "slider"
            | "spinbutton"
            | "switch"
            | "tab"
            | "treeitem"
    )
}

fn role_is_context(role: &str) -> bool {
    matches!(
        role,
        "form" | "dialog" | "alert" | "heading" | "main" | "navigation" | "region"
    )
}

/// 扩展控制命令的默认超时。
///
/// 只有「调用方没有自己的超时语义」的简单控制命令（cookies / frames /
/// network 启停）才用它兜底；pageOp 等长命令一律传调用方的 timeout，
/// 否则 `snapshot --timeout 300` 落到 CSP 回退通道上会被硬编码的 30s 截断。
const DEFAULT_EXTENSION_CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

async fn extension_control(
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
fn humanize_extension_error(raw: &str) -> String {
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

async fn network_start(State(state): State<AppState>, Json(req): Json<TabRequest>) -> Json<Value> {
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

async fn network_list(
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

async fn network_detail(
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

async fn network_clear(State(state): State<AppState>, Json(req): Json<TabRequest>) -> Json<Value> {
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

async fn network_stop(State(state): State<AppState>, Json(req): Json<TabRequest>) -> Json<Value> {
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

async fn console_start(State(state): State<AppState>, Json(req): Json<TabRequest>) -> Json<Value> {
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

async fn console_list(
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

async fn console_clear(State(state): State<AppState>, Json(req): Json<TabRequest>) -> Json<Value> {
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

async fn console_stop(State(state): State<AppState>, Json(req): Json<TabRequest>) -> Json<Value> {
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

const JOB_OPS: &[&str] = &[
    "exec",
    "scan",
    "read",
    "find",
    "inject",
    "frames",
    "downloads",
    "net",
    "action",
    "wait",
    "cookies",
    "snapshot",
    "click",
    "fill",
    "mouse-click",
    "send-keys",
    "upload",
    "screenshot",
    "save-pdf",
];

async fn run_steps(State(state): State<AppState>, Json(req): Json<RunRequest>) -> Json<Value> {
    touch(&state).await;
    if req.steps.is_empty() {
        return Json(json!({
            "ok": false,
            "error": "run: steps 不能为空",
            "error_code": "empty_steps"
        }));
    }

    let mut results: Vec<Value> = Vec::with_capacity(req.steps.len());
    let mut stopped_at: Option<usize> = None;
    for (index, step) in req.steps.iter().enumerate() {
        let started = Instant::now();
        let outcome = run_job(
            &state,
            &step.op,
            step.args.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(value) => results.push(json!({
                "index": index,
                "op": step.op,
                "ok": true,
                "elapsed_ms": elapsed_ms,
                "result": value
            })),
            Err(err) => {
                results.push(json!({
                    "index": index,
                    "op": step.op,
                    "ok": false,
                    "elapsed_ms": elapsed_ms,
                    "error": err.to_string()
                }));
                if req.stop_on_error {
                    stopped_at = Some(index);
                    break;
                }
            }
        }
    }

    let ok = results
        .iter()
        .all(|step| step.get("ok").and_then(Value::as_bool) == Some(true));
    Json(json!({
        "ok": ok,
        "count": results.len(),
        "total": req.steps.len(),
        "stopped_at": stopped_at,
        "steps": results
    }))
}

/// 并行批量执行：一次请求内跑多步，最多 `concurrency` 个并发。
///
/// 与 `/run` 的分工：`/run` 串行、遇错即停，适合有前后依赖的流程；
/// `/batch` 并发，适合跨标签页批量读取/截图/操作——这是大规模处理的主路径。
async fn batch_steps(State(state): State<AppState>, Json(req): Json<BatchRequest>) -> Json<Value> {
    touch(&state).await;
    if req.steps.is_empty() {
        return Json(json!({
            "ok": false,
            "error": "batch: steps 不能为空",
            "error_code": "empty_steps"
        }));
    }
    let total = req.steps.len();
    let concurrency = req.concurrency.clamp(1, 32);
    let started = Instant::now();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut set = tokio::task::JoinSet::new();
    for (index, step) in req.steps.into_iter().enumerate() {
        let state = state.clone();
        let permits = semaphore.clone();
        set.spawn(async move {
            let _guard = permits.acquire_owned().await.ok();
            let step_started = Instant::now();
            let outcome = run_job(
                &state,
                &step.op,
                step.args,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
            (
                index,
                step.op,
                step_started.elapsed().as_millis() as u64,
                outcome,
            )
        });
    }
    let mut slots: Vec<Option<Value>> = vec![None; total];
    let mut ok_count = 0usize;
    let mut panicked = 0usize;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((index, op, elapsed_ms, outcome)) => {
                let entry = match outcome {
                    Ok(value) => {
                        ok_count += 1;
                        json!({ "index": index, "op": op, "ok": true, "elapsed_ms": elapsed_ms, "result": value })
                    }
                    Err(err) => json!({
                        "index": index, "op": op, "ok": false,
                        "elapsed_ms": elapsed_ms, "error": err.to_string()
                    }),
                };
                if let Some(slot) = slots.get_mut(index) {
                    *slot = Some(entry);
                }
            }
            Err(_) => panicked += 1,
        }
    }
    let steps: Vec<Value> = slots.into_iter().flatten().collect();
    Json(json!({
        "ok": ok_count == total && panicked == 0,
        "count": steps.len(),
        "total": total,
        "failed": total - ok_count,
        "panicked": panicked,
        "concurrency": concurrency,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "steps": steps
    }))
}

async fn job_submit(
    State(state): State<AppState>,
    Json(req): Json<JobSubmitRequest>,
) -> Json<Value> {
    touch(&state).await;
    let op = req.op.trim().to_string();
    if !JOB_OPS.contains(&op.as_str()) {
        return Json(json!({
            "ok": false,
            "error": format!("unsupported async op: {op}"),
            "error_code": "unsupported_job_op",
            "supported": JOB_OPS
        }));
    }

    let id = Uuid::new_v4().simple().to_string();
    let created_at = epoch_secs();
    let cancel = Arc::new(AtomicBool::new(false));
    let record = JobRecord {
        id: id.clone(),
        op: op.clone(),
        status: JobStatus::Running,
        created_at,
        started_at: None,
        finished_at: None,
        elapsed_ms: 0,
        result: None,
        error: None,
        session_key: None,
        cancel: cancel.clone(),
    };
    {
        let mut store = state.jobs.lock().await;
        // 只保留最近一段历史，避免长期运行无限增长（淘汰顺序见 `evict_for_insert`）。
        store.evict_for_insert();
        store.jobs.insert(id.clone(), record);
    }

    let task_state = state.clone();
    let task_id = id.clone();
    let task_op = op.clone();
    tokio::spawn(async move {
        let started = Instant::now();
        {
            let mut store = task_state.jobs.lock().await;
            if let Some(job) = store.jobs.get_mut(&task_id) {
                job.started_at = Some(epoch_secs());
            }
        }
        let outcome = run_job(&task_state, &task_op, req.args, cancel.clone()).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let cancelled = cancel.load(Ordering::Relaxed);
        let mut store = task_state.jobs.lock().await;
        if let Some(job) = store.jobs.get_mut(&task_id) {
            job.finished_at = Some(epoch_secs());
            job.elapsed_ms = elapsed_ms;
            // 只有「仍在 Running」才允许 cancel 改写终态：cancel 落在任务刚完成的
            // 窗口里时结果已经产生，判成 Cancelled 会把结果整个丢掉。
            if job.status.is_running() && cancelled {
                job.status = JobStatus::Cancelled;
                job.error = Some(json!({ "code": "job_cancelled", "message": "job cancelled" }));
            } else {
                match outcome {
                    Ok(value) => {
                        job.session_key = value
                            .get("session_key")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        job.result = Some(value);
                        job.status = JobStatus::Succeeded;
                    }
                    Err(err) => {
                        job.status = JobStatus::Failed;
                        job.error =
                            Some(json!({ "code": "job_failed", "message": err.to_string() }));
                    }
                }
            }
        }
    });

    Json(json!({
        "ok": true,
        "job": { "id": id, "op": op, "status": "running", "created_at": created_at },
        "hint": format!("agent-browser-cli job wait {id}")
    }))
}

async fn job_get(State(state): State<AppState>, AxumPath(job_id): AxumPath<String>) -> Json<Value> {
    touch(&state).await;
    let store = state.jobs.lock().await;
    match store.jobs.get(&job_id) {
        Some(job) => Json(json!({ "ok": true, "job": job.to_json() })),
        None => Json(json!({
            "ok": false,
            "error": format!("job {job_id} 不存在或已过期"),
            "error_code": "job_not_found"
        })),
    }
}

async fn job_list(State(state): State<AppState>, Query(query): Query<JobListQuery>) -> Json<Value> {
    touch(&state).await;
    let limit = query.limit.clamp(1, 200);
    let store = state.jobs.lock().await;
    let mut jobs: Vec<&JobRecord> = store.jobs.values().collect();
    jobs.sort_by(|left, right| {
        right
            .created_at
            .partial_cmp(&left.created_at)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let running = jobs.iter().filter(|job| job.status.is_running()).count();
    let items: Vec<Value> = jobs.iter().take(limit).map(|job| job.to_json()).collect();
    Json(json!({
        "ok": true,
        "count": items.len(),
        "running": running,
        "jobs": items
    }))
}

async fn job_cancel(
    State(state): State<AppState>,
    AxumPath(job_id): AxumPath<String>,
) -> Json<Value> {
    touch(&state).await;
    let mut store = state.jobs.lock().await;
    let Some(job) = store.jobs.get_mut(&job_id) else {
        return Json(json!({
            "ok": false,
            "error": format!("job {job_id} 不存在或已过期"),
            "error_code": "job_not_found"
        }));
    };
    // 只有 Running 的任务才真正接受取消：已结束的任务再 cancel 不能把终态
    // 覆盖成 Cancelled（结果已经产生，丢了就再也拿不回来，）。
    // 对终态任务保持幂等：仍返回 ok:true，但明确 cancel_requested:false。
    let running = job.status.is_running();
    if running {
        job.cancel.store(true, Ordering::Relaxed);
    }
    Json(json!({
        "ok": true,
        "job_id": job_id,
        "status": job.status.as_str(),
        "cancel_requested": running,
        "note": if running {
            "取消是协作式的：exec 的 --wait-js 轮询与 wait --js 可及时停止；wait --selector/--text 的观察器、scan/read/snapshot 需等当前步骤完成；已发出的页面脚本无法中断"
        } else {
            "任务已结束，取消未生效（状态与结果保持不变）"
        }
    }))
}

async fn run_job(
    state: &AppState,
    op: &str,
    args: Value,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    match op {
        "exec" => {
            exec_value(
                state,
                serde_json::from_value::<ExecRequest>(args)?,
                Some(cancel),
            )
            .await
        }
        "scan" => {
            let req: ScanRequest = serde_json::from_value(args)?;
            let timeout = Duration::from_secs_f64(req.timeout.unwrap_or(30.0).clamp(5.0, 300.0));
            scan_page(
                state,
                req.tabs_only,
                SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
                req.text_only,
                req.fast,
                req.max_nodes,
                timeout,
            )
            .await
        }
        "snapshot" => snapshot_page(state, serde_json::from_value(args)?).await,
        "read" => read_value(state, serde_json::from_value(args)?).await,
        "inject" => inject_value(state, serde_json::from_value(args)?).await,
        "frames" => frames_value(state, serde_json::from_value(args)?).await,
        "downloads" => downloads_value(state, serde_json::from_value(args)?).await,
        "net" => net_value(state, serde_json::from_value(args)?).await,
        "find" => find_value(state, serde_json::from_value(args)?).await,
        "wait" => {
            let req: WaitRequest = serde_json::from_value(args)?;
            wait_value(state, req, Some(cancel.as_ref())).await
        }
        "action" | "hover" | "select" | "check" | "uncheck" | "scroll" | "focus" => {
            let mut value = args;
            if op != "action" {
                if let Some(object) = value.as_object_mut() {
                    object.insert("op".to_string(), json!(op));
                }
            }
            action_value(state, serde_json::from_value(value)?).await
        }
        "cookies" => cookies_value(state, serde_json::from_value(args)?).await,
        "click" => {
            target_operation_value(state, OperationKind::Click, serde_json::from_value(args)?).await
        }
        "mouse-click" => {
            target_operation_value(
                state,
                OperationKind::MouseClick,
                serde_json::from_value(args)?,
            )
            .await
        }
        "fill" => fill_value(state, serde_json::from_value(args)?).await,
        "send-keys" => send_keys_value(state, serde_json::from_value(args)?).await,
        "upload" => upload_value(state, serde_json::from_value(args)?).await,
        "screenshot" => screenshot_page(state, serde_json::from_value(args)?).await,
        "save-pdf" => save_pdf_page(state, serde_json::from_value(args)?).await,
        other => Err(anyhow!("unsupported async op: {other}")),
    }
}

async fn screenshot(
    State(state): State<AppState>,
    Json(req): Json<ScreenshotRequest>,
) -> Json<Value> {
    touch(&state).await;
    Json(match screenshot_page(&state, req).await {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

async fn save_pdf(State(state): State<AppState>, Json(req): Json<SavePdfRequest>) -> Json<Value> {
    touch(&state).await;
    Json(match save_pdf_page(&state, req).await {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

async fn screenshot_page(state: &AppState, req: ScreenshotRequest) -> Result<Value> {
    let tab_id = select_tab(
        state,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
    )
    .await?;
    let timeout = Duration::from_secs_f64(req.timeout.max(0.1));
    let format = normalize_screenshot_format(&req.format)?;
    let target = match (req.target, req.selector) {
        (Some(_), Some(_)) => {
            return Err(anyhow!("screenshot: --target 不能和 --selector 同时使用"))
        }
        (Some(target), None) | (None, Some(target)) => Some(target),
        (None, None) => None,
    };
    let mut params = json!({ "format": format });
    if format == "jpeg" {
        let quality = req.quality.unwrap_or(80);
        if !(1..=100).contains(&quality) {
            return Err(anyhow!("screenshot: quality 必须在 1..=100"));
        }
        params["quality"] = json!(quality);
    }
    if let Some(target) = target.as_deref() {
        let rect = target_rect(state, &tab_id, target, timeout).await?;
        params["clip"] = json!({
            "x": rect.x,
            "y": rect.y,
            "width": rect.width,
            "height": rect.height,
            "scale": 1.0
        });
    } else if req.full_page {
        let metrics = cdp_call(state, &tab_id, "Page.getLayoutMetrics", json!({}), timeout).await?;
        if !metrics.ok {
            return Err(anyhow!(metrics.error.unwrap_or_else(|| {
                "screenshot: Page.getLayoutMetrics failed".to_string()
            })));
        }
        let content_size = metrics
            .data
            .and_then(|data| data.get("contentSize").cloned())
            .ok_or_else(|| anyhow!("screenshot: missing contentSize"))?;
        params["clip"] = json!({
            "x": content_size.get("x").and_then(Value::as_f64).unwrap_or(0.0),
            "y": content_size.get("y").and_then(Value::as_f64).unwrap_or(0.0),
            "width": content_size.get("width").and_then(Value::as_f64).unwrap_or(0.0),
            "height": content_size.get("height").and_then(Value::as_f64).unwrap_or(0.0),
            "scale": 1.0
        });
        params["captureBeyondViewport"] = json!(true);
    }
    let captured = cdp_call(state, &tab_id, "Page.captureScreenshot", params, timeout).await?;
    if !captured.ok {
        return Err(anyhow!(captured.error.unwrap_or_else(|| {
            "screenshot: Page.captureScreenshot failed".to_string()
        })));
    }
    let data = captured
        .data
        .and_then(|data| data.get("data").and_then(Value::as_str).map(str::to_string))
        .ok_or_else(|| anyhow!("screenshot: CDP response missing data"))?;
    let bytes = decode_base64(&data)?;
    // 解码成功不代表数据可用：空/截断的 base64 会得到 0 字节或半截文件。
    validate_screenshot_bytes(&bytes, format)?;
    let path = req.out.unwrap_or_else(|| default_screenshot_path(format));
    write_bytes(&path, &bytes)?;
    Ok(json!({
        "status": "success",
        "path": path,
        "format": format,
        "bytes": bytes.len(),
        "target": target,
        "full_page": req.full_page
    }))
}

async fn save_pdf_page(state: &AppState, req: SavePdfRequest) -> Result<Value> {
    let tab_id = select_tab(
        state,
        SessionSelector::new(req.switch_tab_id, req.browser, req.profile),
    )
    .await?;
    let timeout = Duration::from_secs_f64(req.timeout.max(0.1));
    let (paper_width, paper_height) = paper_size(&req.paper)?;
    if !(0.1..=2.0).contains(&req.scale) {
        return Err(anyhow!("save-pdf: scale 必须在 0.1..=2.0"));
    }
    let title = current_title(state, &tab_id, timeout)
        .await
        .unwrap_or_else(|_| "page".to_string());
    let result = cdp_call(
        state,
        &tab_id,
        "Page.printToPDF",
        json!({
            "printBackground": req.print_background,
            "landscape": req.landscape,
            "scale": req.scale,
            "paperWidth": paper_width,
            "paperHeight": paper_height,
            "preferCSSPageSize": true
        }),
        timeout,
    )
    .await?;
    if !result.ok {
        return Err(anyhow!(result
            .error
            .unwrap_or_else(|| "save-pdf: Page.printToPDF failed".to_string())));
    }
    let data = result
        .data
        .and_then(|data| data.get("data").and_then(Value::as_str).map(str::to_string))
        .ok_or_else(|| anyhow!("save-pdf: CDP response missing data"))?;
    let bytes = decode_base64(&data)?;
    // 同 screenshot：先校验魔数与长度，再考虑 50MB 上限。
    validate_pdf_bytes(&bytes)?;
    const MAX_PDF_BYTES: usize = 50 * 1024 * 1024;
    if bytes.len() > MAX_PDF_BYTES {
        return Err(anyhow!("save-pdf: PDF exceeds max size 50MB"));
    }
    let path = req.out.unwrap_or_else(|| default_pdf_path(&title));
    write_bytes(&path, &bytes)?;
    Ok(json!({
        "status": "success",
        "path": path,
        "bytes": bytes.len(),
        "paper": req.paper.to_ascii_lowercase(),
        "landscape": req.landscape,
        "scale": req.scale,
        "print_background": req.print_background
    }))
}

async fn target_rect(
    state: &AppState,
    tab_id: &str,
    target: &str,
    timeout: Duration,
) -> Result<RectInfo> {
    let object_id = resolve_target_object(state, tab_id, target, timeout).await?;
    let script = "function() { this.scrollIntoView({ block: 'center', inline: 'center' }); const r = this.getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; }";
    let value = call_function_resolved(state, tab_id, &object_id, script, timeout).await?;
    let rect: RectInfo = serde_json::from_value(value)?;
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return Err(anyhow!("screenshot: target has no layout box"));
    }
    Ok(rect)
}

async fn current_title(state: &AppState, tab_id: &str, timeout: Duration) -> Result<String> {
    let response = execute_raw_js_on_tab(state, "return document.title", tab_id, timeout).await?;
    Ok(response
        .data
        .unwrap_or(Value::Null)
        .as_str()
        .unwrap_or("page")
        .to_string())
}

fn normalize_screenshot_format(format: &str) -> Result<&'static str> {
    match format.to_ascii_lowercase().as_str() {
        "png" => Ok("png"),
        "jpg" | "jpeg" => Ok("jpeg"),
        other => Err(anyhow!(
            "screenshot: unsupported format {other}, expected png or jpeg"
        )),
    }
}

fn paper_size(paper: &str) -> Result<(f64, f64)> {
    match paper.to_ascii_lowercase().as_str() {
        "letter" => Ok((8.5, 11.0)),
        "legal" => Ok((8.5, 14.0)),
        "a4" => Ok((8.27, 11.69)),
        "a3" => Ok((11.69, 16.54)),
        "tabloid" => Ok((11.0, 17.0)),
        other => Err(anyhow!("save-pdf: unsupported paper {other}")),
    }
}

fn default_screenshot_path(format: &str) -> PathBuf {
    // 不要硬编码 /tmp：Windows 上会落到当前盘符根目录（可能是只读或意外位置）。
    std::env::temp_dir()
        .join("agent-browser-cli-screenshots")
        .join(format!(
            "screenshot-{}.{}",
            timestamp_compact(),
            if format == "jpeg" { "jpg" } else { format }
        ))
}

fn default_pdf_path(title: &str) -> PathBuf {
    std::env::temp_dir()
        .join("agent-browser-cli-pdfs")
        .join(format!(
            "{}-{}.pdf",
            sanitize_filename(title),
            timestamp_compact()
        ))
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}

/// 文件名里的时间戳：毫秒精度 + 4 位随机后缀。
///
/// 秒级时间戳在同秒内连拍（脚本批量截图 / 循环存 PDF）会互相覆盖，
/// 而且 `write_bytes` 是直接覆盖写，用户只会看到最后一张。
fn timestamp_compact() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let millis = now.as_millis();
    let salt = Uuid::new_v4().simple().to_string();
    format!("{millis}-{}", &salt[..4])
}

fn sanitize_filename(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        let mapped = if matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            || ch.is_control()
            || ch.is_whitespace()
        {
            '-'
        } else {
            ch
        };
        if mapped == '-' && out.ends_with('-') {
            continue;
        }
        out.push(mapped);
        if out.chars().count() >= 80 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "page".to_string()
    } else {
        trimmed
    }
}

fn decode_base64(input: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0;
    for byte in input.bytes().filter(|b| !b.is_ascii_whitespace()) {
        if byte == b'=' {
            break;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(anyhow!("invalid base64 data")),
        } as u32;
        buf = (buf << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Ok(out)
}

/// 截图数据的最低可信长度。比一张 1×1 的 PNG（约 70 字节）略大：
/// 真实视口截图不可能比它更短，短于这个值只可能是空/截断传输。
const MIN_SCREENSHOT_BYTES: usize = 100;
/// PDF 的最低可信长度。
const MIN_PDF_BYTES: usize = 200;

/// 校验 CDP 回传的画面数据：**能解码 ≠ 可用**。
///
/// `decode_base64` 对空串和截断串都会「成功」返回（0 字节或半截文件），
/// 旧实现照样写盘并返回 `status:success`，退出码 0，调用方拿到坏图却以为成功。
fn validate_screenshot_bytes(bytes: &[u8], format: &str) -> Result<()> {
    let magic_ok = match format {
        "png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        // 其它格式不认魔数，只校验长度（当前 CLI 只产出 png/jpeg）。
        _ => true,
    };
    if bytes.len() < MIN_SCREENSHOT_BYTES || !magic_ok {
        return Err(anyhow!(
            "浏览器返回的画面数据无效（{format}，{} 字节，魔数{}），未写入文件",
            bytes.len(),
            if magic_ok { "正确" } else { "不符" }
        ));
    }
    Ok(())
}

fn validate_pdf_bytes(bytes: &[u8]) -> Result<()> {
    if bytes.len() < MIN_PDF_BYTES || !bytes.starts_with(b"%PDF-") {
        return Err(anyhow!(
            "浏览器返回的画面数据无效（PDF，{} 字节），未写入文件",
            bytes.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn test_state() -> AppState {
        let (shutdown, _shutdown_rx) = mpsc::unbounded_channel();
        AppState {
            driver: Arc::new(RwLock::new(DriverState::default())),
            execs: Arc::new(Mutex::new(crate::protocol::ExecRegistry::default())),
            jobs: Arc::new(Mutex::new(JobStore::default())),
            session_locks: Arc::new(Mutex::new(HashMap::new())),
            page_scale: Arc::new(Mutex::new(HashMap::new())),
            ws_bind_error: Arc::new(Mutex::new(None)),
            ext_reload_sent: Arc::new(Mutex::new(HashSet::new())),
            profile_label_lock: Arc::new(Mutex::new(())),
            net_persist_lock: Arc::new(Mutex::new(())),
            started_at: SystemTime::now(),
            last_activity: Arc::new(Mutex::new(Instant::now())),
            inflight: Arc::new(AtomicUsize::new(0)),
            inflight_since: Arc::new(Mutex::new(None)),
            idle_shutdown_ttl: IDLE_SHUTDOWN_TTL,
            ws_security_warned: Arc::new(AtomicBool::new(false)),
            ws_connections: Arc::new(std::sync::Mutex::new(HashMap::new())),
            ws_conn_seq: Arc::new(AtomicUsize::new(0)),
            shutdown,
            sessions_ready: Arc::new(Notify::new()),
            extension_port: config::DEFAULT_EXTENSION_PORT,
            api_token: "test-token".to_string(),
        }
    }

    fn insert_test_session(
        driver: &mut DriverState,
        browser_id: &str,
        profile_id: &str,
        tab_id: &str,
    ) -> mpsc::UnboundedReceiver<String> {
        let session_key = crate::protocol::make_session_key(browser_id, profile_id, tab_id);
        let (sender, receiver) = mpsc::unbounded_channel();
        driver.sessions.insert(
            session_key.clone(),
            Session {
                session_key: session_key.clone(),
                tab_id: tab_id.to_string(),
                browser_id: browser_id.to_string(),
                profile_id: profile_id.to_string(),
                profile_label: None,
                extension_version: Some("2.1".to_string()),
                file_scheme_access: Some(true),
                info: TabInfo {
                    id: tab_id.to_string(),
                    tab_id: tab_id.to_string(),
                    browser_id: browser_id.to_string(),
                    profile_id: profile_id.to_string(),
                    profile_label: None,
                    session_key,
                    url: "https://example.com".to_string(),
                    title: "test".to_string(),
                    tab_type: "ext_ws".to_string(),
                    connected_at: None,
                },
                sender,
                disconnected_at: None,
            },
        );
        receiver
    }

    #[test]
    fn api_access_requires_token_and_rejects_foreign_origins() {
        let mut headers = HeaderMap::new();
        headers.insert(
            config::API_TOKEN_HEADER,
            HeaderValue::from_static("test-token"),
        );
        assert!(validate_api_access(&headers, "test-token").is_ok());

        headers.insert(ORIGIN, HeaderValue::from_static("https://evil.example"));
        let error = validate_api_access(&headers, "test-token").unwrap_err();
        assert_eq!(error.0, StatusCode::FORBIDDEN);
        assert_eq!(error.1, "origin_not_allowed");

        headers.insert(ORIGIN, HeaderValue::from_static(LOOPBACK_API_ORIGIN));
        assert!(validate_api_access(&headers, "test-token").is_ok());
        headers.remove(config::API_TOKEN_HEADER);
        let error = validate_api_access(&headers, "test-token").unwrap_err();
        assert_eq!(error.0, StatusCode::UNAUTHORIZED);
        assert_eq!(error.1, "invalid_api_token");
    }

    #[test]
    fn websocket_origin_requires_a_valid_chrome_extension_id() {
        let mut headers = HeaderMap::new();
        assert!(!is_allowed_extension_origin(&headers));

        headers.insert(ORIGIN, HeaderValue::from_static("https://evil.example"));
        assert!(!is_allowed_extension_origin(&headers));

        headers.insert(
            ORIGIN,
            HeaderValue::from_static("chrome-extension://abcdefghijklmnopabcdefghijklmnop"),
        );
        assert!(is_allowed_extension_origin(&headers));

        headers.insert(
            ORIGIN,
            HeaderValue::from_static(
                "chrome-extension://abcdefghijklmnopabcdefghijklmnop.example.com",
            ),
        );
        assert!(!is_allowed_extension_origin(&headers));
    }

    #[test]
    fn ws_trust_enforces_identity_only_when_configured() {
        let empty = WsTrust::default();
        assert!(!empty.enforces_identity(), "未配置 = 兼容模式");
        assert_eq!(empty.token_value(), None);

        let blank_token = WsTrust {
            extension_ids: Vec::new(),
            token: Some("   ".to_string()),
        };
        assert!(!blank_token.enforces_identity(), "空白 token 等同于未配置");

        let full = WsTrust {
            extension_ids: vec!["abcdefghijklmnopabcdefghijklmnop".to_string()],
            token: Some(" secret ".to_string()),
        };
        assert!(full.enforces_identity());
        assert_eq!(full.token_value(), Some("secret"));
    }

    /// N3-02 / §4.4：坏 JSON 与**空内容**都必须变成 `Corrupt`，绝不能退化成「未配置」
    /// （= 任意扩展可连）。
    #[test]
    fn parse_ws_trust_accepts_valid_json() {
        match parse_ws_trust(r#"{"extension_ids":["abc"],"token":"t"}"#) {
            WsTrustState::Ok(trust) => {
                assert_eq!(trust.extension_ids, vec!["abc".to_string()]);
                assert_eq!(trust.token_value(), Some("t"));
                assert!(trust.enforces_identity());
            }
            WsTrustState::Corrupt(err) => panic!("合法 JSON 不该判成损坏: {err}"),
        }
        // 空对象是合法的「什么都没配」。
        match parse_ws_trust("{}") {
            WsTrustState::Ok(trust) => assert!(!trust.enforces_identity()),
            WsTrustState::Corrupt(err) => panic!("空对象不该判成损坏: {err}"),
        }
    }

    /// §4.4：存在但为空 ≠ 未配置。清空文件以前会静默回到「任意扩展可连」，
    /// 现在必须 fail-closed，并在文案里告诉用户"删文件才回兼容模式"。
    #[test]
    fn parse_ws_trust_treats_empty_as_corrupt() {
        for raw in ["", "   ", "\n\t\r\n"] {
            match parse_ws_trust(raw) {
                WsTrustState::Corrupt(err) => {
                    assert!(
                        err.contains("删除该文件"),
                        "文案要指出回到兼容模式的做法，实际: {err}"
                    );
                }
                WsTrustState::Ok(trust) => panic!(
                    "空文件必须 fail-closed，不能退化成兼容模式（白名单 {} 条）",
                    trust.extension_ids.len()
                ),
            }
        }
    }

    #[test]
    fn parse_ws_trust_rejects_corrupt_json() {
        // 尾逗号：用户手改配置最常见的错误，以前会让整套白名单静默失效。
        for raw in [
            r#"{"extension_ids": ["abc",], "token": null}"#,
            r#"{"extension_ids": "not-an-array"}"#,
            "not json at all",
        ] {
            match parse_ws_trust(raw) {
                WsTrustState::Corrupt(err) => {
                    assert!(!err.trim().is_empty(), "损坏时必须带得上错误信息: {raw}")
                }
                WsTrustState::Ok(trust) => panic!(
                    "坏 JSON 必须 fail-closed，不能解析成兼容模式: {raw} -> {:?}",
                    trust.extension_ids
                ),
            }
        }
    }

    /// N3-04：`/reload-trust` 的判定核心——只踢掉白名单外的人，兼容模式不踢。
    #[test]
    fn reload_trust_revokes_only_untrusted_extensions() {
        let trusted = WsTrustState::Ok(WsTrust {
            extension_ids: vec!["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()],
            token: None,
        });
        assert!(
            !ws_trust_should_revoke(&trusted, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", None),
            "id 比对与握手一致：忽略大小写与空白"
        );
        assert!(ws_trust_should_revoke(
            &trusted,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            None
        ));

        // 兼容模式（白名单为空、无 token）不能踢人，否则清空白名单会瞬间断掉所有连接。
        // 连接上残留的旧指纹也不能让人被误踢：降级到兼容模式不该断谁。
        let compat = WsTrustState::Ok(WsTrust::default());
        assert!(!ws_trust_should_revoke(
            &compat,
            "cccccccccccccccccccccccccccccccc",
            None
        ));
        assert!(!ws_trust_should_revoke(
            &compat,
            "cccccccccccccccccccccccccccccccc",
            Some(token_fingerprint("old-secret"))
        ));

        // 配置损坏 = fail-closed：全部撤销，且原因排在白名单之前。
        let corrupt = WsTrustState::Corrupt("trailing comma".to_string());
        assert!(ws_trust_should_revoke(
            &corrupt,
            "dddddddddddddddddddddddddddddddd",
            Some(token_fingerprint("whatever"))
        ));

        // `reason` 的实际取值（CLI 与用户都靠它理解为什么被踢）。
        assert_eq!(
            ws_trust_revoke_reason(&corrupt, "dddddddddddddddddddddddddddddddd", None),
            Some("corrupt_config")
        );
        assert_eq!(
            ws_trust_revoke_reason(&trusted, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", None),
            Some("untrusted_id")
        );
        assert_eq!(
            ws_trust_revoke_reason(&trusted, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", None),
            None
        );
    }

    /// §4.1：token-only 配置（白名单空 + token 有值）时，指纹对不上的已建连接必须撤销。
    ///
    /// 这是以前"加了 token 却锁不死"的缺口：两条断言分别覆盖"连接带的是别的 token"
    /// 与"连接根本没带 token"（配置收紧前连上的）。
    #[test]
    fn reload_trust_revokes_token_mismatch_under_token_only_config() {
        let token_only = WsTrustState::Ok(WsTrust {
            extension_ids: Vec::new(),
            token: Some("secret".to_string()),
        });
        let id = "cccccccccccccccccccccccccccccccc";

        assert_eq!(
            ws_trust_revoke_reason(&token_only, id, Some(token_fingerprint("other"))),
            Some("token_mismatch"),
            "白名单放行不代表可信：token-only 配置下指纹不符要撤销"
        );
        assert_eq!(
            ws_trust_revoke_reason(&token_only, id, None),
            Some("token_mismatch"),
            "这条连接是在没配 token 时上来的，配置收紧后应当断开"
        );
    }

    /// §4.1 的反向断言：指纹匹配就不该踢——否则加 token 会把自己也踹下线。
    #[test]
    fn reload_trust_keeps_token_matching_connection() {
        let token_only = WsTrustState::Ok(WsTrust {
            extension_ids: Vec::new(),
            // 带空白：配置侧读过 `token_value()` 时会 trim，两侧必须对齐。
            token: Some(" secret ".to_string()),
        });
        let id = "cccccccccccccccccccccccccccccccc";

        assert_eq!(
            ws_trust_revoke_reason(&token_only, id, Some(token_fingerprint("secret"))),
            None,
            "同一 token 的连接不能被自己的配置踢掉"
        );
        // 相对断言：不写死哈希值，只确认指纹的相等语义（`DefaultHasher` 跨版本不保证）。
        assert_eq!(
            token_fingerprint("secret"),
            token_fingerprint("secret"),
            "同一字符串两次哈希必须相等"
        );
        assert_ne!(
            token_fingerprint("secret"),
            token_fingerprint("secret2"),
            "不同字符串不该撞上"
        );
    }

    /// 注销必须靠 Drop guard：这条测试守住「登记表会泄漏」这个最容易漏的点。
    #[test]
    fn ws_conn_guard_unregisters_on_drop() {
        let state = test_state();
        let revoked = Arc::new(AtomicBool::new(false));
        ws_connections_lock(&state).insert(
            7,
            WsConn {
                extension_id: "x".to_string(),
                token_fingerprint: None,
                revoked,
            },
        );
        assert_eq!(ws_connections_lock(&state).len(), 1);
        {
            let _guard = WsConnGuard {
                state: state.clone(),
                conn_id: 7,
            };
        }
        assert_eq!(
            ws_connections_lock(&state).len(),
            0,
            "Drop 必须注销连接，否则登记表只增不减"
        );
    }

    /// N3-06：challenge 的外层 `ok` 必须等于业务结论，且 `result` 内部结构不动。
    #[test]
    fn challenge_envelope_mirrors_inner_ok() {
        // 超时：内层 ok=false → 外层也必须 false，并带出 error_code。
        let timeout = json_result_with_inner_ok(Ok(json!({
            "ok": false,
            "cleared": false,
            "passed": false,
            "error_code": "challenge_timeout",
            "error": "challenge 在 30s 内未清除",
        })));
        assert_eq!(timeout["ok"], json!(false));
        assert_eq!(timeout["error_code"], json!("challenge_timeout"));
        assert_eq!(timeout["result"]["ok"], json!(false), "内层结构不能改");
        assert_eq!(timeout["result"]["cleared"], json!(false));
        assert_eq!(timeout["result"]["passed"], json!(false));

        // 成功：外层仍是 true。
        let cleared = json_result_with_inner_ok(Ok(json!({
            "ok": true,
            "cleared": true,
        })));
        assert_eq!(cleared["ok"], json!(true));
        assert_eq!(cleared["result"]["cleared"], json!(true));

        // status/detect 分支没有 ok 字段 = 识别本身成功，不能误判成失败。
        let status = json_result_with_inner_ok(Ok(json!({ "action": "status" })));
        assert_eq!(status["ok"], json!(true));
        assert_eq!(status["result"]["action"], json!("status"));

        // 真正的错误路径保持原样。
        let failed = json_result_with_inner_ok(Err(anyhow!("boom")));
        assert_eq!(failed["ok"], json!(false));
        assert!(failed.get("result").is_none());
    }

    /// N3-07：卡死阈值必须同时满足「不误伤正常长命令」与「晚于正常空闲」。
    #[test]
    fn inflight_stall_threshold_has_a_floor() {
        let mut state = test_state();
        state.idle_shutdown_ttl = IDLE_SHUTDOWN_TTL;
        assert_eq!(max_inflight_stall(&state), MAX_INFLIGHT_STALL);
        assert!(MAX_INFLIGHT_STALL >= Duration::from_secs(1800));
        // TTL 配得很大时跟着放大，保证「卡死」不会早于「正常空闲」。
        state.idle_shutdown_ttl = Duration::from_secs(7200);
        assert_eq!(max_inflight_stall(&state), Duration::from_secs(14400));
    }

    #[test]
    fn handshake_query_token_is_parsed_and_decoded() {
        assert_eq!(
            query_param(Some("token=abc"), "token").as_deref(),
            Some("abc")
        );
        assert_eq!(
            query_param(Some("a=1&token=x%20y"), "token").as_deref(),
            Some("x y")
        );
        assert_eq!(query_param(Some("token="), "token").as_deref(), Some(""));
        assert_eq!(query_param(None, "token"), None);
        assert_eq!(query_param(Some("token=abc"), "other"), None);
        // query 语义里裸 `+` 不是空格；空格只能来自 `%20`。
        assert_eq!(
            query_param(Some("token=a+b"), "token").as_deref(),
            Some("a+b")
        );
        assert_eq!(
            query_param(Some("token=a%2Bb"), "token").as_deref(),
            Some("a+b")
        );
    }

    /// 两段式变更观察的「线上契约」：`tests/mock_extension.mjs` 靠
    /// `__abcMonitor__` 与 `status: 'watching'` 这两处字面量识别脚本并回包，
    /// 改动这两个哨兵必须同步改 mock，否则的守卫会红得莫名其妙。
    #[test]
    fn monitor_scripts_keep_their_wire_contract() {
        let start = monitor_start_script("monitor-1");
        assert!(
            start.contains("__abcMonitor__"),
            "mock 靠这个字面量识别脚本"
        );
        assert!(start.contains("status: 'watching'"), "成功后必须回这个状态");
        assert!(start.contains("status: 'unsupported'"), "装不上时的状态");
        assert!(start.contains("\"monitor-1\""), "观察令牌要真的注进脚本");

        let stop = monitor_stop_script("monitor-1");
        assert!(stop.contains("__abcMonitor__"));
        assert!(stop.contains("status: 'ok', watch: state"));
        assert!(stop.contains("status: 'navigated'"));
        assert!(stop.contains("status: 'superseded'"));
        assert!(stop.contains("window[KEY] = null"), "收数后要清掉槽位");
        assert!(stop.contains("\"monitor-1\""), "令牌不匹配时判 superseded");
    }

    /// 的核心不变式：`track_inflight` 必须比 token 校验更外层
    /// （401 也算在途、也续期），且请求结束后计数一定归零。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unauthorized_api_request_is_tracked_and_released() {
        let state = test_state();
        let probe = state.clone();
        if let Some(past) = Instant::now().checked_sub(Duration::from_secs(60)) {
            *state.last_activity.lock().await = past;
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, build_api_router(state))
                .await
                .unwrap();
        });

        let status_line = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut stream = std::net::TcpStream::connect(addr).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .write_all(
                    format!("GET /tabs HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .unwrap();
            let mut buf = String::new();
            let _ = stream.read_to_string(&mut buf);
            buf.lines().next().unwrap_or_default().to_string()
        })
        .await
        .unwrap();

        assert!(
            status_line.contains("401"),
            "缺少 token 必须被拒绝，实际: {status_line}"
        );
        let mut inflight = usize::MAX;
        for _ in 0..100 {
            inflight = probe.inflight.load(Ordering::Relaxed);
            if inflight == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            inflight, 0,
            "请求结束后 inflight 必须归零，否则 daemon 永不自退"
        );
        assert!(
            probe.last_activity.lock().await.elapsed() < Duration::from_secs(30),
            "被 401 拒绝的请求也要续期 last_activity（说明 track_inflight 在最外层）"
        );
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_router_rejects_missing_and_non_extension_origins() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, build_ws_router(test_state()))
                .await
                .unwrap();
        });

        let status_lines = tokio::task::spawn_blocking(move || {
            use std::io::{BufRead, BufReader, Write};

            let handshake = |origin: Option<&str>| {
                let mut stream = std::net::TcpStream::connect(addr).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let origin_header = origin
                    .map(|value| format!("Origin: {value}\r\n"))
                    .unwrap_or_default();
                let request = format!(
                    "GET / HTTP/1.1\r\n\
                     Host: {addr}\r\n\
                     Upgrade: websocket\r\n\
                     Connection: Upgrade\r\n\
                     Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                     Sec-WebSocket-Version: 13\r\n\
                     {origin_header}\r\n"
                );
                stream.write_all(request.as_bytes()).unwrap();
                stream.flush().unwrap();
                let mut status_line = String::new();
                BufReader::new(stream).read_line(&mut status_line).unwrap();
                status_line
            };

            (
                handshake(None),
                handshake(Some("https://evil.example")),
                handshake(Some("chrome-extension://abcdefghijklmnopabcdefghijklmnop")),
            )
        })
        .await
        .unwrap();
        server.abort();
        let _ = server.await;

        assert!(status_lines.0.starts_with("HTTP/1.1 403 Forbidden"));
        assert!(status_lines.1.starts_with("HTTP/1.1 403 Forbidden"));
        assert!(status_lines
            .2
            .starts_with("HTTP/1.1 101 Switching Protocols"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn api_router_enforces_token_and_origin_checks() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, build_api_router(test_state()))
                .await
                .unwrap();
        });

        let responses = tokio::task::spawn_blocking(move || {
            let client = reqwest::blocking::Client::new();
            let url = format!("http://{addr}/health");
            let send = |token: Option<&str>, origin: Option<&str>| {
                let mut request = client.get(&url);
                if let Some(token) = token {
                    request = request.header(config::API_TOKEN_HEADER, token);
                }
                if let Some(origin) = origin {
                    request = request.header("origin", origin);
                }
                let response = request.send().unwrap();
                let status = response.status();
                let body = response.json::<Value>().unwrap();
                (status, body)
            };
            (
                send(None, None),
                send(Some("wrong-token"), None),
                send(Some("test-token"), Some("https://evil.example")),
                send(Some("test-token"), None),
            )
        })
        .await
        .unwrap();
        server.abort();
        let _ = server.await;

        assert_eq!(responses.0 .0, StatusCode::UNAUTHORIZED);
        assert_eq!(responses.0 .1["error_code"], "invalid_api_token");
        assert_eq!(responses.1 .0, StatusCode::UNAUTHORIZED);
        assert_eq!(responses.1 .1["error_code"], "invalid_api_token");
        assert_eq!(responses.2 .0, StatusCode::FORBIDDEN);
        assert_eq!(responses.2 .1["error_code"], "origin_not_allowed");
        assert_eq!(responses.3 .0, StatusCode::OK);
        assert_eq!(responses.3 .1["running"], true);
    }

    /// N3-04：`/reload-trust` 必须落在 API 鉴权层内，且回包报告检查了多少条连接。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reload_trust_requires_token_and_reports_checked() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = test_state();
        // 预置一条已登记连接：用来验证 `checked` 真的在数登记表，而不是写死 0。
        ws_connections_lock(&state).insert(
            1,
            WsConn {
                extension_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                token_fingerprint: None,
                revoked: Arc::new(AtomicBool::new(false)),
            },
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, build_api_router(state))
                .await
                .unwrap();
        });

        let (unauth, authed) = tokio::task::spawn_blocking(move || {
            let client = reqwest::blocking::Client::new();
            let url = format!("http://{addr}/reload-trust");
            let unauth = client.post(&url).send().unwrap().status();
            let response = client
                .post(&url)
                .header(config::API_TOKEN_HEADER, "test-token")
                .send()
                .unwrap();
            let status = response.status();
            let body = response.json::<Value>().unwrap();
            (unauth, (status, body))
        })
        .await
        .unwrap();
        server.abort();
        let _ = server.await;

        assert_eq!(unauth, StatusCode::UNAUTHORIZED, "管理端点不能绕过 token");
        assert_eq!(authed.0, StatusCode::OK);
        // 回包形态固定：ok/checked 必须在；配置是否损坏取决于本机文件，不做断言。
        assert!(authed.1.get("ok").is_some(), "回包: {}", authed.1);
        assert_eq!(authed.1["checked"], json!(1), "回包: {}", authed.1);
    }

    #[tokio::test]
    async fn open_result_uses_executor_scope_when_browser_tab_ids_overlap() {
        let state = test_state();
        {
            let mut driver = state.driver.write().await;
            let _receiver_a = insert_test_session(&mut driver, "browser-a", "profile-a", "7");
            let _receiver_b = insert_test_session(&mut driver, "browser-b", "profile-b", "42");
        }

        let result = normalize_open_result(
            &state,
            json!({
                "js_return": { "id": 42 },
                "tab_id": "7",
                "session_key": "browser-a:profile-a:7"
            }),
        )
        .await;
        assert_eq!(
            result.get("opened_session_key").and_then(Value::as_str),
            Some("browser-a:profile-a:42")
        );
    }

    #[tokio::test]
    async fn open_result_refuses_ambiguous_tab_id_without_executor_scope() {
        let state = test_state();
        {
            let mut driver = state.driver.write().await;
            let _receiver_a = insert_test_session(&mut driver, "browser-a", "profile-a", "42");
            let _receiver_b = insert_test_session(&mut driver, "browser-b", "profile-b", "42");
        }

        let result = normalize_open_result(&state, json!({ "js_return": { "id": 42 } })).await;
        assert!(result.get("opened_session_key").is_some_and(Value::is_null));
    }

    #[tokio::test]
    async fn page_execution_reports_the_session_that_received_the_command() {
        let state = test_state();
        let (mut receiver_a, _receiver_b) = {
            let mut driver = state.driver.write().await;
            let receiver_a = insert_test_session(&mut driver, "browser-a", "profile-a", "7");
            let receiver_b = insert_test_session(&mut driver, "browser-b", "profile-b", "8");
            driver.default_session_key = Some("browser-a:profile-a:7".to_string());
            (receiver_a, receiver_b)
        };

        let exec_state = state.clone();
        let execution = tokio::spawn(async move {
            execute_page_js(
                &exec_state,
                "return 1",
                SessionSelector::new(Some("browser-a:profile-a:7".to_string()), None, None),
                true,
                Duration::from_secs(15),
            )
            .await
        });
        let payload = tokio::time::timeout(Duration::from_secs(1), receiver_a.recv())
            .await
            .expect("command should be dispatched")
            .expect("browser-a receiver should remain connected");
        let exec_id = serde_json::from_str::<Value>(&payload)
            .unwrap()
            .get("id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();

        state.driver.write().await.default_session_key = Some("browser-b:profile-b:8".to_string());
        let (sender, _receiver) = mpsc::unbounded_channel();
        handle_ws_message(
            WsIncoming::Result {
                id: exec_id,
                result: json!(1),
                new_tabs: None,
            },
            &state,
            sender,
            &mut Vec::new(),
        )
        .await;

        let result = execution.await.unwrap().unwrap();
        assert_eq!(
            result.get("session_key").and_then(Value::as_str),
            Some("browser-a:profile-a:7")
        );
        assert_eq!(result.get("tab_id").and_then(Value::as_str), Some("7"));
    }

    #[test]
    fn pending_default_session_does_not_match_another_browser_tab() {
        let mut driver = DriverState {
            preferred_default_session_key: Some("browser-a:profile-a:42".to_string()),
            ..DriverState::default()
        };
        assert!(!should_set_default_session(
            &driver,
            "browser-b:profile-b:42"
        ));
        assert!(should_set_default_session(
            &driver,
            "browser-a:profile-a:42"
        ));
        driver.default_session_key = Some("browser-a:profile-a:7".to_string());
        assert!(should_set_default_session(
            &driver,
            "browser-a:profile-a:42"
        ));
    }

    #[test]
    fn request_target_accepts_compatible_fields() {
        assert_eq!(
            resolve_request_target(Some("./demo.html"), None).unwrap(),
            "./demo.html"
        );
        assert_eq!(
            resolve_request_target(None, Some("https://example.com")).unwrap(),
            "https://example.com"
        );
        assert_eq!(
            resolve_request_target(Some("same"), Some("same")).unwrap(),
            "same"
        );
    }

    #[test]
    fn request_target_rejects_missing_and_conflicting_fields() {
        assert_eq!(
            resolve_request_target(None, None).unwrap_err().code,
            "missing_open_target"
        );
        assert_eq!(
            resolve_request_target(Some("a"), Some("b"))
                .unwrap_err()
                .code,
            "ambiguous_open_target"
        );
    }

    #[test]
    fn extension_versions_compare_numerically() {
        assert!(version_at_least("2.1", "2.1"));
        assert!(version_at_least("2.10", "2.1"));
        assert!(version_at_least("2.1.1", "2.1"));
        assert!(!version_at_least("2.0", "2.1"));
        assert!(!version_at_least("unknown", "2.1"));
    }

    #[test]
    fn extension_freshness_flags_outdated_extension() {
        let bundled = bundled_extension_version();
        assert!(version_at_least(bundled, "2.2"));

        let stale = extension_freshness(&[json!({ "extension_version": "0.0.1" })]);
        assert_eq!(stale.get("stale").and_then(Value::as_bool), Some(true));
        assert!(stale.get("hint").and_then(Value::as_str).is_some());

        let fresh = extension_freshness(&[json!({ "extension_version": bundled })]);
        assert_eq!(fresh.get("stale").and_then(Value::as_bool), Some(false));
        assert_eq!(fresh.get("hint"), Some(&Value::Null));
    }

    /// 在途执行表拆出来后，结果回传、断连、会话清理都必须只靠这张表完成；
    /// 漏清会泄漏 oneshot 发送端，漏取会让命令等到超时。
    #[test]
    fn exec_registry_tracks_and_clears_entries() {
        use crate::protocol::{ExecRegistry, PendingExec};

        let mut registry = ExecRegistry::default();
        let (tx_a, _rx_a) = oneshot::channel();
        let (tx_b, _rx_b) = oneshot::channel();
        let (tx_c, _rx_c) = oneshot::channel();
        registry.register(
            "e1",
            "s1",
            PendingExec {
                delivered_at: None,
                tx: tx_a,
            },
        );
        registry.register(
            "e2",
            "s1",
            PendingExec {
                delivered_at: None,
                tx: tx_b,
            },
        );
        registry.register(
            "e3",
            "s2",
            PendingExec {
                delivered_at: None,
                tx: tx_c,
            },
        );
        assert_eq!(registry.pending.len(), 3);
        assert_eq!(registry.active.len(), 3);

        registry.acked.insert("e1".to_string());
        assert!(registry.take("e1").is_some());
        assert!(registry.acked.is_empty(), "take 必须顺手清掉 acked");
        assert!(!registry.active.contains_key("e1"));
        assert!(registry.take("e1").is_none(), "重复 take 不能 panic");

        let orphaned = registry.take_session("s1");
        assert_eq!(orphaned.len(), 1, "s1 只剩 e2");
        assert_eq!(registry.pending.len(), 1);

        registry.retain_sessions(&["s1".to_string()]);
        assert!(
            registry.pending.is_empty(),
            "s2 不在存活列表里，e3 应被清掉"
        );
        assert!(registry.active.is_empty());
    }

    /// 淘汰必须按「完成时间升序」，而不是 HashMap 的迭代顺序。
    /// 旧实现 `values().take(20)` 会随机删掉刚完成、正等 `job wait` 的新任务。
    #[test]
    fn job_store_evicts_oldest_finished_jobs() {
        fn record(
            id: &str,
            status: JobStatus,
            created_at: f64,
            finished_at: Option<f64>,
        ) -> JobRecord {
            JobRecord {
                id: id.to_string(),
                op: "exec".to_string(),
                status,
                created_at,
                started_at: Some(created_at),
                finished_at,
                elapsed_ms: 0,
                result: None,
                error: None,
                session_key: None,
                cancel: Arc::new(AtomicBool::new(false)),
            }
        }

        let mut store = JobStore::default();
        for index in 0..120u32 {
            let id = format!("job-{index:03}");
            store.jobs.insert(
                id.clone(),
                record(
                    &id,
                    JobStatus::Succeeded,
                    f64::from(index),
                    Some(f64::from(index)),
                ),
            );
        }
        store.evict_for_insert();
        assert_eq!(
            store.jobs.len(),
            100,
            "超过软上限时必须淘汰一批（120 - 20）"
        );
        for index in 0..20u32 {
            assert!(
                !store.jobs.contains_key(&format!("job-{index:03}")),
                "完成时间最旧的 job-{index:03} 必须先被淘汰"
            );
        }
        // 刚完成的最新任务必须留下：调用方可能正在 `job wait` 它。
        assert!(store.jobs.contains_key("job-119"));
    }

    /// 的另一半：全是 Running 时表也必须有硬上限，且淘汰最旧的。
    #[test]
    fn job_store_stays_bounded_when_all_jobs_are_running() {
        let mut store = JobStore::default();
        for index in 0..JOB_STORE_HARD_LIMIT {
            let id = format!("run-{index:04}");
            store.jobs.insert(
                id.clone(),
                JobRecord {
                    id: id.clone(),
                    op: "wait".to_string(),
                    status: JobStatus::Running,
                    created_at: index as f64,
                    started_at: Some(index as f64),
                    finished_at: None,
                    elapsed_ms: 0,
                    result: None,
                    error: None,
                    session_key: None,
                    cancel: Arc::new(AtomicBool::new(false)),
                },
            );
        }
        store.evict_for_insert();
        assert_eq!(
            store.jobs.len(),
            JOB_STORE_HARD_LIMIT - 1,
            "硬上限触发时腾出一个位置给新任务"
        );
        assert!(
            !store.jobs.contains_key("run-0000"),
            "最旧的 running 先淘汰"
        );
        assert!(store.jobs.contains_key("run-0499"), "最新的 running 保留");
    }

    /// 极端嵌套的树（1000 层）不能让递归爆栈：三个递归函数都要在深度上限处停住，
    /// 且结果仍然可序列化（能原样吐给 CLI）。
    #[test]
    fn deep_tree_recursion_is_bounded() {
        // 逐层手工包装：`json!` 每层都会把内层重新序列化一遍，1000 层会把
        // 构造阶段自己递归爆栈（那是宏的问题，不是被测函数的问题）。
        fn nested(depth: usize) -> Value {
            let mut node = json!({ "ref": "@e1", "role": "button" });
            for _ in 0..depth {
                let mut obj = serde_json::Map::new();
                obj.insert("role".to_string(), json!("generic"));
                obj.insert("children".to_string(), Value::Array(vec![node]));
                node = Value::Object(obj);
            }
            node
        }

        // 每次重新构造，不 clone 原始深树：`Value::clone()` 对 1000 层也是递归的，
        // 在 debug 构建下（帧很大）测试自己就会爆栈，测不到被测函数。
        let pruned = prune_tree_keep_refs(vec![nested(1000)]);
        let folded = fold_tree(vec![nested(1000)], 10_000);

        let refs: HashMap<String, ElementRef> = HashMap::new();
        let with_details = attach_details_to_tree(vec![nested(1000)], &refs);

        // 深度上限生效：三条递归路径都必须停住，而不是靠「序列化没报错」证明没崩。
        fn depth_of(value: &Value) -> usize {
            match value.get("children").and_then(Value::as_array) {
                Some(children) => 1 + children.iter().map(depth_of).max().unwrap_or(0),
                None => 1,
            }
        }
        fn node_count(nodes: &[Value]) -> usize {
            nodes
                .iter()
                .map(|node| {
                    1 + node
                        .get("children")
                        .and_then(Value::as_array)
                        .map(|children| node_count(children))
                        .unwrap_or(0)
                })
                .sum()
        }

        let pruned_depth = pruned.iter().map(depth_of).max().unwrap_or(0);
        assert!(
            pruned_depth <= MAX_TREE_DEPTH + 1,
            "prune 必须截断到 {MAX_TREE_DEPTH} 层，实际 {pruned_depth}"
        );
        // 1000 层链上唯一的 ref 在深度上限之外：prune 会整支丢弃（不是 panic），
        // 所以这里断言的是「不超上限」而不是「一定有节点」。
        assert!(
            node_count(&pruned) <= MAX_TREE_DEPTH + 1,
            "prune 截断后节点数也必须受限，实际 {}",
            node_count(&pruned)
        );
        // 对照：ref 在深度上限以内时必须完整保留这条祖先链（10 层 + 叶子 = 11 个节点）。
        let shallow = prune_tree_keep_refs(vec![nested(10)]);
        assert_eq!(node_count(&shallow), 11, "上限内的 ref 不能被 prune 误删");

        // 节点预算：1001 个节点的深链给 50 个预算，必须恰好保留 50 个，
        // 且结果的深度不超过预算（不能出现「预算只租约、子树照旧」）。
        let limited = fold_tree(vec![nested(1000)], 50);
        assert_eq!(node_count(&limited), 50, "fold_tree 必须严格按预算截断");
        let limited_depth = limited.iter().map(depth_of).max().unwrap_or(0);
        assert!(
            limited_depth <= 50,
            "fold_tree 结果深度必须受节点预算约束，实际 {limited_depth}"
        );
        // 预算远大于节点数时，fold 仍受深度上限约束：1000 层链在 depth=256 处
        // 丢掉子树，留下 0..=256 共 257 个节点（不是 1001）。
        assert_eq!(node_count(&folded), MAX_TREE_DEPTH + 1);

        // attach_details 只控制「下钻」深度，不裁剪树本身：断言它不崩且树原样保留。
        assert_eq!(node_count(&with_details), 1001);
        assert!(serde_json::to_string(&with_details).is_ok());
    }

    #[test]
    fn site_match_pattern_strips_query_and_keeps_regular_hosts() {
        assert_eq!(
            site_match_pattern("https://example.com?q=1").as_deref(),
            Some("*://example.com/*")
        );
        assert_eq!(
            site_match_pattern("https://example.com:8443/a?b=1#c").as_deref(),
            Some("*://example.com/*")
        );
        // 以前 `starts_with("chrom")` 会误杀这两个正常域名（B-10）。
        assert_eq!(
            site_match_pattern("https://chromium.org/docs").as_deref(),
            Some("*://chromium.org/*")
        );
        assert_eq!(
            site_match_pattern("https://chrome.google.com/webstore").as_deref(),
            Some("*://chrome.google.com/*")
        );
        // IPv6 + 端口：match pattern 不能带端口，必须保留方括号。
        assert_eq!(
            site_match_pattern("http://[::1]:8080/x").as_deref(),
            Some("*://[::1]/*")
        );
        // 非 http(s) 一律 fail-closed。
        for url in [
            "chrome://newtab/",
            "chrome-extension://abc/options.html",
            "about:blank",
            "data:text/html,x",
            "blob:https://example.com/uuid",
            "file:///tmp/a.html",
        ] {
            assert_eq!(site_match_pattern(url), None, "{url}");
        }
    }

    #[test]
    fn net_hook_name_is_profile_scoped_and_stable() {
        let a = net_hook_name("chrome:profile-a");
        let b = net_hook_name("chrome:profile-b");
        assert_ne!(a, b);
        assert_eq!(a, net_hook_name("chrome:profile-a"));
        assert!(a.starts_with("__net_hook_"));
        assert_eq!(a.len(), "__net_hook_".len() + 16);
        assert!(a["__net_hook_".len()..]
            .chars()
            .all(|c| c.is_ascii_hexdigit()));
        // 固定向量：跨构建/跨 Rust 版本必须不变，否则升级后算不出旧注入名。
        assert_eq!(a, "__net_hook_dfbead77ec260ece");
    }
}
