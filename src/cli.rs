use crate::{config, server};
use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use fs2::FileExt;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant};

const HOST: &str = "127.0.0.1";
const PORT: u16 = 18767;

/// 默认 compact 输出（省 token），`--pretty` 时才缩进。
static PRETTY_OUTPUT: AtomicBool = AtomicBool::new(false);
/// 记录本次调用的失败退出码：`ok:false` → 1，job 等待超时 → 3。
static FAILURE_EXIT_CODE: AtomicI32 = AtomicI32::new(0);
const EXIT_FAILURE: i32 = 1;
const EXIT_JOB_TIMEOUT: i32 = 3;

#[derive(Debug, Parser)]
#[command(
    name = "agent-browser-cli",
    version,
    about = "Agent 浏览器感知与控制 CLI：后台操作、异步任务、JS 注入"
)]
pub struct Cli {
    /// 立即返回 job_id，页面操作在 daemon 内后台继续执行，不阻塞当前会话。
    #[arg(long = "async", global = true)]
    pub r#async: bool,
    /// 输出带缩进的 JSON（默认 compact，节省 token）。
    #[arg(long = "pretty", global = true)]
    pub pretty: bool,
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// 列出全部标签页（含 browser_id/profile_id/tab_id），确认后续操作目标
    Tabs(TabsArgs),
    /// 按 browser → profile → 标签页树形展示归属关系
    Tabtree(TabtreeArgs),
    /// 按 tab/browser/profile 反查归属：当前打的是哪个浏览器、哪个 Profile
    Lookup(LookupCommand),
    /// 给 Chrome Profile 起可读别名（当前 daemon 内唯一），之后可用 --profile <label>
    #[command(name = "profile-label")]
    ProfileLabel(ProfileLabelCommand),
    /// 内容感知：取正文、列表、页面文本；只看内容比 snapshot 更省 token
    Scan(ScanArgs),
    /// 执行任意 JS 并回传结果；封装命令覆盖不到时的逃生口
    Exec(ExecArgs),
    /// 读取当前页面 Cookie（含 httpOnly，仅本地返回不落盘）
    Cookies(CookiesArgs),
    /// 给文件输入框设置本地文件，一次可传多个
    Upload(UploadArgs),
    /// 操作定位：生成带 @e 引用的可点击树，供 click/fill 使用
    Snapshot(SnapshotArgs),
    /// 点击元素（CSS 选择器或 snapshot 给的 @e 引用）
    Click(TargetCommandArgs),
    /// 填写输入框，默认覆盖；可 --append 追加、--clear 清空
    Fill(FillCommandArgs),
    /// 向焦点元素或 --target 指定元素发送按键（Enter、快捷键等）
    #[command(name = "send-keys")]
    SendKeys(SendKeysCommandArgs),
    /// 按坐标或元素发真实鼠标事件；普通 click 不生效时用它
    #[command(name = "mouse-click")]
    MouseClick(TargetCommandArgs),
    /// 截图当前页面，支持整页、指定元素、png/jpeg/webp
    Screenshot(ScreenshotArgs),
    /// 把当前页面另存为 PDF（纸张、横竖、缩放可调）
    #[command(name = "save-pdf")]
    SavePdf(SavePdfArgs),
    /// 抓取页面网络请求：start 开始、list/detail 查看、clear/stop 结束
    Network(NetworkCommand),
    /// 抓取页面 console 输出：start 开始、list 查看、clear/stop 结束
    Console(ConsoleCommand),
    /// 管理 --async 提交的后台任务：list / get / wait / cancel
    Job(JobCommand),
    /// 按步骤文件串行执行多步操作，遇错即停（--continue-on-error 可继续）
    Run(RunArgs),
    /// 按步骤文件并发执行，跨标签批量处理（必须显式给 switch_tab_id）
    Batch(BatchArgs),
    /// 结构化读页面：标题、正文、表单、计数；要读内容优先用它
    Read(ReadArgs),
    /// 按 CSS / 可见文本 / XPath 找元素，拿 selector 给后续动作
    Find(FindArgs),
    /// 注入 JS；--persist <name> 命名后每次导航自动重放
    Inject(InjectArgs),
    /// 列出页面内 iframe，取 frame_id 供 --frame 使用
    Frames(FramesArgs),
    /// 列出已连接的浏览器实例，多浏览器/多 Profile 时用来选目标
    Browsers,
    /// 对所有或指定 browser/profile 并发执行同一操作
    Broadcast(BroadcastArgs),
    /// 查看下载列表，可按状态或 ID 过滤
    Downloads(DownloadsArgs),
    /// 发起下载并保存到本地（可指定文件名、子目录）
    Download(DownloadArgs),
    /// 等待下载完成；省略 --id 则等最近一条
    #[command(name = "download-wait")]
    DownloadWait(DownloadWaitArgs),
    /// 网络拦截：页面内请求、hook 记录、mock 规则、拦截日志
    Net(NetCommand),
    /// 悬停元素，触发 hover 才出现的菜单或提示
    Hover(ActionArgs),
    /// 选择下拉框选项（按 --value 或 --label）
    Select(ActionArgs),
    /// 勾选或取消勾选 checkbox / radio
    Check(ActionArgs),
    /// 滚动页面：--to top/bottom、--dy 像素或滚到指定元素
    Scroll(ActionArgs),
    /// 等待元素、文本或 JS 条件成立（默认 10s）
    Wait(WaitArgs),
    /// 挑战页 / 验证码：识别、等待自动放行、转人工（不自动破解）
    Challenge(ChallengeArgs),
    /// 浏览器后退
    Back(TabOnlyArgs),
    /// 重新加载当前标签页
    Reload(TabOnlyArgs),
    /// 打开 URL 或本地文件；多个目标并发打开，--background 不打扰用户
    Open(OpenArgs),
    /// 关闭指定标签页（--tab 必填）
    Close(CloseArgs),
    /// 健康状态：daemon、扩展连接、可用标签页；退出码即结论
    Status,
    /// 查看 daemon 日志尾部
    Logs(LogsArgs),
    /// 环境自检：安装位置、端口、扩展连接逐项诊断
    Doctor,
    /// 把随包的 SKILL.md 安装到 ~/.agents/skills，让 Agent 学会用法
    #[command(name = "install-skill")]
    InstallSkill(InstallSkillArgs),
    /// 修改扩展 WebSocket 端口并重启 daemon
    #[command(name = "set-extension-port")]
    SetExtensionPort(SetExtensionPortArgs),
    /// 管理 WebSocket 信任配置：只允许指定扩展 ID 连接、或要求握手 token
    #[command(name = "trust-extension")]
    TrustExtension(TrustExtensionArgs),
    /// 停止后台 daemon
    Stop,
    /// 重启 daemon（改配置、换端口后使用）
    Restart,
    /// 前台运行 daemon，日志打到 stdout（调试用；平时自动拉起）
    Daemon,
}

#[derive(Debug, Args)]
struct TabsArgs {
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    /// 输出完整 URL（默认截断到 80 字符）
    #[arg(long)]
    full: bool,
}

#[derive(Debug, Args)]
struct TabtreeArgs {
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    /// 输出完整 URL 和 session_key；默认使用 compact 输出以减少 token 消耗。
    #[arg(long)]
    full: bool,
}

#[derive(Debug, Args)]
struct ScanArgs {
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    tabs_only: bool,
    #[arg(long)]
    text_only: bool,
    #[arg(long, default_value_t = 60.0)]
    timeout: f64,
    /// 快速模式：一次注入的紧凑文本抽取，超大页面不做整页序列化
    #[arg(long)]
    fast: bool,
    /// 处理节点/元素上限（默认 8000，fast 模式 80000；与服务端默认值一致）
    #[arg(long = "max-nodes")]
    max_nodes: Option<usize>,
}

#[derive(Debug, Args)]
struct ExecArgs {
    #[arg(default_value = "")]
    script: String,
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    monitor: bool,
    #[arg(long)]
    wait_js: Option<String>,
    #[arg(long, default_value_t = 3.0)]
    wait_timeout: f64,
    #[arg(long, default_value_t = 0.1)]
    wait_interval: f64,
    #[arg(long, default_value_t = 60.0)]
    timeout: f64,
    /// 返回值上限（字符）。超出时返回截断预览，避免一条命令占满上下文
    #[arg(long = "max-bytes")]
    max_bytes: Option<usize>,
    /// 执行环境：main（默认）或 isolated（不受页面 CSP 影响，但看不到页面变量）
    #[arg(long, default_value = "main")]
    world: String,
    /// 在指定 iframe 里执行（先用 frames 查 frame_id）
    #[arg(long)]
    frame: Option<i64>,
    /// 在标签页的所有 frame 里执行
    #[arg(long = "all-frames")]
    all_frames: bool,
}

#[derive(Debug, Args)]
struct CookiesArgs {
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    /// 读取指定 URL 的 Cookie（默认当前页面）
    #[arg(long)]
    url: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct UploadArgs {
    /// 文件输入框：CSS selector 或 @e 引用
    target: String,
    /// 一个或多个本地文件路径
    #[arg(required = true, num_args = 1..)]
    files: Vec<String>,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct SnapshotArgs {
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value_t = 0)]
    offset: usize,
    #[arg(long, default_value_t = 200)]
    limit: usize,
    #[arg(long)]
    details: bool,
    /// 强制走 DOM 快照（不依赖 CDP），CDP 被其他扩展 iframe 挡住时使用
    #[arg(long)]
    js: bool,
    /// 只保留可操作元素（按钮/链接/输入框等），显著缩小大树
    #[arg(long = "interactive-only")]
    interactive_only: bool,
    /// 返回节点上限，超出截断（默认不限制）
    #[arg(long = "max-nodes", default_value_t = 0)]
    max_nodes: usize,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct TargetCommandArgs {
    target: String,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    monitor: bool,
    #[arg(long)]
    wait_js: Option<String>,
    #[arg(long, default_value_t = 3.0)]
    wait_timeout: f64,
    #[arg(long, default_value_t = 0.1)]
    wait_interval: f64,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
    /// 在指定 iframe 内执行（先用 frames 查 frame_id）
    #[arg(long)]
    frame: Option<i64>,
}

#[derive(Debug, Args)]
struct FillCommandArgs {
    target: String,
    value: Option<String>,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    append: bool,
    #[arg(long)]
    clear: bool,
    #[arg(long)]
    monitor: bool,
    #[arg(long)]
    wait_js: Option<String>,
    #[arg(long, default_value_t = 3.0)]
    wait_timeout: f64,
    #[arg(long, default_value_t = 0.1)]
    wait_interval: f64,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
    /// 在指定 iframe 内填写
    #[arg(long)]
    frame: Option<i64>,
}

#[derive(Debug, Args)]
struct SendKeysCommandArgs {
    keys: String,
    #[arg(long)]
    target: Option<String>,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    monitor: bool,
    #[arg(long)]
    wait_js: Option<String>,
    #[arg(long, default_value_t = 3.0)]
    wait_timeout: f64,
    #[arg(long, default_value_t = 0.1)]
    wait_interval: f64,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
    /// 在指定 iframe 内发送按键
    #[arg(long)]
    frame: Option<i64>,
}

#[derive(Debug, Args)]
struct ScreenshotArgs {
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    target: Option<String>,
    #[arg(long)]
    selector: Option<String>,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long, default_value = "png")]
    format: String,
    #[arg(long)]
    quality: Option<u8>,
    #[arg(long)]
    full_page: bool,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct SavePdfArgs {
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long, default_value = "a4")]
    paper: String,
    #[arg(long)]
    landscape: bool,
    #[arg(long, default_value_t = 1.0)]
    scale: f64,
    #[arg(long = "no-print-background")]
    no_print_background: bool,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Subcommand)]
enum NetworkAction {
    Start(TabOnlyArgs),
    List(NetworkListArgs),
    Detail(NetworkDetailArgs),
    Clear(TabOnlyArgs),
    Stop(TabOnlyArgs),
}

#[derive(Debug, Args)]
struct NetworkCommand {
    #[command(subcommand)]
    action: NetworkAction,
}

#[derive(Debug, Subcommand)]
enum ConsoleAction {
    Start(TabOnlyArgs),
    List(ConsoleListArgs),
    Clear(TabOnlyArgs),
    Stop(TabOnlyArgs),
}

#[derive(Debug, Args)]
struct ConsoleCommand {
    #[command(subcommand)]
    action: ConsoleAction,
}

#[derive(Debug, Args)]
struct JobCommand {
    #[command(subcommand)]
    action: JobAction,
}

#[derive(Debug, Subcommand)]
enum JobAction {
    /// 列出最近提交的后台任务
    List(JobListArgs),
    /// 查询单个后台任务的状态与结果
    Get(JobGetArgs),
    /// 阻塞等待任务结束（带超时，超时后仍可再次查询）
    Wait(JobWaitArgs),
    /// 请求取消任务（协作式：等待类任务立即停止）
    Cancel(JobCancelArgs),
}

#[derive(Debug, Args)]
struct JobListArgs {
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

#[derive(Debug, Args)]
struct JobGetArgs {
    id: String,
}

#[derive(Debug, Args)]
struct JobWaitArgs {
    id: String,
    #[arg(long, default_value_t = 300.0)]
    timeout: f64,
    #[arg(long, default_value_t = 0.3)]
    interval: f64,
}

#[derive(Debug, Args)]
struct JobCancelArgs {
    id: String,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// 步骤文件：{"steps":[{"op":"exec","args":{...}}, ...]}，也接受顶层数组
    file: PathBuf,
    /// 某一步失败后继续执行后续步骤（默认遇错即停）
    #[arg(long)]
    continue_on_error: bool,
}

#[derive(Debug, Args)]
struct TabOnlyArgs {
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct NetworkListArgs {
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    filter: Option<String>,
    #[arg(long, default_value_t = 100)]
    limit: usize,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct NetworkDetailArgs {
    request_id: String,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct ConsoleListArgs {
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    level: Option<String>,
    #[arg(long, default_value_t = 100)]
    limit: usize,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct BatchArgs {
    /// 步骤文件：{"steps":[{"op":"read","args":{...}}, ...]}，也接受顶层数组
    file: PathBuf,
    /// 并发度：同时在多个标签页上执行的上限（1-32）
    #[arg(long, default_value_t = 4)]
    concurrency: usize,
}

#[derive(Debug, Args)]
struct ReadArgs {
    /// 只读取该 CSS 选择器范围内的内容
    #[arg(long)]
    selector: Option<String>,
    /// 正文字符上限（默认 20000）
    #[arg(long = "max-chars", default_value_t = 20000)]
    max_chars: usize,
    /// 附带链接列表
    #[arg(long)]
    links: bool,
    /// 不返回表单结构
    #[arg(long = "no-forms")]
    no_forms: bool,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
    /// 连隐藏内容一起返回（textContent 语义）
    #[arg(long = "include-hidden")]
    include_hidden: bool,
    /// 页面内文本抽取时间预算（毫秒，默认 2000）
    #[arg(long = "budget-ms")]
    budget_ms: Option<u64>,
    /// 最多处理多少个节点（默认 60000）
    #[arg(long = "max-nodes")]
    max_nodes: Option<usize>,
    /// 在指定 iframe 内读取（先用 frames 查 frame_id）
    #[arg(long)]
    frame: Option<i64>,
}

#[derive(Debug, Args)]
struct FindArgs {
    /// CSS 选择器
    selector: Option<String>,
    /// 可见文本（包含匹配）
    #[arg(long)]
    text: Option<String>,
    /// XPath
    #[arg(long)]
    xpath: Option<String>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    /// 文本精确匹配（默认包含匹配）
    #[arg(long)]
    exact: bool,
    /// 包含不可见元素
    #[arg(long = "include-hidden")]
    include_hidden: bool,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value_t = 20.0)]
    timeout: f64,
    /// 在指定 iframe 内查找
    #[arg(long)]
    frame: Option<i64>,
}

#[derive(Debug, Args)]
struct ActionArgs {
    /// CSS 选择器，或 text=可见文本
    target: Option<String>,
    /// 命中多个元素时按索引取（从 0 开始）
    #[arg(long)]
    index: Option<i64>,
    /// select 的选项值或文本
    #[arg(long)]
    value: Option<String>,
    #[arg(long)]
    label: Option<String>,
    /// scroll 的纵向位移
    #[arg(long)]
    dy: Option<f64>,
    /// scroll 到 top / bottom
    #[arg(long)]
    to: Option<String>,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value_t = 15.0)]
    timeout: f64,
    /// 在指定 iframe 内执行
    #[arg(long)]
    frame: Option<i64>,
}

#[derive(Debug, Args)]
struct InjectArgs {
    /// 脚本内容；也可用 --file 读文件（--list / --remove 时省略）
    script: Option<String>,
    #[arg(long)]
    file: Option<PathBuf>,
    /// 命名持久注入：每次页面导航后自动重放
    #[arg(long, conflicts_with_all = ["remove", "list"])]
    persist: Option<String>,
    /// 移除已持久化的注入
    #[arg(long, conflicts_with = "list")]
    remove: Option<String>,
    /// 列出已持久化的注入
    #[arg(long)]
    list: bool,
    /// 执行环境 main / isolated
    #[arg(long, default_value = "main")]
    world: String,
    /// 只在匹配的 URL 上重放（可多次），默认 <all_urls>
    #[arg(long = "match")]
    matches: Vec<String>,
    /// 注入到指定 iframe
    #[arg(long)]
    frame: Option<i64>,
    /// 注入到所有 frame
    #[arg(long = "all-frames")]
    all_frames: bool,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct FramesArgs {
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value_t = 20.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct BroadcastArgs {
    /// 要广播的操作：exec / read / find / scan / net / downloads / click ...
    op: String,
    /// exec 的脚本（也可用 --file）
    #[arg(long)]
    script: Option<String>,
    #[arg(long)]
    file: Option<PathBuf>,
    /// 其余参数的 JSON 对象，例如 '{"max_chars":2000}'
    #[arg(long = "args")]
    args_json: Option<String>,
    /// 只在这些 browser 上执行（可重复，支持 id 前缀）
    #[arg(long = "browser")]
    browsers: Vec<String>,
    /// 只在这些 profile 上执行（可重复，id 或 label）
    #[arg(long = "profile")]
    profiles: Vec<String>,
    #[arg(long)]
    concurrency: Option<usize>,
    #[arg(long, default_value_t = 180.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct DownloadsArgs {
    #[arg(long)]
    limit: Option<usize>,
    /// in_progress / complete / interrupted
    #[arg(long)]
    state: Option<String>,
    #[arg(long)]
    id: Option<i64>,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value_t = 20.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct DownloadArgs {
    url: String,
    #[arg(long)]
    filename: Option<String>,
    /// 下载到 Chrome 默认下载目录下的子目录
    #[arg(long)]
    dir: Option<String>,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct DownloadWaitArgs {
    /// 等待指定下载；省略则等待最近一条
    #[arg(long)]
    id: Option<i64>,
    /// 等待超时（秒）
    #[arg(long = "timeout", default_value_t = 120.0)]
    wait_timeout: f64,
    #[arg(long, default_value_t = 0.5)]
    interval: f64,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
}

#[derive(Debug, Args)]
struct NetCommand {
    #[command(subcommand)]
    action: NetAction,
}

#[derive(Debug, Subcommand)]
enum NetAction {
    /// 在页面上下文发请求（带 cookie / 同源策略 / 页面 CSP 视角）
    Fetch(NetFetchArgs),
    /// 包装 fetch / XHR，开始记录（可同时 mock）
    Hook(NetHookArgs),
    /// 读取拦截记录
    Log(NetLogArgs),
    /// 添加 mock 规则（命中后返回伪造响应，也可 abort）
    Mock(NetMockArgs),
    /// 列出 mock 规则
    Routes(NetTargetArgs),
    /// 删除 mock 规则
    Unroute(NetUnrouteArgs),
    /// 还原 fetch / XHR 并清空状态
    Reset(NetTargetArgs),
}

#[derive(Debug, Args)]
struct NetTargetArgs {
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    /// 在指定 iframe 内执行
    #[arg(long)]
    frame: Option<i64>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct NetFetchArgs {
    url: String,
    #[arg(long, default_value = "GET")]
    method: String,
    /// 请求头，格式 'Name: value'（可重复）
    #[arg(long = "header", short = 'H')]
    headers: Vec<String>,
    /// 请求体：以 { 或 [ 开头按 JSON，否则按纯文本
    #[arg(long)]
    body: Option<String>,
    /// 返回形式：text（默认）或 base64
    #[arg(long = "response-type", default_value = "text")]
    response_type: String,
    /// 响应体上限（字符，默认 200000）
    #[arg(long = "max-chars", default_value_t = 200000)]
    max_chars: usize,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    frame: Option<i64>,
    #[arg(long, default_value_t = 60.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct NetHookArgs {
    /// 记录响应体预览（会增加开销）
    #[arg(long = "capture-bodies")]
    capture_bodies: bool,
    /// 持久化：页面导航后自动重装 hook（可配合 --match 限定 URL）
    #[arg(long)]
    persist: bool,
    /// 持久化的 URL 匹配模式（可重复），默认 <all_urls>
    #[arg(long = "match")]
    matches: Vec<String>,
    #[arg(long = "max-log", default_value_t = 200)]
    max_log: usize,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    frame: Option<i64>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct NetLogArgs {
    #[arg(long, default_value_t = 50)]
    limit: usize,
    /// 按 URL 片段过滤
    #[arg(long = "match")]
    matches: Option<String>,
    /// 读取后清空
    #[arg(long)]
    clear: bool,
    /// 记录被规则放行（pass）的请求；默认只记命中规则的，排查漏 mock 时用
    #[arg(long)]
    debug_pass: bool,
    /// 日志来源：page 只看页面侧（fetch/XHR）、cdp 只看扩展侧、both 合并两者（默认）
    #[arg(long, value_parser = ["page", "cdp", "both"])]
    engine: Option<String>,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    frame: Option<i64>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct NetMockArgs {
    /// URL 片段或正则
    pattern: String,
    /// 伪造响应的状态码（不填时：只改请求/响应就不伪造，纯 mock 默认 200）
    #[arg(long)]
    status: Option<u16>,
    /// 响应体：以 { 或 [ 开头按 JSON，否则按纯文本
    #[arg(long)]
    body: Option<String>,
    #[arg(long)]
    method: Option<String>,
    /// 响应头，格式 'Name: value'（可重复）
    #[arg(long = "header", short = 'H')]
    headers: Vec<String>,
    /// 直接让请求失败（模拟网络错误）
    #[arg(long)]
    abort: bool,
    /// 追加/覆盖请求头，格式 'Name: value'（可重复）
    #[arg(long = "request-header")]
    request_headers: Vec<String>,
    /// 替换请求体（以 { 或 [ 开头按 JSON）
    #[arg(long = "request-body")]
    request_body: Option<String>,
    /// 放行真实请求但改写响应状态码
    #[arg(long = "response-status")]
    response_status: Option<u16>,
    /// 放行真实请求但改写响应体
    #[arg(long = "response-body")]
    response_body: Option<String>,
    /// 拦截引擎：page（默认，包装页面 fetch/XHR）/ cdp（扩展侧 Fetch 域，
    /// 能改 responseType=blob 的原始二进制体，也能拦 img/script 这类请求）
    #[arg(long, default_value = "page")]
    engine: String,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    frame: Option<i64>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct NetUnrouteArgs {
    pattern: String,
    /// 拦截引擎：page（默认，删页面内规则）/ cdp（删扩展侧 Fetch 域的规则）
    #[arg(long, default_value = "page")]
    engine: String,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    frame: Option<i64>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct WaitArgs {
    #[arg(long)]
    selector: Option<String>,
    #[arg(long)]
    text: Option<String>,
    #[arg(long)]
    js: Option<String>,
    /// 只等待元素可见（配合 --selector）
    #[arg(long)]
    visible: bool,
    #[arg(long, default_value_t = 10.0)]
    timeout: f64,
    #[arg(long, default_value_t = 0.1)]
    interval: f64,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    /// 在指定 iframe 内等待
    #[arg(long)]
    frame: Option<i64>,
}

#[derive(Debug, Args)]
struct ChallengeArgs {
    /// status（默认，识别当前页挑战/验证码）| wait（等自动放行）| solve（等人完成或自动通过，拿 token）| focus（带到前台）
    #[arg(default_value = "status")]
    action: String,
    /// wait/solve 的总超时（秒）；不传时由服务端按 action 取默认（wait 30s，solve 120s）
    #[arg(long)]
    timeout: Option<f64>,
    /// wait 的检查间隔（秒）
    #[arg(long, default_value_t = 0.25)]
    interval: f64,
    /// wait 期间超过这么多秒还没放行，就把标签页带到前台交给人工（0 表示不聚焦）
    #[arg(long = "focus-after", default_value_t = 8.0)]
    focus_after: f64,
    /// 禁止自动聚焦（默认会在 focus-after 后把标签页带到前台）
    #[arg(long = "no-focus")]
    no_focus: bool,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    frame: Option<i64>,
}

#[derive(Debug, Args)]
struct OpenArgs {
    /// URL / 本地文件；给多个时并发打开
    #[arg(required = true, num_args = 1..)]
    targets: Vec<String>,
    /// 后台打开：不切换当前标签页（默认是前台打开）
    #[arg(long, conflicts_with = "focus")]
    background: bool,
    #[arg(long)]
    window: bool,
    /// 打开后聚焦浏览器窗口（默认只切标签页，不抢系统焦点）
    #[arg(long)]
    focus: bool,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    session: Option<String>,
    #[arg(long = "group-title")]
    group_title: Option<String>,
    #[arg(long, default_value_t = 10.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct CloseArgs {
    #[arg(long)]
    tab: String,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct LogsArgs {
    #[arg(long, default_value_t = 100)]
    tail: usize,
}

#[derive(Debug, Args)]
struct InstallSkillArgs {
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct SetExtensionPortArgs {
    port: u16,
}

#[derive(Debug, Args)]
#[command(
    after_help = "扩展 ID 获取：chrome://extensions → 右上角打开「开发者模式」→ 本扩展卡片下方的「ID」（32 位 a-p 小写字母）。\n\
                  配置写入 <用户配置目录>/ws-trust.json，daemon 每次 WebSocket 握手都会重新读取，无需重启；\
                  已经连上的扩展要重连（在 chrome://extensions 重载本扩展）才会按新规则重新校验。\n\
                  扩展 ID 列表与 token 同时配置时两个条件都要满足。"
)]
struct TrustExtensionArgs {
    /// 扩展 ID（32 位 a-p 小写字母）；--remove 时必填
    extension_id: Option<String>,
    /// 把该扩展 ID 从信任列表移除
    #[arg(long)]
    remove: bool,
    /// 只查看当前信任配置，不修改
    #[arg(long)]
    list: bool,
    /// 清空扩展白名单与 token，回到「任何扩展都能连」的兼容状态
    #[arg(long)]
    clear: bool,
    /// 设置 WebSocket 握手 token（扩展侧需配置同一值，握手时以 ?token= 传入）
    #[arg(long, value_name = "VALUE")]
    token: Option<String>,
    /// 清除握手 token（只清 token，保留扩展白名单）
    #[arg(long = "clear-token")]
    clear_token: bool,
}

#[derive(Debug, Args)]
struct LookupCommand {
    #[command(subcommand)]
    target: LookupTarget,
}

#[derive(Debug, Subcommand)]
enum LookupTarget {
    Tab(LookupTabArgs),
    Browser(LookupBrowserArgs),
    Profile(LookupProfileArgs),
}

#[derive(Debug, Args)]
struct LookupTabArgs {
    tab: String,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    profile: Option<String>,
}

#[derive(Debug, Args)]
struct LookupBrowserArgs {
    browser: String,
}

#[derive(Debug, Args)]
struct LookupProfileArgs {
    profile: String,
}

#[derive(Debug, Args)]
struct ProfileLabelCommand {
    #[command(subcommand)]
    action: ProfileLabelAction,
}

#[derive(Debug, Subcommand)]
enum ProfileLabelAction {
    Set(ProfileLabelSetArgs),
    Clear(ProfileLabelClearArgs),
}

#[derive(Debug, Args)]
struct ProfileLabelSetArgs {
    label: String,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

#[derive(Debug, Args)]
struct ProfileLabelClearArgs {
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    browser: Option<String>,
    #[arg(long)]
    tab: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let is_async = cli.r#async;
    PRETTY_OUTPUT.store(cli.pretty, Ordering::Relaxed);
    match cli.command {
        CommandKind::Daemon => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(server::run_daemon())
        }
        CommandKind::Tabs(args) => {
            ensure_server()?;
            let mut path = "/tabs".to_string();
            let mut query = Vec::new();
            if let Some(browser) = args.browser {
                query.push(format!("browser={}", query_escape(&browser)));
            }
            if let Some(profile) = args.profile {
                query.push(format!("profile={}", query_escape(&profile)));
            }
            if args.full {
                query.push("full=true".to_string());
            }
            if !query.is_empty() {
                path.push('?');
                path.push_str(&query.join("&"));
            }
            print_json(request("GET", &path, None, 30.0)?);
            Ok(())
        }
        CommandKind::Tabtree(args) => {
            ensure_server()?;
            let mut path = "/tabtree".to_string();
            let mut query = Vec::new();
            if let Some(tab) = args.tab {
                query.push(format!("tab={}", query_escape(&tab)));
            }
            if let Some(browser) = args.browser {
                query.push(format!("browser={}", query_escape(&browser)));
            }
            if let Some(profile) = args.profile {
                query.push(format!("profile={}", query_escape(&profile)));
            }
            if args.full {
                query.push("full=true".to_string());
            }
            if !query.is_empty() {
                path.push('?');
                path.push_str(&query.join("&"));
            }
            print_json(request("GET", &path, None, 30.0)?);
            Ok(())
        }
        CommandKind::Lookup(args) => run_lookup_command(args),
        CommandKind::ProfileLabel(args) => run_profile_label_command(args),
        CommandKind::Scan(args) => {
            ensure_server()?;
            let payload = json!({
                "tabs_only": args.tabs_only,
                "text_only": args.text_only,
                "fast": args.fast,
                "max_nodes": args.max_nodes,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
                "timeout": args.timeout,
            });
            if is_async {
                print_json(submit_job("scan", payload)?);
            } else {
                print_json(request("POST", "/scan", Some(payload), args.timeout + 5.0)?);
            }
            Ok(())
        }
        CommandKind::Exec(args) => {
            ensure_server()?;
            let script = match args.file {
                // `--file -` 从 stdin 读取，方便 heredoc / 管道传脚本（Windows 下尤其省事）。
                Some(file) if file.as_os_str() == std::ffi::OsStr::new("-") => {
                    let mut buffer = String::new();
                    io::stdin().read_to_string(&mut buffer)?;
                    buffer
                }
                Some(file) => std::fs::read_to_string(file)?,
                None => args.script,
            };
            let payload = json!({
                "script": script,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
                "no_monitor": !args.monitor,
                "wait_js": args.wait_js,
                "wait_timeout": args.wait_timeout,
                "wait_interval": args.wait_interval,
                "timeout": args.timeout,
                "max_bytes": args.max_bytes,
                "world": args.world,
                "frame_id": args.frame,
                "all_frames": args.all_frames,
            });
            if is_async {
                print_json(submit_job("exec", payload)?);
            } else {
                // 客户端要覆盖「页面执行 + 轮询等待」两段，并比服务端多留余量，
                // 让服务端的结构化超时错误先返回，而不是 reqwest 的裸 "operation timed out"。
                print_json(request(
                    "POST",
                    "/exec",
                    Some(payload),
                    args.timeout + args.wait_timeout + 10.0,
                )?);
            }
            Ok(())
        }
        CommandKind::Cookies(args) => {
            ensure_server()?;
            let payload = json!({
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
                "url": args.url,
            });
            if is_async {
                print_json(submit_job("cookies", payload)?);
            } else {
                print_json(request("POST", "/cookies", Some(payload), args.timeout)?);
            }
            Ok(())
        }
        CommandKind::Upload(args) => {
            ensure_server()?;
            let payload = json!({
                "target": args.target,
                "files": args.files,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
                "timeout": args.timeout,
            });
            if is_async {
                print_json(submit_job("upload", payload)?);
            } else {
                print_json(request(
                    "POST",
                    "/upload",
                    Some(payload),
                    args.timeout + 5.0,
                )?);
            }
            Ok(())
        }
        CommandKind::Snapshot(args) => {
            ensure_server()?;
            let payload = json!({
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
                "offset": args.offset,
                "limit": args.limit,
                "details": args.details,
                "js": args.js,
                "interactive_only": args.interactive_only,
                "max_nodes": args.max_nodes,
                "timeout": args.timeout,
            });
            if is_async {
                print_json(submit_job("snapshot", payload)?);
            } else {
                print_json(request("POST", "/snapshot", Some(payload), args.timeout)?);
            }
            Ok(())
        }
        CommandKind::Click(args) => run_target_command("/click", "click", args, is_async),
        CommandKind::MouseClick(args) => {
            run_target_command("/mouse-click", "mouse-click", args, is_async)
        }
        CommandKind::Fill(args) => run_fill_command(args, is_async),
        CommandKind::SendKeys(args) => run_send_keys_command(args, is_async),
        CommandKind::Screenshot(args) => run_screenshot_command(args, is_async),
        CommandKind::SavePdf(args) => run_save_pdf_command(args, is_async),
        CommandKind::Network(args) => run_network_command(args),
        CommandKind::Console(args) => run_console_command(args),
        CommandKind::Job(args) => run_job_command(args),
        CommandKind::Run(args) => {
            if is_async {
                return Err(anyhow!(
                    "run 不支持 --async；需要后台执行请用 --async 提交单步任务，或把管道拆成多个 job"
                ));
            }
            ensure_server()?;
            let raw = std::fs::read_to_string(&args.file)
                .with_context(|| format!("读取步骤文件失败: {}", args.file.display()))?;
            let mut payload: Value = serde_json::from_str(&raw)
                .with_context(|| format!("步骤文件不是合法 JSON: {}", args.file.display()))?;
            if payload.is_array() {
                payload = json!({ "steps": payload });
            }
            let Some(object) = payload.as_object_mut() else {
                return Err(anyhow!("步骤文件必须是对象或数组"));
            };
            object
                .entry("stop_on_error")
                .or_insert(json!(!args.continue_on_error));
            // 管道可能包含多个动作，客户端超时给足；每一步自己有 timeout。
            print_json(request("POST", "/run", Some(payload), 600.0)?);
            Ok(())
        }
        CommandKind::Batch(args) => {
            ensure_server()?;
            let raw = std::fs::read_to_string(&args.file)
                .with_context(|| format!("读取步骤文件失败: {}", args.file.display()))?;
            let mut payload: Value = serde_json::from_str(&raw)
                .with_context(|| format!("步骤文件不是合法 JSON: {}", args.file.display()))?;
            if payload.is_array() {
                payload = json!({ "steps": payload });
            }
            let Some(object) = payload.as_object_mut() else {
                return Err(anyhow!("步骤文件必须是对象或数组"));
            };
            object.insert(
                "concurrency".to_string(),
                json!(args.concurrency.clamp(1, 32)),
            );
            print_json(request("POST", "/batch", Some(payload), 600.0)?);
            Ok(())
        }
        CommandKind::Read(args) => {
            ensure_server()?;
            let payload = json!({
                "selector": args.selector,
                "max_chars": args.max_chars,
                "links": args.links,
                "forms": !args.no_forms,
                "include_hidden": args.include_hidden,
                "budget_ms": args.budget_ms,
                "max_nodes": args.max_nodes,
                "frame_id": args.frame,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
                "timeout": args.timeout,
            });
            if is_async {
                print_json(submit_job("read", payload)?);
            } else {
                print_json(request("POST", "/read", Some(payload), args.timeout + 5.0)?);
            }
            Ok(())
        }
        CommandKind::Find(args) => {
            ensure_server()?;
            let payload = json!({
                "selector": args.selector,
                "text": args.text,
                "xpath": args.xpath,
                "limit": args.limit,
                "exact": args.exact,
                "visible": !args.include_hidden,
                "frame_id": args.frame,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
                "timeout": args.timeout,
            });
            if is_async {
                print_json(submit_job("find", payload)?);
            } else {
                print_json(request("POST", "/find", Some(payload), args.timeout + 5.0)?);
            }
            Ok(())
        }
        CommandKind::Hover(args) => run_action_command("hover", args, is_async),
        CommandKind::Select(args) => run_action_command("select", args, is_async),
        CommandKind::Check(args) => run_action_command("check", args, is_async),
        CommandKind::Scroll(args) => run_action_command("scroll", args, is_async),
        CommandKind::Inject(args) => {
            ensure_server()?;
            let (action, name, code) = if args.list {
                ("list", None, None)
            } else if let Some(name) = args.remove.clone() {
                ("remove", Some(name), None)
            } else {
                let code = match args.file.as_ref() {
                    Some(path) => std::fs::read_to_string(path)
                        .with_context(|| format!("读取脚本文件失败: {}", path.display()))?,
                    None => args.script.clone().ok_or_else(|| {
                        anyhow!("inject: 需要脚本内容，或 --file/--list/--remove")
                    })?,
                };
                match args.persist.clone() {
                    Some(name) => ("persist", Some(name), Some(code)),
                    None => ("run", None, Some(code)),
                }
            };
            let mut payload = json!({
                "action": action,
                "world": args.world,
                "frame_id": args.frame,
                "all_frames": args.all_frames,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
            });
            if let Some(name) = name {
                payload["name"] = json!(name);
            }
            if let Some(code) = code {
                payload["code"] = json!(code);
            }
            if !args.matches.is_empty() {
                payload["matches"] = json!(args.matches);
            }
            print_json(request(
                "POST",
                "/inject",
                Some(payload),
                args.timeout + 5.0,
            )?);
            Ok(())
        }
        CommandKind::Browsers => {
            ensure_server()?;
            print_json(request("GET", "/browsers", None, 30.0)?);
            Ok(())
        }
        CommandKind::Broadcast(args) => run_broadcast(args),
        CommandKind::Downloads(args) => run_downloads(args),
        CommandKind::Download(args) => run_download(args),
        CommandKind::DownloadWait(args) => run_download_wait(args),
        CommandKind::Net(cmd) => run_net_command(cmd),
        CommandKind::Frames(args) => {
            ensure_server()?;
            print_json(request(
                "POST",
                "/frames",
                Some(json!({
                    "switch_tab_id": args.tab,
                    "browser": args.browser,
                    "profile": args.profile,
                })),
                args.timeout,
            )?);
            Ok(())
        }
        CommandKind::Wait(args) => {
            ensure_server()?;
            let payload = json!({
                "selector": args.selector,
                "text": args.text,
                "js": args.js,
                "visible": args.visible,
                "timeout": args.timeout,
                "interval": args.interval,
                "frame_id": args.frame,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
            });
            if is_async {
                print_json(submit_job("wait", payload)?);
            } else {
                print_json(request("POST", "/wait", Some(payload), args.timeout + 5.0)?);
            }
            Ok(())
        }
        CommandKind::Challenge(args) => {
            ensure_server()?;
            let action = args.action.to_lowercase();
            let payload = json!({
                "action": action,
                // 不传就不下发：让服务端按 action 取默认（wait 30s / solve 120s）。
                // 否则 CLI 的 30s 默认值会把服务端的 120s 默认值永久压掉。
                "timeout": args.timeout,
                "interval": args.interval,
                "focus_after": args.focus_after,
                "focus": !args.no_focus,
                "frame_id": args.frame,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
            });
            // 等服务端的时间要比 wait 的总超时更长，否则 CLI 先断、命令还在跑。
            let effective_timeout =
                args.timeout
                    .unwrap_or(if action == "solve" { 120.0 } else { 30.0 });
            let http_timeout = if action == "wait" || action == "solve" {
                effective_timeout + 15.0
            } else {
                30.0
            };
            let mut value = request("POST", "/challenge", Some(payload), http_timeout)?;
            // 超时 / 未通过不是成功：把顶层 ok 改成 false，统一由 print_json 的
            // 「ok:false → 退出码 1」处理，避免这类命令只能靠 cleared/passed 字段判断。
            if (action == "wait" || action == "solve")
                && value.get("ok").and_then(Value::as_bool) == Some(true)
            {
                let achieved = if action == "solve" {
                    value
                        .pointer("/result/passed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                } else {
                    value
                        .pointer("/result/cleared")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                };
                if !achieved {
                    value["ok"] = json!(false);
                    value["error_code"] = json!("challenge_timeout");
                    value["error"] = json!(format!(
                        "challenge {action}: 超时未达成目标（页面仍被挑战页拦截），可用 --timeout 延长等待"
                    ));
                }
            }
            print_json(value);
            Ok(())
        }
        CommandKind::Back(args) => run_navigation_command("history.back(); return 'back'", args),
        CommandKind::Reload(args) => run_navigation_command(
            "setTimeout(() => location.reload(), 50); return 'reload'",
            args,
        ),
        CommandKind::Open(args) => {
            if !args.timeout.is_finite() || args.timeout <= 0.0 {
                return Err(anyhow!("open --timeout 必须是大于 0 的有限秒数"));
            }
            ensure_server()?;
            let cwd = env::current_dir().context("无法读取当前工作目录")?;
            let request_timeout = (args.timeout + 20.0).max(30.0);
            let build_payload = |target: &str| {
                json!({
                    "target": target,
                    "url": target,
                    "cwd": cwd,
                    // 默认前台打开（切到新标签页）；需要不打扰用户时显式 --background。
                    "active": !args.background,
                    "window": args.window,
                    "allow_focus": args.focus,
                    "keep_alive": true,
                    "switch_tab_id": args.tab,
                    "browser": args.browser,
                    "profile": args.profile,
                    "session": args.session,
                    "group_title": args.group_title,
                    "readiness_timeout": args.timeout,
                })
            };
            if args.targets.len() == 1 {
                print_json(request(
                    "POST",
                    "/open",
                    Some(build_payload(&args.targets[0])),
                    request_timeout,
                )?);
                return Ok(());
            }
            // 多个目标：并发打开。daemon 侧每个 /open 互相独立，
            // 串行开会把 N 个标签的等待时间叠加，这里直接开 N 个线程。
            let started = Instant::now();
            let mut handles = Vec::with_capacity(args.targets.len());
            for target in &args.targets {
                let payload = build_payload(target);
                handles.push(std::thread::spawn(move || {
                    request("POST", "/open", Some(payload), request_timeout)
                }));
            }
            let mut tabs = Vec::new();
            let mut failed = 0usize;
            for (index, handle) in handles.into_iter().enumerate() {
                match handle.join() {
                    Ok(Ok(value)) => {
                        let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
                        if !ok {
                            failed += 1;
                        }
                        tabs.push(json!({
                            "index": index,
                            "target": args.targets[index],
                            "ok": ok,
                            "result": value.get("result").cloned().unwrap_or(Value::Null),
                            "error": value.get("error").cloned().unwrap_or(Value::Null),
                        }));
                    }
                    Ok(Err(err)) => {
                        failed += 1;
                        tabs.push(json!({ "index": index, "target": args.targets[index], "ok": false, "error": err.to_string() }));
                    }
                    Err(_) => {
                        failed += 1;
                        tabs.push(json!({ "index": index, "target": args.targets[index], "ok": false, "error": "thread panicked" }));
                    }
                }
            }
            print_json(json!({
                "ok": failed == 0,
                "count": tabs.len(),
                "failed": failed,
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "tabs": tabs
            }));
            Ok(())
        }
        CommandKind::Close(args) => {
            ensure_server()?;
            print_json(request(
                "POST",
                "/close",
                Some(
                    json!({ "tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
                ),
                args.timeout,
            )?);
            Ok(())
        }
        CommandKind::Status => {
            print_json(status_value()?);
            Ok(())
        }
        CommandKind::Logs(args) => print_logs(args.tail),
        CommandKind::Doctor => {
            print_json(doctor_value()?);
            Ok(())
        }
        CommandKind::InstallSkill(args) => install_skill(args),
        CommandKind::SetExtensionPort(args) => {
            let was_running = is_server_alive();
            let config = config::set_extension_port(args.port)?;
            let mut restarted = false;
            if was_running {
                let _ = request("POST", "/shutdown", Some(json!({})), 3.0);
                wait_server_stopped(Duration::from_secs(5));
                ensure_server()?;
                restarted = true;
            }
            print_json(json!({
                "ok": true,
                "extension_port": config.extension_port,
                "restarted": restarted
            }));
            Ok(())
        }
        CommandKind::TrustExtension(args) => run_trust_extension(args),
        CommandKind::Stop => {
            match request("POST", "/shutdown", Some(json!({})), 3.0) {
                Ok(value) => print_json(value),
                // 端口还在监听却连不上/被拒 = 有一个用别的 token 的残留 daemon。
                // 这种情况必须报错：报 not_running 会让人以为已经停了，后续命令继续踩坑。
                Err(err) if is_server_alive() => print_json(json!({
                    "ok": false,
                    "status": "shutdown_failed",
                    "error": err.to_string(),
                    "hint": "18767 端口上还有一个 daemon 不认当前 token，它没被停掉。请先结束占用 18767 的进程（Windows: netstat -ano | findstr 18767 后 taskkill /PID <pid> /F）"
                })),
                Err(_) => print_json(json!({ "ok": true, "status": "not_running" })),
            }
            Ok(())
        }
        CommandKind::Restart => {
            let _ = request("POST", "/shutdown", Some(json!({})), 3.0);
            wait_server_stopped(Duration::from_secs(5));
            ensure_server()?;
            print_json(request("GET", "/health", None, 3.0)?);
            Ok(())
        }
    }?;
    // 让脚本/智能体可以用退出码判断成败；设置 AGENT_BROWSER_CLI_EXIT0=1 可恢复旧行为。
    let code = FAILURE_EXIT_CODE.load(Ordering::Relaxed);
    if code != 0 && env::var_os("AGENT_BROWSER_CLI_EXIT0").is_none() {
        std::process::exit(code);
    }
    Ok(())
}

fn query_escape(input: &str) -> String {
    input
        .bytes()
        .flat_map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => vec![b as char],
            _ => format!("%{b:02X}").chars().collect(),
        })
        .collect()
}

/// 复用同一个 HTTP Client：`reqwest::blocking::Client` 每次构建都会拉起独立的运行时线程，
/// 旧实现一条命令建 2 个，是热路径上最稳定的一笔固定开销。
fn http_client() -> Result<&'static Client> {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let built = Client::builder().build()?;
    let _ = CLIENT.set(built);
    Ok(CLIENT.get().expect("client just initialized"))
}

/// token 在进程内缓存；文件暂时不存在（冷启动）时不要缓存 None，
/// 否则会拿旧值去请求刚启动的 daemon，得到 401。
fn api_token() -> Option<String> {
    static TOKEN: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    let cell = TOKEN.get_or_init(|| Mutex::new(None));
    if let Some(token) = cell.lock().ok().and_then(|guard| guard.clone()) {
        return Some(token);
    }
    let fresh = config::load_api_token().ok().flatten();
    if let Some(token) = fresh.clone() {
        if let Ok(mut guard) = cell.lock() {
            *guard = Some(token);
        }
    }
    fresh
}

/// 一次 `/ping` 的三种结论。免鉴权、不触碰 token 缓存：冷启动时 daemon 刚写完 token，
/// 这里若走 request() 会把旧 token 固化进缓存。
enum DaemonPing {
    /// 端口没人监听，或监听方不是我们的 daemon。
    Down,
    /// daemon 正常。
    Ready,
    /// daemon 活着但插件端口没绑上（`/ping` 的 `ws_bind_error` 有值）。
    Degraded(String),
}

fn ping_state() -> DaemonPing {
    let Ok(client) = http_client() else {
        return DaemonPing::Down;
    };
    let Ok(value) = client
        .get(format!("http://{HOST}:{PORT}/ping"))
        .timeout(Duration::from_secs(1))
        .send()
        .and_then(|response| response.json::<Value>())
    else {
        return DaemonPing::Down;
    };
    if value.get("running").and_then(Value::as_bool) != Some(true) {
        return DaemonPing::Down;
    }
    match value.get("ws_bind_error").and_then(Value::as_str) {
        Some(err) => DaemonPing::Degraded(err.to_string()),
        None => DaemonPing::Ready,
    }
}

fn daemon_log_path_hint() -> String {
    config::daemon_log_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "<用户配置目录>/logs/daemon.log".to_string())
}

/// daemon 活着但插件端口没监听：扩展永远也连不上，必须当场说清楚，
/// 否则后续每条命令只会报「插件未连接」，看起来像扩展没装。
fn degraded_daemon_error(reason: &str) -> anyhow::Error {
    anyhow!(
        "daemon 在跑，但插件端口（默认 18765）没有监听：{reason}\n\
         最常见的原因是刚重启过 daemon，旧的扩展连接还在 TIME_WAIT（Windows 约 2 分钟），\
         新 daemon 绑定该端口时拿到 os error 10048。\n\
         处理：稍等一会再试；或立刻用 `agent-browser-cli set-extension-port <其它端口>` 换一个端口。\n\
         daemon 日志: {}",
        daemon_log_path_hint()
    )
}

fn request(method: &str, path: &str, payload: Option<Value>, timeout_secs: f64) -> Result<Value> {
    let client = http_client()?;
    let url = format!("http://{HOST}:{PORT}{path}");
    let timeout = Duration::from_secs_f64(timeout_secs.max(0.1));
    let builder = match method {
        "GET" => client.get(url).timeout(timeout),
        "POST" => client.post(url).timeout(timeout),
        _ => return Err(anyhow!("不支持的 HTTP 方法: {method}")),
    };
    let builder = match api_token() {
        Some(token) => builder.header(config::API_TOKEN_HEADER, token),
        None => builder,
    };
    let response = if method == "POST" {
        builder.json(&payload.unwrap_or_else(|| json!({}))).send()?
    } else {
        builder.send()?
    };
    let status = response.status();
    let value: Value = response.json().unwrap_or_else(|err| {
        json!({
            "ok": false,
            "error": format!("daemon 返回了非 JSON 响应 (HTTP {}): {err}", status.as_u16()),
            "error_code": "invalid_response"
        })
    });
    if value.get("error_code").and_then(Value::as_str) == Some("invalid_api_token") {
        let token_path = config::api_token_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "~/.agent-browser-cli/api-token".to_string());
        return Err(anyhow!(
            "daemon 正在运行，但 API token 不匹配（{token_path}）；通常是残留的旧 daemon 占用了 18767。\n\
             处理方式：结束占用 18767 的进程后重试，或先关闭其他 agent-browser-cli 实例"
        ));
    }
    Ok(value)
}

fn is_server_alive() -> bool {
    // 只做一次 TCP 探测：用于「端口有没有人在听」的粗判（restart/stop 等流程）。
    // 注意它无法区分「我们的 daemon」和「别的程序占着 18767」，
    // 所以 ensure_server() 的就绪判定必须用 /ping，不能用这个。
    tcp_listening(PORT)
}

fn ensure_server() -> Result<()> {
    // 就绪判定必须是 /ping 而不是 TCP：18767 可能被别的程序（或半死的旧 daemon）占着，
    // 只测 TCP 会把它当成可用，命令随后就变成一条 10s 的 reqwest 超时错误。
    match ping_state() {
        DaemonPing::Ready => return Ok(()),
        DaemonPing::Degraded(reason) => return Err(degraded_daemon_error(&reason)),
        DaemonPing::Down => {}
    }
    // 锁文件必须放在用户可写目录：装在只读位置（Program Files / Nix store / 只读容器）时，
    // 往 exe 目录写会让所有命令以一个裸 IO 错误失败。
    let lock_dir = config::user_config_dir()?;
    fs::create_dir_all(&lock_dir)
        .with_context(|| format!("创建配置目录失败: {}", lock_dir.display()))?;
    let lock_path = lock_dir.join("daemon.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    // Windows 上子进程会继承可继承句柄：daemon 一旦继承锁文件句柄，
    // 即使 CLI 退出，文件锁仍被 daemon 持有，后续所有命令都会卡在 lock_exclusive()。
    make_handle_non_inheritable(&lock);
    lock.lock_exclusive()?;
    let result = ensure_server_locked();
    let _ = lock.unlock();
    result
}

fn ensure_server_locked() -> Result<()> {
    // 拿到锁之后必须用 /ping 二次确认，不能只看 TCP：
    // 「别人刚 bind 了 18767 但 axum 还没就绪」和「端口被别的程序占着」都会让 TCP 探测成功，
    // 只凭 TCP 就认为「已经在跑」或不加判断地 spawn，会让两个 daemon 同时去抢插件端口
    // 18765，后启动的那个在 WebSocket bind 处报 os error 10048。
    match ping_state() {
        DaemonPing::Ready => return Ok(()),
        DaemonPing::Degraded(reason) => return Err(degraded_daemon_error(&reason)),
        DaemonPing::Down => {}
    }
    let log_path = daemon_log_path_hint();
    if is_server_alive() {
        // 18767 有人在听但 /ping 不通：可能是没有锁的旧版本 CLI 拉起的 daemon 正在就绪，
        // 也可能是别的程序占着端口。等一小会儿，仍然不通就明确报错，不再 spawn 第二个。
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            sleep(Duration::from_millis(150));
            match ping_state() {
                DaemonPing::Ready => return Ok(()),
                DaemonPing::Degraded(reason) => return Err(degraded_daemon_error(&reason)),
                DaemonPing::Down => {}
            }
        }
        return Err(anyhow!(
            "18767 已被占用，但对 /ping 无响应：可能是残留的旧 daemon 或别的程序。\n\
             处理：先结束占用 18767 的进程再重试（Windows: netstat -ano | findstr 18767 后 \
             taskkill /PID <pid> /F；macOS/Linux: lsof -i:18767）。\n\
             daemon 日志: {log_path}"
        ));
    }
    start_server()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        // 冷启动路径不敏感，用 /ping 确认 axum 已经能处理请求，避免只测到 TCP 握手。
        match ping_state() {
            DaemonPing::Ready => {
                // WebSocket 监听是独立 task，可能比 API 晚几毫秒才失败，再确认一次。
                sleep(Duration::from_millis(250));
                if let DaemonPing::Degraded(reason) = ping_state() {
                    return Err(degraded_daemon_error(&reason));
                }
                return Ok(());
            }
            DaemonPing::Degraded(reason) => return Err(degraded_daemon_error(&reason)),
            DaemonPing::Down => sleep(Duration::from_millis(150)),
        }
    }
    Err(anyhow!(
        "agent-browser-cli server 启动超时（15s）：18767 可能被其它进程占用，或 daemon 启动即退出。\n\
         daemon 日志: {log_path}（也可运行 agent-browser-cli logs --tail 100 查看）"
    ))
}

fn start_server() -> Result<()> {
    let exe = env::current_exe()?;
    let log_path = config::ensure_log_file()?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("打开 daemon 日志失败: {}", log_path.display()))?;
    let log_err = log.try_clone()?;
    let mut command = Command::new(exe);
    command
        .arg("daemon")
        // 别拿 exe 目录当工作目录：相对路径参数（如 screenshot --out）会以安装目录为基准，
        // 既不可预期，装在只读位置时也可能直接失败。
        .current_dir(dirs::home_dir().unwrap_or_else(env::temp_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：不弹控制台窗口。
        // DETACHED_PROCESS：daemon 不挂在调用方的控制台上。
        // CREATE_BREAKAWAY_FROM_JOB：调用方（智能体/PowerShell/CI）常把子进程放进
        //   带「关闭即杀」的 Job Object，不脱离的话 CLI 一退出 daemon 就被一起回收，
        //   导致每条命令都冷启动、并频繁出现端口占用冲突。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        command.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 后台 daemon 必须脱离当前终端会话，否则 CLI 退出后子进程会被一起回收。
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    // Windows: 调用方（智能体/PowerShell/Node）通常用管道捕获 CLI 输出。
    // 子进程默认会继承这些可继承句柄，导致 daemon 一直持有管道写端，
    // 上层读不到 EOF，命令看起来「卡死」直到超时。这里在 spawn 期间临时关闭继承。
    #[cfg(windows)]
    let inherit_guard = WindowsStdHandleInheritGuard::clear();
    let spawned = command.spawn();
    #[cfg(windows)]
    drop(inherit_guard);
    #[cfg(windows)]
    match spawned {
        Ok(_child) => {}
        Err(err) => {
            // 有些 Job Object 不允许 breakaway，退回不带该标志的方式再试一次。
            const ERROR_ACCESS_DENIED: i32 = 5;
            if err.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000 | 0x0000_0008);
                let inherit_guard = WindowsStdHandleInheritGuard::clear();
                let retry = command.spawn();
                drop(inherit_guard);
                retry?;
            } else {
                return Err(err.into());
            }
        }
    }
    #[cfg(not(windows))]
    spawned?;
    Ok(())
}

/// 临时清除当前进程 std 句柄的「可继承」标记，spawn 后恢复。
#[cfg(windows)]
struct WindowsStdHandleInheritGuard {
    saved: Vec<(*mut std::ffi::c_void, u32)>,
}

#[cfg(windows)]
impl WindowsStdHandleInheritGuard {
    fn clear() -> Self {
        use std::ffi::c_void;
        const STD_INPUT_HANDLE: u32 = -10i32 as u32;
        const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
        const STD_ERROR_HANDLE: u32 = -12i32 as u32;
        const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
        const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

        #[link(name = "kernel32")]
        extern "system" {
            fn GetStdHandle(std_handle: u32) -> *mut c_void;
            fn GetHandleInformation(object: *mut c_void, flags: *mut u32) -> i32;
            fn SetHandleInformation(object: *mut c_void, mask: u32, flags: u32) -> i32;
        }

        let mut saved = Vec::new();
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            unsafe {
                let handle = GetStdHandle(which);
                if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                    continue;
                }
                let mut flags: u32 = 0;
                if GetHandleInformation(handle, &mut flags) != 0
                    && (flags & HANDLE_FLAG_INHERIT) != 0
                    && SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) != 0
                {
                    saved.push((handle, flags));
                }
            }
        }
        Self { saved }
    }
}

#[cfg(windows)]
impl Drop for WindowsStdHandleInheritGuard {
    fn drop(&mut self) {
        const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
        #[link(name = "kernel32")]
        extern "system" {
            fn SetHandleInformation(object: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
        }
        for (handle, flags) in &self.saved {
            unsafe {
                let _ = SetHandleInformation(*handle, HANDLE_FLAG_INHERIT, *flags);
            }
        }
    }
}

fn wait_server_stopped(timeout: Duration) -> bool {
    // 除了 API 端口，还要等插件端口（WebSocket）也释放：旧 daemon 退出时两个监听
    // 不是同时关闭的，只要 API 一关就 spawn 新 daemon，新 daemon 会在 WS bind 处
    // 撞上还在监听的 18765（os error 10048），表现就是「扩展永远连不上」。
    let extension_port = config::load_existing()
        .map(|cfg| cfg.extension_port)
        .unwrap_or(config::DEFAULT_EXTENSION_PORT);
    let deadline = Instant::now() + timeout;
    loop {
        if !is_server_alive() && !tcp_listening(extension_port) {
            return true;
        }
        if Instant::now() >= deadline {
            return !is_server_alive() && !tcp_listening(extension_port);
        }
        sleep(Duration::from_millis(100));
    }
}

/// 关闭句柄继承，避免 daemon 继承 CLI 打开的启动锁 / 其它句柄。
#[cfg(windows)]
fn make_handle_non_inheritable(file: &std::fs::File) {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    extern "system" {
        fn SetHandleInformation(object: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    unsafe {
        let handle = file.as_raw_handle();
        let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
    }
}

#[cfg(not(windows))]
fn make_handle_non_inheritable(_file: &std::fs::File) {}

fn print_json(value: Value) {
    // 失败要能被 `$?` / `&&` 捕获：旧实现恒 exit 0，智能体只能靠解析文本来判断。
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        let code = match value.get("error_code").and_then(Value::as_str) {
            Some("job_wait_timeout") => EXIT_JOB_TIMEOUT,
            _ => EXIT_FAILURE,
        };
        FAILURE_EXIT_CODE.store(code, Ordering::Relaxed);
    }
    let text = if PRETTY_OUTPUT.load(Ordering::Relaxed) {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .unwrap_or_else(|_| "{}".to_string());
    println!("{text}");
}

fn status_value() -> Result<Value> {
    let configured = config::load_or_create()?.extension_port;
    let value = match request("GET", "/health", None, 1.0) {
        Ok(value) => enrich_status(value),
        // 端口在监听却拿不到 /health：最常见的原因不是「没启动」，而是残留的旧 daemon
        // 用了另一份 token（request() 对 invalid_api_token 直接返回 Err）。
        // 此时报 daemon_not_running 会把最该排查的原因藏起来。
        Err(err) if is_server_alive() => json!({
            "ok": false,
            "healthy": false,
            "summary": "daemon_token_mismatch",
            "message": "18767 端口上有一个 daemon 在监听，但 API token 不匹配（通常是残留的旧实例）",
            "hint": "先结束占用 18767 的进程（Windows: netstat -ano | findstr 18767 后 taskkill /PID <pid> /F；macOS/Linux: lsof -i:18767），再重试——新 daemon 启动时会刷新 token",
            "error": err.to_string(),
            "running": false,
            "ready": false,
            "ports": {
                "api": PORT,
                "extension": {
                    "configured": configured,
                    "listening": null,
                    "matched": false
                }
            },
            "connection": {
                "extension_connected": false,
                "active_tabs": 0
            }
        }),
        Err(_) => json!({
            "ok": false,
            "healthy": false,
            "summary": "daemon_not_running",
            "message": "daemon 未运行；执行 tabs/open/exec/scan 会自动启动，或运行 agent-browser-cli restart 手动启动",
            "running": false,
            "ready": false,
            "ports": {
                "api": PORT,
                "extension": {
                    "configured": configured,
                    "listening": null,
                    "matched": false
                }
            },
            "connection": {
                "extension_connected": false,
                "active_tabs": 0
            }
        }),
    };
    Ok(value)
}

fn enrich_status(mut value: Value) -> Value {
    let running = value
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ready = value.get("ready").and_then(Value::as_bool).unwrap_or(false);
    let matched = value
        .pointer("/ports/extension/matched")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let extension_connected = value
        .pointer("/connection/extension_connected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let active_tabs = value
        .pointer("/connection/active_tabs")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    // daemon 的插件端口没绑上（os error 10048 之类）：这时「插件未连接」是结果，不是原因。
    let bind_error = value
        .pointer("/ports/extension/bind_error")
        .and_then(Value::as_str)
        .filter(|err| !err.is_empty())
        .map(str::to_string);
    let (healthy, summary, message): (bool, &str, String) = if !running {
        (
            false,
            "daemon_not_running",
            "daemon 未运行；执行 tabs/open/exec/scan 会自动启动，或运行 agent-browser-cli restart 手动启动。\
             若 18767 已被别的进程/残留 daemon 占用，先结束它再启动"
                .to_string(),
        )
    } else if let Some(err) = bind_error {
        (
            false,
            "extension_port_bind_failed",
            format!(
                "插件端口没有监听：{err}。最常见的原因是刚重启过 daemon，旧连接还在 TIME_WAIT\
                 （Windows 约 2 分钟）；稍等一会重试，或 agent-browser-cli set-extension-port <其它端口> 换端口"
            ),
        )
    } else if !matched {
        (
            false,
            "port_mismatch",
            "CLI 配置的插件端口和 daemon 实际监听端口不一致；运行 agent-browser-cli restart 让配置生效"
                .to_string(),
        )
    } else if !extension_connected {
        (
            false,
            "extension_not_connected",
            "浏览器插件未连接；确认 Chrome 已打开、插件已启用，且插件端口和 CLI 配置一致"
                .to_string(),
        )
    } else if active_tabs == 0 || !ready {
        (
            false,
            "no_active_tabs",
            "没有可用的普通网页标签页；请在 Chrome 打开一个 http/https 页面后重试".to_string(),
        )
    } else {
        (
            true,
            "ready",
            "daemon、插件连接和浏览器标签页均可用".to_string(),
        )
    };
    // 只有真的 healthy 才 ok：否则 status 恒 ok:true，`$?` 永远判不出问题。
    value["ok"] = json!(healthy);
    value["healthy"] = json!(healthy);
    value["summary"] = json!(summary);
    value["message"] = json!(message);
    // 扩展是磁盘上的文件副本：改了不重新加载，Chrome 里跑的还是旧代码，
    // 表现就是「明明修好了但报错照旧」。这里把它变成显式的一行提示。
    if value
        .pointer("/extension/stale")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        if let Some(hint) = value.pointer("/extension/hint").cloned() {
            value["warning"] = hint;
        }
    }
    value
}

fn print_logs(tail: usize) -> Result<()> {
    let path = config::daemon_log_path()?;
    if !path.exists() {
        return Err(anyhow!(
            "日志文件不存在或尚无日志: {}；先运行 agent-browser-cli tabs/open/exec/scan 或 restart 启动 daemon",
            path.display()
        ));
    }
    if tail == 0 {
        return Ok(());
    }
    let lines =
        tail_lines(&path, tail).with_context(|| format!("读取日志文件失败: {}", path.display()))?;
    for line in &lines {
        println!("{line}");
    }
    Ok(())
}

/// 只从文件尾部按块倒读，凑够 `tail` 行就停：日志涨到几百 MB 时不必整份读进内存。
fn tail_lines(path: &Path, tail: usize) -> Result<Vec<String>> {
    use std::io::{Seek, SeekFrom};

    const CHUNK: u64 = 64 * 1024;
    let mut file = fs::File::open(path)?;
    let mut end = file.metadata()?.len();
    if end == 0 {
        return Ok(Vec::new());
    }
    // 多要一个换行：文件末尾没有换行时，最后一行也要算一行。
    let mut newlines = 0usize;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    while end > 0 && newlines < tail.saturating_add(1) {
        let start = end.saturating_sub(CHUNK);
        let mut chunk = vec![0u8; (end - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk)?;
        newlines += chunk.iter().filter(|byte| **byte == b'\n').count();
        chunks.push(chunk);
        end = start;
    }
    let mut buffer = Vec::new();
    for chunk in chunks.into_iter().rev() {
        buffer.extend_from_slice(&chunk);
    }
    // 没读到文件头时，第一条一定是被块边界切断的半行（也可能切断多字节字符），
    // 用 from_utf8_lossy 兜住非法字节后整行丢弃。
    let text = String::from_utf8_lossy(&buffer);
    let all: Vec<&str> = text.lines().collect();
    let skip = usize::from(end > 0 && !all.is_empty());
    let lines = &all[skip..];
    let from = lines.len().saturating_sub(tail);
    Ok(lines[from..].iter().map(|line| line.to_string()).collect())
}

fn doctor_value() -> Result<Value> {
    let exe_ok = env::current_exe().map(|p| p.is_file()).unwrap_or(false);
    let config_path = config::config_path()?;
    let config_exists = config_path.exists();
    let config_result = config::load_existing();
    let (configured_port, config_ok, config_hint) = match config_result {
        Ok(cfg) => (Some(cfg.extension_port), true, Value::Null),
        Err(err) => (None, false, json!(err.to_string())),
    };
    let api_reachable = tcp_listening(PORT);
    let health = request("GET", "/health", None, 1.0).ok();
    let listening_port = health
        .as_ref()
        .and_then(|v| v.pointer("/ports/extension/listening"))
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok());
    let extension_port_to_check = listening_port.or(configured_port);
    let extension_port_ok = extension_port_to_check.map(tcp_listening).unwrap_or(false);
    let extension_connected = health
        .as_ref()
        .and_then(|v| v.pointer("/connection/extension_connected"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let active_tabs = health
        .as_ref()
        .and_then(|v| v.pointer("/connection/active_tabs"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let file_scheme_profiles = health
        .as_ref()
        .and_then(|v| v.pointer("/connection/profiles"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let extension_stale = health
        .as_ref()
        .and_then(|v| v.pointer("/extension/stale"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let extension_bundled = health
        .as_ref()
        .and_then(|v| v.pointer("/extension/bundled_version"))
        .cloned()
        .unwrap_or(Value::Null);
    let extension_versions = health
        .as_ref()
        .and_then(|v| v.pointer("/extension/connected_versions"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let running = health
        .as_ref()
        .and_then(|v| v.get("running"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ready = health
        .as_ref()
        .and_then(|v| v.get("ready"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let port_conflict = configured_port == Some(PORT);
    let port_matched =
        configured_port.is_some() && listening_port.is_some() && configured_port == listening_port;
    let log_path = config::daemon_log_path()?;

    let mut checks = Vec::new();
    checks.push(json!({
        "name": "cli",
        "ok": exe_ok,
        "hint": if exe_ok { Value::Null } else { json!("当前 CLI 二进制不可执行或无法定位") }
    }));
    checks.push(json!({
        "name": "api",
        "ok": api_reachable,
        "host": HOST,
        "port": PORT,
        "hint": if api_reachable { Value::Null } else { json!("daemon API 未监听；doctor 不会自动启动 daemon，可运行 agent-browser-cli restart") }
    }));
    checks.push(json!({
        "name": "config",
        "ok": config_ok,
        "path": config_path,
        "exists": config_exists,
        "extension_port": configured_port,
        "hint": config_hint
    }));
    checks.push(json!({
        "name": "extension_port",
        "ok": extension_port_ok && !port_conflict,
        "configured_port": configured_port,
        "listening_port": listening_port,
        "port_matched": port_matched,
        "hint": extension_port_hint(configured_port, listening_port, extension_port_ok, port_conflict)
    }));
    checks.push(json!({
        "name": "health",
        "ok": running,
        "running": running,
        "ready": ready,
        "extension_connected": extension_connected,
        "active_tabs": active_tabs,
        "hint": if running { Value::Null } else { json!("daemon 未运行；运行 agent-browser-cli restart 或执行 tabs/open/exec/scan 自动启动") }
    }));
    checks.push(json!({
        "name": "extension_connected",
        "ok": extension_connected,
        "configured_port": configured_port,
        "listening_port": listening_port,
        "connected": extension_connected,
        "hint": if extension_connected { Value::Null } else { json!(format!("浏览器插件未连接；确认插件已启用，且插件端口为 {}", configured_port.unwrap_or(config::DEFAULT_EXTENSION_PORT))) }
    }));
    checks.push(json!({
        "name": "active_tabs",
        "ok": active_tabs > 0,
        "active_tabs": active_tabs,
        "hint": if active_tabs > 0 { Value::Null } else { json!("Chrome 需要至少打开一个普通 http/https/file 网页标签页") }
    }));
    checks.push(json!({
        "name": "extension_fresh",
        "ok": !extension_stale,
        "optional": true,
        "bundled_version": extension_bundled,
        "connected_versions": extension_versions,
        "hint": if extension_stale {
            json!("浏览器里的扩展版本低于磁盘上的版本：打开 chrome://extensions 对本扩展点一次「重新加载」，否则代码改动不会生效")
        } else {
            Value::Null
        }
    }));
    checks.push(json!({
        "name": "file_scheme_access",
        "ok": true,
        "optional": true,
        "profiles": file_scheme_profiles,
        "hint": "文件网址访问是可选能力；关闭时不影响普通 http/https 页面控制"
    }));

    let ok = checks.iter().all(|c| {
        c.get("optional").and_then(Value::as_bool).unwrap_or(false)
            || c.get("ok").and_then(Value::as_bool).unwrap_or(false)
    });
    Ok(json!({
        "ok": ok,
        "checks": checks,
        "log_path": log_path,
        "log_exists": log_path.exists()
    }))
}

fn extension_port_hint(
    configured: Option<u16>,
    listening: Option<u16>,
    extension_port_ok: bool,
    port_conflict: bool,
) -> Value {
    if port_conflict {
        json!("插件端口不能和 API 端口 18767 相同；运行 agent-browser-cli set-extension-port <port> 修改")
    } else if configured.is_some() && listening.is_some() && configured != listening {
        json!("CLI 配置和 daemon 实际监听端口不一致；运行 agent-browser-cli restart 让配置生效")
    } else if !extension_port_ok {
        json!("当前插件端口未监听；daemon 未启动或 WebSocket 端口启动失败")
    } else {
        Value::Null
    }
}

fn tcp_listening(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("{HOST}:{port}").parse().expect("固定地址应可解析"),
        Duration::from_millis(300),
    )
    .is_ok()
}

#[derive(Debug)]
struct SkillPlanItem {
    path: PathBuf,
    action: String,
    will_write: bool,
    note: String,
}

fn install_skill(args: InstallSkillArgs) -> Result<()> {
    let source = resolve_skill_source()?;
    let home = home_dir()?;
    let main_target = home.join(".agents/skills/agent-browser-cli");
    let mut plan = Vec::new();
    plan.push(SkillPlanItem {
        path: main_target.clone(),
        action: if main_target.exists() {
            "update_main"
        } else {
            "create_main"
        }
        .to_string(),
        will_write: true,
        note: format!("复制内置 skill 目录: {}", source.display()),
    });

    for parent in [
        home.join(".codex/skills"),
        home.join(".claude/skills"),
        home.join(".config/agents/skills"),
        home.join(".cursor/skills"),
        home.join(".gemini/skills"),
    ] {
        let target = parent.join("agent-browser-cli");
        if !parent.exists() {
            plan.push(SkillPlanItem {
                path: target,
                action: "skip_missing_parent".to_string(),
                will_write: false,
                note: "父目录不存在，跳过软链接".to_string(),
            });
            continue;
        }
        let meta = fs::symlink_metadata(&target).ok();
        if let Some(meta) = meta {
            if meta.file_type().is_symlink() {
                plan.push(SkillPlanItem {
                    path: target,
                    action: "update_symlink".to_string(),
                    will_write: true,
                    note: format!("更新软链接到 {}", main_target.display()),
                });
            } else {
                plan.push(SkillPlanItem {
                    path: target,
                    action: "skip_existing_entity".to_string(),
                    will_write: false,
                    note: "目标已存在且不是软链接，必须手动处理，--yes 也不会覆盖".to_string(),
                });
            }
        } else {
            plan.push(SkillPlanItem {
                path: target,
                action: "create_symlink".to_string(),
                will_write: true,
                note: format!("创建软链接到 {}", main_target.display()),
            });
        }
    }

    print_skill_plan(&plan, args.dry_run);
    if args.dry_run {
        return Ok(());
    }
    if !args.yes && !confirm_install()? {
        println!("已取消安装");
        return Ok(());
    }

    copy_dir_clean(&source, &main_target)?;
    for item in plan
        .iter()
        .filter(|item| item.action.ends_with("symlink") && item.will_write)
    {
        if let Some(parent) = item.path.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::symlink_metadata(&item.path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            remove_path(&item.path)?;
        }
        create_symlink_dir(&main_target, &item.path)?;
    }
    println!("安装完成: {}", main_target.join("SKILL.md").display());
    Ok(())
}

fn print_skill_plan(plan: &[SkillPlanItem], dry_run: bool) {
    println!("agent-browser-cli skill 安装计划");
    if dry_run {
        println!("模式: dry-run，不写文件、不创建软链接");
    }
    for item in plan {
        println!(
            "- [{}] {} -> {} ({})",
            if item.will_write { "write" } else { "skip" },
            item.action,
            item.path.display(),
            item.note
        );
    }
}

fn confirm_install() -> Result<bool> {
    print!("确认执行以上安装计划？输入 yes 继续: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim() == "yes")
}

fn resolve_skill_source() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(package_dir) = env::var_os("AGENT_BROWSER_CLI_PACKAGE_DIR") {
        candidates.push(PathBuf::from(package_dir).join("skills/agent-browser-cli"));
    }
    if let Ok(exe) = env::current_exe() {
        for ancestor in exe.ancestors().take(6) {
            candidates.push(ancestor.join("skills/agent-browser-cli"));
        }
    }
    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd.join("skills/agent-browser-cli"));
    }
    for candidate in candidates {
        if candidate.join("SKILL.md").is_file() {
            return Ok(candidate);
        }
    }
    Err(anyhow!("找不到内置 skill 目录 skills/agent-browser-cli"))
}

fn copy_dir_clean(source: &Path, target: &Path) -> Result<()> {
    if fs::symlink_metadata(target)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(anyhow!("主安装目录不能是软链接: {}", target.display()));
    }
    // 先拷到同级临时目录，成功后再 rename 换上去：中途失败时旧目录仍然完整，
    // 不会留下「目录已被清空、新文件还没拷完」的半成品。
    let tmp = target.with_file_name(format!(
        ".{}.tmp-{}",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("agent-browser-cli"),
        std::process::id()
    ));
    if fs::symlink_metadata(&tmp).is_ok() {
        fs::remove_dir_all(&tmp).with_context(|| format!("清理临时目录失败: {}", tmp.display()))?;
    }
    let copied = (|| -> Result<()> {
        fs::create_dir_all(&tmp).with_context(|| format!("创建临时目录失败: {}", tmp.display()))?;
        copy_dir_contents(source, &tmp)
    })();
    if let Err(err) = copied {
        let _ = fs::remove_dir_all(&tmp);
        return Err(err);
    }
    if fs::symlink_metadata(target).is_ok() {
        remove_path(target).with_context(|| format!("清理主安装目录失败: {}", target.display()))?;
    }
    if let Err(err) = fs::rename(&tmp, target) {
        // 旧目录已经删掉，临时目录里的内容就是唯一的一份，别顺手清掉。
        return Err(err).with_context(|| {
            format!(
                "替换主安装目录失败: {}（新内容已保留在 {}）",
                target.display(),
                tmp.display()
            )
        });
    }
    Ok(())
}

/// 递归复制目录内容（不做清理），供 `copy_dir_clean` 往临时目录里写。
fn copy_dir_contents(source: &Path, target: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let src = entry.path();
        let dst = target.join(entry.file_name());
        let meta = entry.file_type()?;
        if meta.is_dir() {
            fs::create_dir_all(&dst).with_context(|| format!("创建目录失败: {}", dst.display()))?;
            copy_dir_contents(&src, &dst)?;
        } else if meta.is_file() {
            fs::copy(&src, &dst).with_context(|| format!("复制文件失败: {}", src.display()))?;
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || meta.is_file() {
        fs::remove_file(path)?;
    } else if meta.is_dir() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_symlink_dir(source: &Path, target: &Path) -> Result<()> {
    std::os::unix::fs::symlink(source, target)
        .with_context(|| format!("创建软链接失败: {}", target.display()))
}

#[cfg(windows)]
fn create_symlink_dir(source: &Path, target: &Path) -> Result<()> {
    std::os::windows::fs::symlink_dir(source, target)
        .with_context(|| format!("创建软链接失败: {}", target.display()))
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("无法定位用户主目录"))
}

// ---------------------------------------------------------------- WebSocket 信任配置
//
// 契约来自 server.rs 的 `WsTrust`：`{ "extension_ids": [...], "token": "..." }`。
// - extension_ids 非空 → 只允许列表内的扩展 ID 连接；
// - token 非空 → 握手必须带 `?token=<同值>`；
// - 两者都空 → 兼容模式（任何扩展都能连）。
// daemon 每次握手都重新读这个文件，所以 CLI 改完不需要重启。

/// 与 server.rs 的 `ws_trust_path()` 保持同一路径：`<用户配置目录>/ws-trust.json`。
fn ws_trust_path() -> Result<PathBuf> {
    Ok(config::user_config_dir()?.join("ws-trust.json"))
}

/// Chrome 扩展 ID 的字母表是 a-p，固定 32 位；这里大小写不敏感并归一成小写。
fn normalize_extension_id(raw: &str) -> Result<String> {
    let id = raw.trim().to_ascii_lowercase();
    if id.len() == 32 && id.bytes().all(|byte| matches!(byte, b'a'..=b'p')) {
        return Ok(id);
    }
    Err(anyhow!(
        "扩展 ID 不合法: {raw}\n\
         扩展 ID 是 32 位 a-p 小写字母（形如 abcdefghijklmnopabcdefghijklmnop）。\n\
         获取方式：chrome://extensions → 打开右上角「开发者模式」→ 本扩展卡片下方的「ID」"
    ))
}

/// 读取信任配置。
///
/// `allow_empty` 决定"文件存在但为空"算不算错误：
/// - 只读命令（`--list`）传 `false`：空文件按**损坏**处理。daemon 侧同样是
///   fail-closed（空 ≠ 未配置），两边语义必须一致，否则 CLI 说"兼容模式"、
///   daemon 却在拒绝所有握手，用户会以为daemon 坏了。
/// - 写入命令（`--clear` / `<id>` / `--token`）传 `true`：空文件当成"待写入的空配置"，
///   让命令能**自愈**，而不是逼用户先手动删文件再重跑。
fn load_ws_trust_json_inner(path: &Path, allow_empty: bool) -> Result<Value> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        // 文件不存在等于「未配置」，即兼容模式，不要报错。
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(json!({})),
        Err(err) => {
            return Err(err).with_context(|| format!("读取信任配置失败: {}", path.display()))
        }
    };
    if text.trim().is_empty() {
        if allow_empty {
            return Ok(json!({}));
        }
        return Err(anyhow!(
            "信任配置为空（{}）：空文件不会被当成「未配置」，daemon 会拒绝所有 WebSocket 连接。\
             要回到兼容模式请删除该文件；要重新配置可运行 agent-browser-cli trust-extension <id>（会直接覆盖）",
            path.display()
        ));
    }
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("信任配置不是合法 JSON: {}", path.display()))?;
    if !value.is_object() {
        return Err(anyhow!(
            "信任配置必须是 JSON 对象（形如 {{\"extension_ids\": [...], \"token\": \"...\"}}）: {}",
            path.display()
        ));
    }
    Ok(value)
}

fn load_ws_trust_json(path: &Path) -> Result<Value> {
    load_ws_trust_json_inner(path, false)
}

fn load_ws_trust_json_for_update(path: &Path) -> Result<Value> {
    load_ws_trust_json_inner(path, true)
}

/// 先写同目录临时文件再原子替换：daemon 可能随时在读，不能让它读到半个 JSON。
///
/// 复用 `config::replace_file_atomically`（Windows 上是「备份 → 替换 → 删备份」，
/// 替换失败会把备份放回），**不能改成先删目标再 rename**：一旦中间失败/进程被杀，
/// `ws-trust.json` 就会消失，daemon 把 `NotFound` 当成「从没配过」→ 回到兼容模式，
/// 用户以为配好的白名单静默失效（fail-open）。
fn save_ws_trust_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;
    }
    let tmp = path.with_file_name(format!(".ws-trust-{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(value)? + "\n";
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 可能包含握手 token，按「仅本人可读写」落盘。
        // rename 保留临时文件的权限位，所以替换后的目标同样是 0600。
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options
            .open(&tmp)
            .with_context(|| format!("创建临时文件失败: {}", tmp.display()))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        // Windows 上打开中的文件不能改名，先显式关闭句柄（与 save_api_token 一致）。
        drop(file);
        config::replace_file_atomically(&tmp, path)
            .with_context(|| format!("保存信任配置失败: {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// 读 extension_ids 并按原顺序去重（服务端比对时会各自 trim + 忽略大小写）。
fn trust_ids_of(value: &Value) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    if let Some(items) = value.get("extension_ids").and_then(Value::as_array) {
        for item in items {
            let Some(raw) = item.as_str() else { continue };
            let id = raw.trim().to_ascii_lowercase();
            if !id.is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

/// 与 server.rs 的 `WsTrust::token_value()` 同语义：trim 后非空才算配置了 token。
fn trust_token_of(value: &Value) -> Option<String> {
    value
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

/// `/reload-trust` 的结果。
///
/// 必须区分四态：`Failed` 与 `NotRunning` 对用户是**完全不同**的处置建议
/// （前者该去看 token/权限，后者只是 daemon 没起）。早先两者都归成"未连接到
/// daemon"，会在 token 不匹配或配置损坏时把人引向错误方向。
enum TrustReload {
    /// 没发起（只读命令，或配置没变）
    Skipped,
    /// daemon 没在跑：配置在下次启动时生效
    NotRunning,
    /// 成功，携带 daemon 回包（含 checked/revoked）
    Done(Value),
    /// 连上了但被拒或出错：原样带出原因
    Failed(String),
}

/// 通知运行中的 daemon 重新核对已建立连接。
///
/// 信任文件本身已经写好了，这里只是「顺带把已经在连的不可信扩展踢掉」，
/// 失败不该让命令整体失败——但**必须如实说明是哪一种失败**。
fn reload_daemon_trust() -> TrustReload {
    if !is_server_alive() {
        return TrustReload::NotRunning;
    }
    match request("POST", "/reload-trust", Some(json!({})), 5.0) {
        Ok(value) => {
            if value.get("ok").and_then(Value::as_bool) == Some(true) {
                return TrustReload::Done(value);
            }
            let reason = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("daemon 拒绝了 /reload-trust")
                .to_string();
            let code = value.get("error_code").and_then(Value::as_str);
            TrustReload::Failed(match code {
                Some(code) => format!("{reason}（{code}）"),
                None => reason,
            })
        }
        Err(err) => TrustReload::Failed(err.to_string()),
    }
}

fn print_trust_state(
    path: &Path,
    value: &Value,
    action: &str,
    changed: bool,
    reload: &TrustReload,
) -> Result<()> {
    let ids = trust_ids_of(value);
    let token_required = trust_token_of(value).is_some();
    let enforcing = !ids.is_empty() || token_required;
    let done = match reload {
        TrustReload::Done(value) => Some(value),
        _ => None,
    };
    let revoked = done
        .and_then(|value| value.get("revoked"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let checked = done
        .and_then(|value| value.get("checked"))
        .and_then(Value::as_u64);
    let message = if !enforcing {
        "未启用信任校验（兼容模式）：任何 Chrome 扩展都能连接。本机开发够用；多扩展/多人共用同一台机器建议 trust-extension <id>".to_string()
    } else {
        match reload {
            // 只读命令或配置没变：说明生效机制即可，不提"断开"。
            TrustReload::Skipped => {
                "已启用 WebSocket 信任校验：daemon 在每次握手时重新读取该文件，无需重启".to_string()
            }
            TrustReload::NotRunning => {
                "已启用 WebSocket 信任校验：已写入配置，但 daemon 没有运行，将在下次启动时生效"
                    .to_string()
            }
            TrustReload::Done(_) if revoked.is_empty() => {
                "已启用 WebSocket 信任校验；已复查当前连接，没有需要断开的扩展".to_string()
            }
            TrustReload::Done(_) => format!(
                "已启用 WebSocket 信任校验，并已断开 {} 条不再可信的连接；之后的握手按新规则校验",
                revoked.len()
            ),
            // 关键：不要把「被拒」说成「未连接」。
            TrustReload::Failed(err) => format!(
                "配置已保存，但通知运行中的 daemon 失败：{err}。已连接的扩展可能仍按旧规则放行；\
                 确认 daemon 状态（agent-browser-cli status）后重试，或运行 restart 确保生效"
            ),
        }
    };
    let mut payload = json!({
        "ok": true,
        "action": action,
        "changed": changed,
        "extension_ids": ids,
        "token_required": token_required,
        "enforcing": enforcing,
        "path": path,
        "message": message,
    });
    if let Some(checked) = checked {
        payload["connections_checked"] = json!(checked);
        payload["connections_revoked"] = json!(revoked.len());
        payload["revoked"] = json!(revoked);
    }
    if let TrustReload::Failed(err) = reload {
        // 机器可读：调用方不该靠解析中文来判断有没有生效。
        payload["reload_applied"] = json!(false);
        payload["reload_error"] = json!(err);
    } else {
        payload["reload_applied"] = json!(!matches!(
            reload,
            TrustReload::NotRunning | TrustReload::Skipped
        ));
    }
    print_json(payload);
    Ok(())
}

fn run_trust_extension(args: TrustExtensionArgs) -> Result<()> {
    let path = ws_trust_path()?;
    let other_than_list = args.extension_id.is_some()
        || args.remove
        || args.clear
        || args.token.is_some()
        || args.clear_token;
    let other_than_clear =
        args.extension_id.is_some() || args.remove || args.token.is_some() || args.clear_token;
    if args.list {
        if other_than_list {
            return Err(anyhow!(
                "--list 只读，不能和 <id> / --remove / --clear / --token / --clear-token 同时使用"
            ));
        }
        let value = load_ws_trust_json(&path)?;
        return print_trust_state(&path, &value, "list", false, &TrustReload::Skipped);
    }
    if args.clear {
        if other_than_clear {
            return Err(anyhow!(
                "--clear 会清空白名单与 token，不能和 <id> / --remove / --token / --clear-token 同时使用"
            ));
        }
        save_ws_trust_json(&path, &json!({ "extension_ids": [], "token": null }))?;
        let value = load_ws_trust_json(&path)?;
        // 清空 = 回到兼容模式，此时不应再踢任何连接（白名单为空表示「谁都能连」）。
        let reload = reload_daemon_trust();
        return print_trust_state(&path, &value, "clear", true, &reload);
    }
    if args.remove && args.extension_id.is_none() {
        return Err(anyhow!(
            "--remove 需要同时给出扩展 ID：agent-browser-cli trust-extension <id> --remove"
        ));
    }
    if args.clear_token && args.token.is_some() {
        return Err(anyhow!("--token 和 --clear-token 不能同时使用"));
    }
    if args.extension_id.is_none() && args.token.is_none() && !args.clear_token {
        return Err(anyhow!(
            "需要扩展 ID，或改用 --list / --clear / --token <值> / --clear-token"
        ));
    }

    let mut value = load_ws_trust_json_for_update(&path)?;
    let before = value.clone();
    let mut ids = trust_ids_of(&value);
    let mut actions: Vec<&str> = Vec::new();
    if let Some(raw) = args.extension_id.as_deref() {
        let id = normalize_extension_id(raw)?;
        if args.remove {
            let before_len = ids.len();
            ids.retain(|item| item != &id);
            actions.push(if ids.len() != before_len {
                "removed"
            } else {
                "remove_noop"
            });
        } else if ids.iter().any(|item| item == &id) {
            // 幂等：已经在列表里就不重复插入。
            actions.push("already_trusted");
        } else {
            ids.push(id);
            actions.push("added");
        }
    }
    let mut token = trust_token_of(&value);
    if let Some(raw) = args.token.as_deref() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(anyhow!(
                "--token 不能是空字符串；要清除 token 请用 --clear-token"
            ));
        }
        token = Some(trimmed.to_string());
        actions.push("token_set");
    } else if args.clear_token {
        token = None;
        actions.push("token_cleared");
    }

    // 只在原有对象上改这两个字段，保留未来可能新增的其它键。
    if let Some(object) = value.as_object_mut() {
        object.insert("extension_ids".to_string(), json!(ids));
        object.insert(
            "token".to_string(),
            token.map(Value::String).unwrap_or(Value::Null),
        );
    }
    let changed = value != before;
    if changed {
        save_ws_trust_json(&path, &value)?;
    }
    // 写盘后才通知 daemon：让它按新配置复核已建立连接并把不可信的踢掉。
    // 配置没变就不必打扰 daemon（反复执行同一条命令不该引起断连）。
    let reload = if changed {
        reload_daemon_trust()
    } else {
        TrustReload::Skipped
    };
    print_trust_state(&path, &value, &actions.join("+"), changed, &reload)
}

fn run_lookup_command(args: LookupCommand) -> Result<()> {
    ensure_server()?;
    let path = match args.target {
        LookupTarget::Tab(args) => {
            let mut path = format!("/lookup/tab/{}", query_escape(&args.tab));
            let mut query = Vec::new();
            if let Some(browser) = args.browser {
                query.push(format!("browser={}", query_escape(&browser)));
            }
            if let Some(profile) = args.profile {
                query.push(format!("profile={}", query_escape(&profile)));
            }
            if !query.is_empty() {
                path.push('?');
                path.push_str(&query.join("&"));
            }
            path
        }
        LookupTarget::Browser(args) => format!("/lookup/browser/{}", query_escape(&args.browser)),
        LookupTarget::Profile(args) => format!("/lookup/profile/{}", query_escape(&args.profile)),
    };
    print_json(request("GET", &path, None, 30.0)?);
    Ok(())
}

fn run_profile_label_command(args: ProfileLabelCommand) -> Result<()> {
    ensure_server()?;
    match args.action {
        ProfileLabelAction::Set(args) => {
            print_json(request(
                "POST",
                "/profile-label",
                Some(json!({
                    "label": args.label,
                    "profile": args.profile,
                    "browser": args.browser,
                    "switch_tab_id": args.tab,
                })),
                args.timeout,
            )?);
        }
        ProfileLabelAction::Clear(args) => {
            print_json(request(
                "POST",
                "/profile-label",
                Some(json!({
                    "label": null,
                    "profile": args.profile,
                    "browser": args.browser,
                    "switch_tab_id": args.tab,
                })),
                args.timeout,
            )?);
        }
    }
    Ok(())
}

fn run_action_command(op: &str, args: ActionArgs, is_async: bool) -> Result<()> {
    ensure_server()?;
    if args.target.is_none() && op != "scroll" {
        return Err(anyhow!("{op}: 需要 target（CSS 选择器或 text=可见文本）"));
    }
    let mut params = json!({});
    if let Some(index) = args.index {
        params["index"] = json!(index);
    }
    if let Some(value) = args.value {
        params["value"] = json!(value);
    }
    if let Some(label) = args.label {
        params["label"] = json!(label);
    }
    if let Some(dy) = args.dy {
        params["dy"] = json!(dy);
    }
    if let Some(to) = args.to {
        params["to"] = json!(to);
    }
    let payload = json!({
        "op": op,
        "target": args.target,
        "params": params,
        "frame_id": args.frame,
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "timeout": args.timeout,
    });
    if is_async {
        print_json(submit_job(op, payload)?);
    } else {
        print_json(request(
            "POST",
            "/action",
            Some(payload),
            args.timeout + 5.0,
        )?);
    }
    Ok(())
}

/// 把 `--header "Name: Value"` 解析成 JSON 对象。
///
/// 漏冒号必须报错、不能静默跳过：`--header Authorization` 被丢掉时请求会**匿名**发出，
/// 调用方看到的是 401/403，很难联想到是参数少了冒号。
fn parse_headers(pairs: &[String]) -> Result<Value> {
    let mut map = serde_json::Map::new();
    for pair in pairs {
        let Some((key, value)) = pair.split_once(':') else {
            return Err(anyhow!(
                "--header / --request-header 需要 `Name: Value` 形式（缺少冒号）: {pair}"
            ));
        };
        map.insert(key.trim().to_string(), json!(value.trim()));
    }
    Ok(Value::Object(map))
}

/// 解析 `--body`：`{`/`[` 开头的一律按 JSON 严格解析，其余按纯文本字符串发送。
///
/// 早先是 `if let Ok(...)` 失败后落到 `json!(text)`，把坏 JSON 当字符串发出去，
/// 服务端收到字符串而非对象，表现为「mock 规则不生效」/「请求体字段全 undefined」。
/// 现在与 `run_broadcast` 的 `--args` 一致：解析失败直接报错，错误信息带行列位置。
fn parse_body(text: &str) -> Result<Value> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return serde_json::from_str::<Value>(text)
            .with_context(|| format!("--body 以 {{ 或 [ 开头但不是合法 JSON: {text}"));
    }
    Ok(json!(text))
}

fn run_broadcast(args: BroadcastArgs) -> Result<()> {
    ensure_server()?;
    let mut payload_args = match args.args_json.as_deref() {
        Some(text) => serde_json::from_str::<Value>(text)
            .context("--args 需要是 JSON 对象，例如 '{\"max_chars\":2000}'")?,
        None => json!({}),
    };
    if !payload_args.is_object() {
        return Err(anyhow!("--args 需要是 JSON 对象"));
    }
    let script = match args.file.as_ref() {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("读取脚本文件失败: {}", path.display()))?,
        ),
        None => args.script.clone(),
    };
    if let Some(script) = script {
        payload_args["script"] = json!(script);
    }
    if args.op == "exec" && payload_args.get("script").is_none() {
        return Err(anyhow!("broadcast exec: 需要脚本（--script 或 --file）"));
    }
    let payload = json!({
        "op": args.op,
        "args": payload_args,
        "browsers": args.browsers,
        "profiles": args.profiles,
        "concurrency": args.concurrency,
    });
    let value = request("POST", "/broadcast", Some(payload), args.timeout)?;
    print_json(value);
    Ok(())
}

fn run_downloads(args: DownloadsArgs) -> Result<()> {
    ensure_server()?;
    let mut payload = json!({
        "action": "list",
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
    });
    if let Some(limit) = args.limit {
        payload["limit"] = json!(limit);
    }
    if let Some(state) = args.state {
        payload["state"] = json!(state);
    }
    if let Some(id) = args.id {
        payload["id"] = json!(id);
    }
    print_json(request("POST", "/downloads", Some(payload), args.timeout)?);
    Ok(())
}

fn run_download(args: DownloadArgs) -> Result<()> {
    ensure_server()?;
    let mut payload = json!({
        "action": "start",
        "url": args.url,
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
    });
    if let Some(filename) = args.filename {
        payload["filename"] = json!(filename);
    }
    if let Some(dir) = args.dir {
        payload["dir"] = json!(dir);
    }
    print_json(request("POST", "/downloads", Some(payload), args.timeout)?);
    Ok(())
}

fn run_download_wait(args: DownloadWaitArgs) -> Result<()> {
    ensure_server()?;
    let payload = json!({
        "action": "wait",
        "id": args.id,
        "wait_timeout": args.wait_timeout,
        "interval": args.interval,
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
    });
    let value = request(
        "POST",
        "/downloads",
        Some(payload),
        args.wait_timeout + 15.0,
    )?;
    let ok = value
        .get("result")
        .and_then(|result| result.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    print_json(value);
    if !ok {
        // 不要 std::process::exit：那会绕过 run() 末尾的 AGENT_BROWSER_CLI_EXIT0 兼容开关。
        FAILURE_EXIT_CODE.store(EXIT_JOB_TIMEOUT, Ordering::Relaxed);
    }
    Ok(())
}

fn run_net_command(cmd: NetCommand) -> Result<()> {
    ensure_server()?;
    let (payload, timeout) = match cmd.action {
        NetAction::Fetch(args) => {
            let mut payload = json!({
                "action": "fetch",
                "url": args.url,
                "method": args.method,
                "response_type": args.response_type,
                "max_chars": args.max_chars,
                "frame_id": args.frame,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
            });
            let headers = parse_headers(&args.headers)?;
            if headers.as_object().is_some_and(|map| !map.is_empty()) {
                payload["headers"] = headers;
            }
            if let Some(body) = args.body {
                payload["body"] = parse_body(&body)?;
            }
            (payload, args.timeout)
        }
        NetAction::Hook(args) => {
            let mut payload = json!({
                "action": "hook",
                "capture_bodies": args.capture_bodies,
                "max_log": args.max_log,
                "persist": args.persist,
                "frame_id": args.frame,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
            });
            if !args.matches.is_empty() {
                payload["matches"] = json!(args.matches);
            }
            (payload, args.timeout)
        }
        NetAction::Log(args) => {
            let mut payload = json!({
                "action": "log",
                "limit": args.limit,
                "clear": args.clear,
                "frame_id": args.frame,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
            });
            if let Some(pattern) = args.matches {
                payload["match"] = json!(pattern);
            }
            // 只有显式打开时才下发：不带 --debug-pass 的普通读取不应关掉
            // 别人已经打开的调试开关。
            if args.debug_pass {
                payload["debug_pass"] = json!(true);
            }
            // 只有显式指定 engine 才下发；缺省（None）沿用服务端合并双源的行为。
            if let Some(engine) = args.engine {
                payload["engine"] = json!(engine);
            }
            (payload, args.timeout)
        }
        NetAction::Mock(args) => {
            let mut rule = json!({
                "match": args.pattern,
                "abort": args.abort,
            });
            if let Some(status) = args.status {
                rule["status"] = json!(status);
            } else if args.body.is_none()
                && args.request_headers.is_empty()
                && args.request_body.is_none()
                && args.response_status.is_none()
                && args.response_body.is_none()
                && !args.abort
            {
                // 什么修饰都没给：当成「返回 200 空响应」的纯 mock。
                rule["status"] = json!(200);
            }
            if let Some(method) = args.method {
                rule["method"] = json!(method);
            }
            if let Some(body) = args.body {
                rule["body"] = parse_body(&body)?;
            }
            let headers = parse_headers(&args.headers)?;
            if headers.as_object().is_some_and(|map| !map.is_empty()) {
                rule["headers"] = headers;
            }
            let request_headers = parse_headers(&args.request_headers)?;
            if request_headers
                .as_object()
                .is_some_and(|map| !map.is_empty())
            {
                rule["requestHeaders"] = request_headers;
            }
            if let Some(body) = args.request_body {
                rule["requestBody"] = parse_body(&body)?;
            }
            if args.response_status.is_some() || args.response_body.is_some() {
                let mut response = json!({});
                if let Some(status) = args.response_status {
                    response["status"] = json!(status);
                }
                if let Some(body) = args.response_body {
                    response["body"] = parse_body(&body)?;
                }
                rule["response"] = response;
            }
            let payload = json!({
                "action": "route",
                "rule": rule,
                "engine": args.engine,
                "frame_id": args.frame,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
            });
            (payload, args.timeout)
        }
        NetAction::Routes(args) => (
            json!({
                "action": "routes",
                "frame_id": args.frame,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
            }),
            args.timeout,
        ),
        NetAction::Unroute(args) => (
            json!({
                "action": "unroute",
                "match": args.pattern,
                // 不传 engine 的话服务端只会删页面内规则，cdpRules 永远清不掉。
                "engine": args.engine,
                "frame_id": args.frame,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
            }),
            args.timeout,
        ),
        NetAction::Reset(args) => (
            json!({
                "action": "reset",
                "frame_id": args.frame,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
            }),
            args.timeout,
        ),
    };
    print_json(request("POST", "/net", Some(payload), timeout + 10.0)?);
    Ok(())
}

/// 前进/后退/刷新这类会打断执行上下文的操作：脚本先返回再触发导航，
/// 避免 daemon 白等到超时。
fn run_navigation_command(script: &str, args: TabOnlyArgs) -> Result<()> {
    ensure_server()?;
    print_json(request(
        "POST",
        "/exec",
        Some(json!({
            "script": script,
            "switch_tab_id": args.tab,
            "browser": args.browser,
            "profile": args.profile,
            "no_monitor": true,
            "timeout": 5.0,
        })),
        12.0,
    )?);
    Ok(())
}

fn run_target_command(path: &str, op: &str, args: TargetCommandArgs, is_async: bool) -> Result<()> {
    ensure_server()?;
    let payload = json!({
        "target": args.target,
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "monitor": args.monitor,
        "wait_js": args.wait_js,
        "wait_timeout": args.wait_timeout,
        "wait_interval": args.wait_interval,
        "timeout": args.timeout,
        "frame_id": args.frame,
    });
    if is_async {
        print_json(submit_job(op, payload)?);
    } else {
        print_json(request(
            "POST",
            path,
            Some(payload),
            args.timeout + args.wait_timeout + 5.0,
        )?);
    }
    Ok(())
}

fn run_fill_command(args: FillCommandArgs, is_async: bool) -> Result<()> {
    ensure_server()?;
    let has_value = args.value.is_some();
    let value = args.value.unwrap_or_default();
    let payload = json!({
        "target": args.target,
        "value": value,
        "has_value": has_value,
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "append": args.append,
        "clear": args.clear,
        "monitor": args.monitor,
        "wait_js": args.wait_js,
        "wait_timeout": args.wait_timeout,
        "wait_interval": args.wait_interval,
        "timeout": args.timeout,
        "frame_id": args.frame,
    });
    if is_async {
        print_json(submit_job("fill", payload)?);
    } else {
        print_json(request(
            "POST",
            "/fill",
            Some(payload),
            args.timeout + args.wait_timeout + 5.0,
        )?);
    }
    Ok(())
}

fn run_send_keys_command(args: SendKeysCommandArgs, is_async: bool) -> Result<()> {
    ensure_server()?;
    let payload = json!({
        "keys": args.keys,
        "frame_id": args.frame,
        "target": args.target,
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "monitor": args.monitor,
        "wait_js": args.wait_js,
        "wait_timeout": args.wait_timeout,
        "wait_interval": args.wait_interval,
        "timeout": args.timeout,
    });
    if is_async {
        print_json(submit_job("send-keys", payload)?);
    } else {
        print_json(request(
            "POST",
            "/send-keys",
            Some(payload),
            args.timeout + args.wait_timeout + 5.0,
        )?);
    }
    Ok(())
}

fn run_network_command(args: NetworkCommand) -> Result<()> {
    ensure_server()?;
    match args.action {
        NetworkAction::Start(args) => print_json(request(
            "POST",
            "/network/start",
            Some(
                json!({ "switch_tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
            ),
            args.timeout,
        )?),
        NetworkAction::List(args) => print_json(request(
            "POST",
            "/network/list",
            Some(json!({
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
                "filter": args.filter,
                "limit": args.limit,
            })),
            args.timeout,
        )?),
        NetworkAction::Detail(args) => print_json(request(
            "POST",
            "/network/detail",
            Some(json!({
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
                "request_id": args.request_id,
            })),
            args.timeout,
        )?),
        NetworkAction::Clear(args) => print_json(request(
            "POST",
            "/network/clear",
            Some(
                json!({ "switch_tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
            ),
            args.timeout,
        )?),
        NetworkAction::Stop(args) => print_json(request(
            "POST",
            "/network/stop",
            Some(
                json!({ "switch_tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
            ),
            args.timeout,
        )?),
    }
    Ok(())
}

fn run_console_command(args: ConsoleCommand) -> Result<()> {
    ensure_server()?;
    match args.action {
        ConsoleAction::Start(args) => print_json(request(
            "POST",
            "/console/start",
            Some(
                json!({ "switch_tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
            ),
            args.timeout,
        )?),
        ConsoleAction::List(args) => print_json(request(
            "POST",
            "/console/list",
            Some(json!({
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
                "level": args.level,
                "limit": args.limit,
            })),
            args.timeout,
        )?),
        ConsoleAction::Clear(args) => print_json(request(
            "POST",
            "/console/clear",
            Some(
                json!({ "switch_tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
            ),
            args.timeout,
        )?),
        ConsoleAction::Stop(args) => print_json(request(
            "POST",
            "/console/stop",
            Some(
                json!({ "switch_tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
            ),
            args.timeout,
        )?),
    }
    Ok(())
}

fn run_screenshot_command(args: ScreenshotArgs, is_async: bool) -> Result<()> {
    ensure_server()?;
    let payload = json!({
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "target": args.target,
        "selector": args.selector,
        "out": args.out,
        "format": args.format,
        "quality": args.quality,
        "full_page": args.full_page,
        "timeout": args.timeout,
    });
    if is_async {
        print_json(submit_job("screenshot", payload)?);
    } else {
        print_json(request(
            "POST",
            "/screenshot",
            Some(payload),
            args.timeout + 5.0,
        )?);
    }
    Ok(())
}

fn run_save_pdf_command(args: SavePdfArgs, is_async: bool) -> Result<()> {
    ensure_server()?;
    let payload = json!({
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "out": args.out,
        "paper": args.paper,
        "landscape": args.landscape,
        "scale": args.scale,
        "print_background": !args.no_print_background,
        "timeout": args.timeout,
    });
    if is_async {
        print_json(submit_job("save-pdf", payload)?);
    } else {
        print_json(request(
            "POST",
            "/save-pdf",
            Some(payload),
            args.timeout + 5.0,
        )?);
    }
    Ok(())
}

fn submit_job(op: &str, args: Value) -> Result<Value> {
    request(
        "POST",
        "/job/submit",
        Some(json!({ "op": op, "args": args })),
        15.0,
    )
}

fn run_job_command(args: JobCommand) -> Result<()> {
    match args.action {
        JobAction::List(args) => {
            ensure_server()?;
            print_json(request(
                "GET",
                &format!("/job/list?limit={}", args.limit),
                None,
                10.0,
            )?);
        }
        JobAction::Get(args) => {
            ensure_server()?;
            print_json(request(
                "GET",
                &format!("/job/get/{}", query_escape(&args.id)),
                None,
                10.0,
            )?);
        }
        JobAction::Cancel(args) => {
            ensure_server()?;
            print_json(request(
                "POST",
                &format!("/job/cancel/{}", query_escape(&args.id)),
                Some(json!({})),
                10.0,
            )?);
        }
        JobAction::Wait(args) => wait_job(&args)?,
    }
    Ok(())
}

fn wait_job(args: &JobWaitArgs) -> Result<()> {
    ensure_server()?;
    let timeout = args.timeout.max(0.1);
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    let interval = Duration::from_secs_f64(args.interval.clamp(0.05, 5.0));
    let path = format!("/job/get/{}", query_escape(&args.id));
    loop {
        let value = request("GET", &path, None, 10.0)?;
        let status = value
            .pointer("/job/status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        if status != "running" {
            print_json(value);
            // 只有 succeeded 才算成功；cancelled 用 130（与 shell 的 SIGINT 惯例一致），
            // 其余终态（failed）按普通失败处理，交给 run() 末尾统一 exit。
            if status != "succeeded" {
                let code = if status == "cancelled" {
                    130
                } else {
                    EXIT_FAILURE
                };
                FAILURE_EXIT_CODE.store(code, Ordering::Relaxed);
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            print_json(json!({
                "ok": false,
                "error": format!("job {} 在 {}s 内未完成", args.id, timeout),
                "error_code": "job_wait_timeout",
                "job": value.get("job").cloned().unwrap_or(Value::Null)
            }));
            return Ok(());
        }
        sleep(interval);
    }
}
