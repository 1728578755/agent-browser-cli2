use super::*;

#[derive(Debug, Deserialize)]
pub(crate) struct ScanRequest {
    #[serde(default)]
    pub(crate) tabs_only: bool,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    #[serde(default)]
    pub(crate) text_only: bool,
    /// 快速模式：单次注入的紧凑文本抽取，超大页面也不整页序列化。
    #[serde(default)]
    pub(crate) fast: bool,
    /// 节点/元素处理上限（0 表示默认）。
    #[serde(default)]
    pub(crate) max_nodes: Option<usize>,
    /// 页面内容抓取上限（秒）。
    #[serde(default)]
    pub(crate) timeout: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ExecRequest {
    #[serde(default)]
    pub(crate) script: String,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    #[serde(default)]
    pub(crate) no_monitor: bool,
    pub(crate) wait_js: Option<String>,
    #[serde(default = "default_wait_timeout")]
    pub(crate) wait_timeout: f64,
    #[serde(default = "default_wait_interval")]
    pub(crate) wait_interval: f64,
    /// 页面脚本执行上限（秒）。后台长任务需要能放宽，默认 60 秒。
    #[serde(default)]
    pub(crate) timeout: Option<f64>,
    /// 返回值上限（字符）。超出时返回截断预览，避免一条命令占满上下文。
    #[serde(default)]
    pub(crate) max_bytes: Option<usize>,
    /// 执行环境：MAIN（默认，可访问页面变量）或 ISOLATED（不受页面 CSP 影响）。
    #[serde(default)]
    pub(crate) world: Option<String>,
    /// 指定 iframe 的 frameId；配合 frames 命令使用。
    #[serde(default)]
    pub(crate) frame_id: Option<i64>,
    /// 在标签页的所有 frame 里执行。
    #[serde(default)]
    pub(crate) all_frames: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OpenRequest {
    pub(crate) target: Option<String>,
    /// 一次打开多个目标：daemon 内部并发执行，只占一个 HTTP 往返。
    #[serde(default)]
    pub(crate) targets: Vec<String>,
    pub(crate) url: Option<String>,
    pub(crate) cwd: Option<PathBuf>,
    #[serde(default = "default_active")]
    pub(crate) active: bool,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) group_title: Option<String>,
    #[serde(default)]
    pub(crate) window: bool,
    #[serde(default)]
    pub(crate) allow_focus: bool,
    /// 保持受控标签页不被 Chrome 内存节省机制丢弃，后台长任务必需。
    #[serde(default = "default_true")]
    pub(crate) keep_alive: bool,
    #[serde(default = "default_open_readiness_timeout")]
    pub(crate) readiness_timeout: f64,
    /// 打开后是否自动处理挑战页：auto（默认）| wait | off。
    #[serde(default)]
    pub(crate) challenge: Option<String>,
    /// 自动等待挑战的总超时（秒）。
    #[serde(default)]
    pub(crate) challenge_timeout: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CleanupRequest {
    /// 不传就用当前请求的调用方身份。
    #[serde(default)]
    pub(crate) agent: Option<String>,
    /// 清理所有智能体打开过的标签页（默认只清自己）。
    #[serde(default)]
    pub(crate) all_agents: bool,
    /// 只列出将要关闭的标签页。
    #[serde(default)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CloseRequest {
    pub(crate) tab_id: String,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    /// 解析 `--tab @N` 时的 URL / 标题快照。
    ///
    /// CLI 先换算 id 再逐个关闭，两次请求之间标签可能被切换/重排。带上下发时
    /// daemon 在真正关闭前复核一次，不一致就拒绝，避免关错标签。旧 CLI 不传，
    /// 字段为 None 时行为不变（向后兼容）。
    #[serde(default)]
    pub(crate) expect_url: Option<String>,
    #[serde(default)]
    pub(crate) expect_title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SnapshotRequest {
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    #[serde(default)]
    pub(crate) offset: usize,
    #[serde(default = "default_snapshot_limit")]
    pub(crate) limit: usize,
    #[serde(default)]
    pub(crate) details: bool,
    /// 强制走 DOM 快照（不走 CDP），用于 CDP 被其他扩展 iframe 挡住的页面。
    #[serde(default)]
    pub(crate) js: bool,
    /// 只保留可操作元素（按钮/链接/输入框等），显著缩小大树。
    #[serde(default)]
    pub(crate) interactive_only: bool,
    /// 页面侧 `page_snapshot.js` 的 `skipGeneric` 开关（JS 侧默认 true，
    /// 跳过 generic 角色节点）。以前 daemon 从不下发、CLI 也无对应 flag，
    /// 属于「不可达配置」；这里接通 daemon 侧（HTTP 请求体可直接带
    /// `skipGeneric:false` 保留 generic 节点，用于调试无障碍树）。CLI flag
    /// 需改 cmd 侧，不在本轮范围。CDP 路径不受影响（AX 树本就不产 generic 引用）。
    #[serde(default)]
    pub(crate) skip_generic: Option<bool>,
    /// 返回节点上限，超出截断（默认 0 表示不限制）。
    #[serde(default)]
    pub(crate) max_nodes: usize,
    #[serde(default = "default_high_level_timeout")]
    pub(crate) timeout: f64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TargetActionRequest {
    pub(crate) target: String,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    #[serde(default)]
    pub(crate) monitor: bool,
    pub(crate) wait_js: Option<String>,
    #[serde(default = "default_wait_timeout")]
    pub(crate) wait_timeout: f64,
    #[serde(default = "default_wait_interval")]
    pub(crate) wait_interval: f64,
    #[serde(default = "default_high_level_timeout")]
    pub(crate) timeout: f64,
    pub(crate) frame_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FillRequest {
    pub(crate) target: String,
    #[serde(default)]
    pub(crate) value: String,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    #[serde(default)]
    pub(crate) append: bool,
    #[serde(default)]
    pub(crate) clear: bool,
    #[serde(default)]
    pub(crate) has_value: bool,
    #[serde(default)]
    pub(crate) monitor: bool,
    pub(crate) wait_js: Option<String>,
    #[serde(default = "default_wait_timeout")]
    pub(crate) wait_timeout: f64,
    #[serde(default = "default_wait_interval")]
    pub(crate) wait_interval: f64,
    #[serde(default = "default_high_level_timeout")]
    pub(crate) timeout: f64,
    pub(crate) frame_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SendKeysRequest {
    #[serde(default)]
    pub(crate) frame_id: Option<i64>,
    pub(crate) keys: String,
    pub(crate) target: Option<String>,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    #[serde(default)]
    pub(crate) monitor: bool,
    pub(crate) wait_js: Option<String>,
    #[serde(default = "default_wait_timeout")]
    pub(crate) wait_timeout: f64,
    #[serde(default = "default_wait_interval")]
    pub(crate) wait_interval: f64,
    #[serde(default = "default_high_level_timeout")]
    pub(crate) timeout: f64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ScreenshotRequest {
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) target: Option<String>,
    pub(crate) selector: Option<String>,
    pub(crate) out: Option<PathBuf>,
    #[serde(default = "default_screenshot_format")]
    pub(crate) format: String,
    pub(crate) quality: Option<u8>,
    #[serde(default)]
    pub(crate) full_page: bool,
    /// 整页拼图的设备像素总高上限（CDP 不可用时的回落通道使用）。
    #[serde(default)]
    pub(crate) max_height_px: Option<u32>,
    #[serde(default = "default_high_level_timeout")]
    pub(crate) timeout: f64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SavePdfRequest {
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) out: Option<PathBuf>,
    #[serde(default = "default_pdf_paper")]
    pub(crate) paper: String,
    #[serde(default)]
    pub(crate) landscape: bool,
    #[serde(default = "default_pdf_scale")]
    pub(crate) scale: f64,
    #[serde(default = "default_true")]
    pub(crate) print_background: bool,
    /// 图片型 PDF 回落时整页拼图的高度上限（设备像素）。
    #[serde(default)]
    pub(crate) max_height_px: Option<u32>,
    #[serde(default = "default_high_level_timeout")]
    pub(crate) timeout: f64,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct TabsQuery {
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    /// 输出完整 URL（默认截断到 80 字符以节省 token）。
    #[serde(default)]
    pub(crate) full: bool,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct CookiesRequest {
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    /// 不传则用当前页面 URL。
    pub(crate) url: Option<String>,
    /// list（默认）/ clear。clear 用于反复测试挑战页与登录态切换。
    pub(crate) action: Option<String>,
    /// clear 时限定域（含子域），不传则按 url（默认当前页面）清。
    pub(crate) domain: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UploadRequest {
    pub(crate) target: String,
    #[serde(default)]
    pub(crate) files: Vec<String>,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    #[serde(default = "default_high_level_timeout")]
    pub(crate) timeout: f64,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct LookupTabQuery {
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct TabtreeQuery {
    pub(crate) tab: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    #[serde(default)]
    pub(crate) full: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionTreeItem {
    pub(crate) browser_id: String,
    pub(crate) profile_id: String,
    pub(crate) profile_label: Option<String>,
    pub(crate) tab_id: String,
    pub(crate) session_key: String,
    pub(crate) title: String,
    pub(crate) url: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProfileLabelRequest {
    pub(crate) label: Option<String>,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TabRequest {
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NetworkListRequest {
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) filter: Option<String>,
    #[serde(default = "default_debug_list_limit")]
    pub(crate) limit: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NetworkDetailRequest {
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) request_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ConsoleListRequest {
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) level: Option<String>,
    #[serde(default = "default_debug_list_limit")]
    pub(crate) limit: usize,
}

pub(crate) fn default_active() -> bool {
    true
}

pub(crate) fn default_wait_timeout() -> f64 {
    3.0
}

pub(crate) fn default_open_readiness_timeout() -> f64 {
    10.0
}

pub(crate) fn default_wait_interval() -> f64 {
    0.1
}

pub(crate) fn default_snapshot_limit() -> usize {
    200
}

pub(crate) fn default_high_level_timeout() -> f64 {
    30.0
}

pub(crate) fn default_screenshot_format() -> String {
    "png".to_string()
}

pub(crate) fn default_pdf_paper() -> String {
    "a4".to_string()
}

pub(crate) fn default_pdf_scale() -> f64 {
    1.0
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_debug_list_limit() -> usize {
    100
}

pub(crate) fn default_job_list_limit() -> usize {
    20
}

#[derive(Debug, Deserialize)]
pub(crate) struct JobSubmitRequest {
    pub(crate) op: String,
    #[serde(default)]
    pub(crate) args: Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct JobListQuery {
    #[serde(default = "default_job_list_limit")]
    pub(crate) limit: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RunStep {
    pub(crate) op: String,
    #[serde(default)]
    pub(crate) args: Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RunRequest {
    pub(crate) steps: Vec<RunStep>,
    /// 某一步失败后是否终止后续步骤，默认终止。
    #[serde(default = "default_true")]
    pub(crate) stop_on_error: bool,
}

pub(crate) fn default_read_max_chars() -> usize {
    20000
}

pub(crate) fn default_find_limit() -> usize {
    50
}

pub(crate) fn default_batch_concurrency() -> usize {
    4
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ReadRequest {
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) selector: Option<String>,
    #[serde(default = "default_read_max_chars")]
    pub(crate) max_chars: usize,
    #[serde(default)]
    pub(crate) links: bool,
    #[serde(default = "default_true")]
    pub(crate) forms: bool,
    /// 连隐藏内容一起返回（textContent 语义，不做可见性过滤）。
    #[serde(default)]
    pub(crate) include_hidden: bool,
    /// visible / all，等价于 --include-hidden 的显式写法。
    pub(crate) mode: Option<String>,
    /// 页面内文本抽取时间预算（毫秒）。
    pub(crate) budget_ms: Option<u64>,
    /// 单次读取最多处理多少个节点。
    pub(crate) max_nodes: Option<usize>,
    /// 在指定 iframe 内读取。
    pub(crate) frame_id: Option<i64>,
    /// DOM 节点数阈值（`page_read.js` 的 `innerTextLimit`，页面侧默认 30000）。
    /// 节点数不超过该值时用 `innerText` 精确抽取；超过则退化为近似可见文本
    /// （`textMode:"visible-approx"`）。**不是字符数上限**——控字符量用 `max_chars`。
    #[serde(default)]
    pub(crate) inner_text_limit: Option<usize>,
    pub(crate) timeout: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct FindRequest {
    pub(crate) selector: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) xpath: Option<String>,
    #[serde(default = "default_find_limit")]
    pub(crate) limit: usize,
    #[serde(default = "default_true")]
    pub(crate) visible: bool,
    #[serde(default)]
    pub(crate) exact: bool,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) frame_id: Option<i64>,
    pub(crate) timeout: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ActionRequest {
    pub(crate) op: String,
    pub(crate) target: Option<Value>,
    #[serde(default)]
    pub(crate) params: Value,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) frame_id: Option<i64>,
    pub(crate) timeout: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct WaitRequest {
    pub(crate) js: Option<String>,
    pub(crate) selector: Option<String>,
    pub(crate) text: Option<String>,
    #[serde(default = "default_wait_timeout")]
    pub(crate) timeout: f64,
    /// 轮询间隔（秒），与 exec 的 --wait-interval 一致。
    #[serde(default = "default_wait_interval")]
    pub(crate) interval: f64,
    #[serde(default)]
    pub(crate) visible: bool,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) frame_id: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct InjectRequest {
    /// run | persist | remove | list
    pub(crate) action: Option<String>,
    pub(crate) code: Option<String>,
    pub(crate) name: Option<String>,
    /// MAIN（默认）或 ISOLATED
    pub(crate) world: Option<String>,
    pub(crate) matches: Option<Vec<String>>,
    pub(crate) frame_id: Option<i64>,
    #[serde(default)]
    pub(crate) all_frames: bool,
    pub(crate) switch_tab_id: Option<String>,
    pub(crate) browser: Option<String>,
    pub(crate) profile: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BatchRequest {
    pub(crate) steps: Vec<RunStep>,
    /// 并发度：同时在多个标签页上执行的上限。
    #[serde(default = "default_batch_concurrency")]
    pub(crate) concurrency: usize,
}
