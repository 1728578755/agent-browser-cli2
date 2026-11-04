use super::*;
#[allow(unused_imports)]
use clap::{Args, Parser, Subcommand};

pub(crate) const TARGET_HELP: &str = "\
目标选择（所有页面命令通用）:
  --tab <tabId>            单个标签页
  --tab 111,222            多个标签页：并发执行，一次返回
  --tab all                当前浏览器/Profile 下所有标签页
  --tab @1,@3              按 tabs 输出顺序（从 1 起），支持 @1-5
  --tab url:example.com    按 URL 子串匹配（不区分大小写）
  --tab title:订单          按标题子串匹配
  --tab group:research     按 Chrome 标签组标题匹配
  --tab window:1234567     按 Chrome 窗口 id 匹配（见 tabs --ids 的 window 字段）
  --tab current            浏览器当前激活的标签页

先检查再执行:
  atb <命令> --dry-run   只解析目标、校验参数，输出计划，不动页面

退出码:
  0 成功   1 执行失败   2 命令写错（选择器/JS/参数/目标）   3 等待超时
  4 结果未知：POST 可能已执行但响应丢失（重试前先用 --idempotency-key 或查询）

多智能体（互不影响默认标签页）:
  --agent <名字>                        显式身份；也可设 ATB_AGENT（旧名 AGENT_BROWSER_CLI_AGENT）
  身份优先级: --agent → 环境变量 → 终端会话标识 → default

收尾:
  atb cleanup            关闭本智能体 open 出来的标签页
  atb cleanup --dry-run  先看会关哪些";

#[derive(Debug, Parser)]
#[command(
    name = "atb",
    bin_name = "atb",
    version,
    about = "Agent 浏览器感知与控制 CLI：多标签并发、多智能体隔离、命令预检",
    after_help = TARGET_HELP
)]
pub struct Cli {
    /// 立即返回 job_id，页面操作在 daemon 内后台继续执行，不阻塞当前会话。
    #[arg(long = "async", global = true)]
    pub r#async: bool,
    /// 输出带缩进的 JSON（默认 compact，节省 token）。
    #[arg(long = "pretty", global = true)]
    pub pretty: bool,
    /// 只做预检和目标解析，不执行：输出将要发送的命令计划。
    #[arg(long = "dry-run", global = true)]
    pub dry_run: bool,
    /// 调用方身份：多个智能体同时使用时靠它隔离默认标签页与清理范围。
    /// 不传时按 ATB_AGENT（旧名 AGENT_BROWSER_CLI_AGENT）→ 终端会话标识 → default 依次回退。
    #[arg(long = "agent", global = true)]
    pub agent: Option<String>,
    /// 幂等键：随请求头 `x-atb-idempotency-key` 发给 daemon。动作类命令
    /// （click/fill/…）在「请求已发出但响应丢失」时不要盲目重试，带上同一个 key
    /// 重发，daemon 会在 60 秒内回放首次响应，保证动作只执行一次。
    #[arg(long = "idempotency-key", global = true)]
    pub idempotency_key: Option<String>,
    #[command(subcommand)]
    pub(crate) command: CommandKind,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CommandKind {
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
    /// 用「不弹调试横幅」的参数启动浏览器（Chrome 只在启动时读，得重启才生效）
    Launch(LaunchArgs),
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
    /// 挑战页 / 验证码：识别、等待自动放行、需要人时转人工
    Challenge(ChallengeArgs),
    /// 自动化痕迹：审计页面可被站点读取的信号，或清除本工具自身留下的痕迹
    Stealth(StealthArgs),
    /// 浏览器后退
    Back(TabOnlyArgs),
    /// 重新加载当前标签页
    Reload(TabOnlyArgs),
    /// 打开 URL 或本地文件；多个目标并发打开，--background 不打扰用户
    Open(OpenArgs),
    /// 关闭指定标签页（--tab 必填）
    Close(CloseArgs),
    /// 关闭本智能体通过 open 打开、且仍未关闭的标签页（结束任务时清理）
    Cleanup(CleanupArgs),
    /// 批量抓取：分批开页 → 逐页执行 JS → 收集结果 → 关页，失败退避重试
    Scrape(ScrapeArgs),
    /// 健康状态：daemon、扩展连接、可用标签页；退出码即结论
    Status,
    /// 查看 daemon 日志尾部
    Logs(LogsArgs),
    /// 输出全部命令的机器可读 schema（参数/类型/默认值/error_code），供 Agent 一次拿到用法
    Schema(SchemaArgs),
    /// 一次探测 daemon/扩展版本与能力门禁，避免逐条命令踩到版本坑才知道
    Capabilities,
    /// 环境自检：安装位置、端口、扩展连接逐项诊断；--fix 尝试自动修复
    Doctor(DoctorArgs),
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
    Restart(RestartArgs),
    /// 前台运行 daemon，日志打到 stdout（调试用；平时自动拉起）
    Daemon,
}

/// `atb restart` 的参数。
///
/// 旧实现是零参命令：daemon 活着但 token 不匹配时只会提示「结束占用 18767 的进程后
/// 重试」然后退出，而 restart 本来就是干这件事的——提示重试等于自锁（P0 实测反馈）。
/// `--force` 把这条手工救援路径自动化。
#[derive(Debug, Args)]
pub(crate) struct RestartArgs {
    /// daemon 在跑但不认当前 token 时，定位并强杀占用 API 端口（18767）的进程后重新拉起
    #[arg(long)]
    pub(crate) force: bool,
}

/// 「已开始调试此浏览器」横幅只有一个开关：启动参数 `--silent-debugger-extension-api`。
/// 命令行参数只在进程启动时读，所以这里做的是「用正确参数把浏览器重新拉起来」。
#[derive(Debug, Args)]
pub(crate) struct LaunchArgs {
    /// chrome | edge | chromium（默认自动探测已安装的）
    #[arg(long)]
    pub(crate) browser: Option<String>,
    /// 用户数据目录（默认取该浏览器的 Profile 根目录，登录态/扩展都在里面）
    #[arg(long = "user-data-dir", value_name = "PATH")]
    pub(crate) user_data_dir: Option<PathBuf>,
    /// 已有窗口在跑时先温和关闭再启动；不加则只报错不动手（未保存的表单会丢，所以要显式给）
    #[arg(long)]
    pub(crate) restart: bool,
    /// --restart 之后不恢复上次的标签页（默认恢复）
    #[arg(long = "no-restore")]
    pub(crate) no_restore: bool,
    /// 温和关闭无效时强杀（Chrome 在 Windows 上会忽略关闭信号；未保存的表单会丢）
    #[arg(long)]
    pub(crate) force: bool,
    /// 等扩展连上 daemon 的秒数
    #[arg(long, default_value_t = 20.0)]
    pub(crate) timeout: f64,
    /// 透传额外启动参数，可重复（例如 --arg=--remote-debugging-port=9222）
    #[arg(long = "arg", value_name = "FLAG")]
    pub(crate) extra_args: Vec<String>,
    /// 启动后要打开的地址（可多个）
    #[arg(value_name = "URL")]
    pub(crate) urls: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TabsArgs {
    #[arg(long)]
    pub(crate) browser: Option<String>,
    /// 只输出目标选择所需字段（tab_id/title/url），把 --tab 直接照着写
    #[arg(long)]
    pub(crate) ids: bool,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    /// 输出完整 URL（默认截断到 80 字符）
    #[arg(long)]
    pub(crate) full: bool,
    /// 只输出指定字段（逗号分隔，如 tab_id,title,url）。
    /// 旧实现 tabs 永远回全部字段，智能体要在长 JSON 里翻找；白名单校验，
    /// 未知字段直接报 unsupported_flag（exit 2），防止拼错字段静默返回空列。
    #[arg(long, value_name = "FIELDS")]
    pub(crate) fields: Option<String>,
    /// 最多返回 N 个标签页（在字段过滤之后截断），大浏览器里省 token
    #[arg(long, value_name = "N")]
    pub(crate) limit: Option<usize>,
    /// 只列出本 agent（ATB_AGENT）通过 open 打开的标签页。
    /// 旧实现 tabs 混着用户自己开的页面，智能体按列表清理会误伤。
    #[arg(long)]
    pub(crate) mine: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TabtreeArgs {
    #[arg(long)]
    pub(crate) tab: Option<String>,
    #[arg(long)]
    pub(crate) browser: Option<String>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    /// 输出完整 URL 和 session_key；默认使用 compact 输出以减少 token 消耗。
    #[arg(long)]
    pub(crate) full: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ScanArgs {
    #[arg(long)]
    pub(crate) tab: Option<String>,
    #[arg(long)]
    pub(crate) browser: Option<String>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long)]
    pub(crate) tabs_only: bool,
    #[arg(long)]
    pub(crate) text_only: bool,
    #[arg(long, default_value_t = 60.0)]
    pub(crate) timeout: f64,
    /// 快速模式：一次注入的紧凑文本抽取，超大页面不做整页序列化
    #[arg(long)]
    pub(crate) fast: bool,
    /// 处理节点/元素上限（默认 8000，fast 模式 80000；与服务端默认值一致）
    #[arg(long = "max-nodes")]
    pub(crate) max_nodes: Option<usize>,
}

#[derive(Debug, Args)]
#[command(after_help = r#"
示例:
  atb exec 'document.title'
  atb exec 'return document.querySelectorAll("a").length'
  atb exec --tab all 'return location.href'      # 所有标签页并发
  atb exec --file script.js                      # 从文件读
  echo 'return 1+1' | atb exec --file -           # 从 stdin 读"#)]
pub(crate) struct ExecArgs {
    #[arg(default_value = "")]
    pub(crate) script: String,
    #[arg(long)]
    pub(crate) file: Option<PathBuf>,
    #[arg(long)]
    pub(crate) tab: Option<String>,
    #[arg(long)]
    pub(crate) browser: Option<String>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long)]
    pub(crate) monitor: bool,
    #[arg(long)]
    pub(crate) wait_js: Option<String>,
    #[arg(long, default_value_t = 3.0)]
    pub(crate) wait_timeout: f64,
    #[arg(long, default_value_t = 0.1)]
    pub(crate) wait_interval: f64,
    #[arg(long, default_value_t = 60.0)]
    pub(crate) timeout: f64,
    /// 返回值上限（字符）。超出时返回截断预览，避免一条命令占满上下文
    #[arg(long = "max-bytes")]
    pub(crate) max_bytes: Option<usize>,
    /// 执行环境：main（默认）或 isolated（不受页面 CSP 影响，但看不到页面变量）
    #[arg(long, default_value = "main")]
    pub(crate) world: String,
    /// 在指定 iframe 里执行（先用 frames 查 frame_id）
    #[arg(long)]
    pub(crate) frame: Option<i64>,
    /// 在标签页的所有 frame 里执行
    #[arg(long = "all-frames")]
    pub(crate) all_frames: bool,
}

#[derive(Debug, Args)]
pub(crate) struct CookiesArgs {
    #[arg(long)]
    pub(crate) tab: Option<String>,
    #[arg(long)]
    pub(crate) browser: Option<String>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    /// 读取指定 URL 的 Cookie（默认当前页面）
    #[arg(long)]
    pub(crate) url: Option<String>,
    /// 清空 Cookie（用于反复测试挑战页 / 登录态）；配合 --domain 只清某个域
    #[arg(long)]
    pub(crate) clear: bool,
    /// 与 --clear 配合：只清这个域及其子域的 Cookie（如 example.com）
    #[arg(long)]
    pub(crate) domain: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    pub(crate) timeout: f64,
}

#[derive(Debug, Args)]
pub(crate) struct UploadArgs {
    /// 文件输入框：CSS selector 或 @e 引用
    pub(crate) target: String,
    /// 一个或多个本地文件路径
    #[arg(required = true, num_args = 1..)]
    pub(crate) files: Vec<String>,
    #[arg(long)]
    pub(crate) tab: Option<String>,
    #[arg(long)]
    pub(crate) browser: Option<String>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    pub(crate) timeout: f64,
}

#[derive(Debug, Args)]
pub(crate) struct SnapshotArgs {
    #[arg(long)]
    pub(crate) tab: Option<String>,
    #[arg(long)]
    pub(crate) browser: Option<String>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long, default_value_t = 0)]
    pub(crate) offset: usize,
    #[arg(long, default_value_t = 200)]
    pub(crate) limit: usize,
    #[arg(long)]
    pub(crate) details: bool,
    /// 强制走 DOM 快照（不依赖 CDP），CDP 被其他扩展 iframe 挡住时使用
    #[arg(long)]
    pub(crate) js: bool,
    /// 只保留可操作元素（按钮/链接/输入框等），显著缩小大树
    #[arg(long = "interactive-only")]
    pub(crate) interactive_only: bool,
    /// 返回节点上限，超出截断（默认不限制）
    #[arg(long = "max-nodes", default_value_t = 0)]
    pub(crate) max_nodes: usize,
    /// 保留无障碍树里的 generic 容器节点（默认跳过：它们既不可点也没有引用价值，
    /// 只会把 @e 编号撑大）。调试无障碍树结构时才需要
    #[arg(long = "no-skip-generic")]
    pub(crate) no_skip_generic: bool,
    #[arg(long, default_value_t = 30.0)]
    pub(crate) timeout: f64,
}

#[derive(Debug, Args)]
#[command(after_help = "\
示例:
  atb click '#submit'
  atb click @e12                             # snapshot 给的引用
  atb click --tab all '#load-more'           # 每个标签页都点一次
  atb click 'div > button.primary'")]
pub(crate) struct TargetCommandArgs {
    pub(crate) target: String,
    #[arg(long)]
    pub(crate) tab: Option<String>,
    #[arg(long)]
    pub(crate) browser: Option<String>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long)]
    pub(crate) monitor: bool,
    #[arg(long)]
    pub(crate) wait_js: Option<String>,
    #[arg(long, default_value_t = 3.0)]
    pub(crate) wait_timeout: f64,
    #[arg(long, default_value_t = 0.1)]
    pub(crate) wait_interval: f64,
    #[arg(long, default_value_t = 30.0)]
    pub(crate) timeout: f64,
    /// 在指定 iframe 内执行（先用 frames 查 frame_id）
    #[arg(long)]
    pub(crate) frame: Option<i64>,
}

#[derive(Debug, Args)]
pub(crate) struct FillCommandArgs {
    pub(crate) target: String,
    pub(crate) value: Option<String>,
    #[arg(long)]
    pub(crate) tab: Option<String>,
    #[arg(long)]
    pub(crate) browser: Option<String>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long)]
    pub(crate) append: bool,
    #[arg(long)]
    pub(crate) clear: bool,
    #[arg(long)]
    pub(crate) monitor: bool,
    #[arg(long)]
    pub(crate) wait_js: Option<String>,
    #[arg(long, default_value_t = 3.0)]
    pub(crate) wait_timeout: f64,
    #[arg(long, default_value_t = 0.1)]
    pub(crate) wait_interval: f64,
    #[arg(long, default_value_t = 30.0)]
    pub(crate) timeout: f64,
    /// 在指定 iframe 内填写
    #[arg(long)]
    pub(crate) frame: Option<i64>,
}

#[derive(Debug, Args)]
pub(crate) struct SendKeysCommandArgs {
    pub(crate) keys: String,
    #[arg(long)]
    pub(crate) target: Option<String>,
    #[arg(long)]
    pub(crate) tab: Option<String>,
    #[arg(long)]
    pub(crate) browser: Option<String>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long)]
    pub(crate) monitor: bool,
    #[arg(long)]
    pub(crate) wait_js: Option<String>,
    #[arg(long, default_value_t = 3.0)]
    pub(crate) wait_timeout: f64,
    #[arg(long, default_value_t = 0.1)]
    pub(crate) wait_interval: f64,
    #[arg(long, default_value_t = 30.0)]
    pub(crate) timeout: f64,
    /// 在指定 iframe 内发送按键
    #[arg(long)]
    pub(crate) frame: Option<i64>,
}

#[derive(Debug, Args)]
#[command(after_help = "\
示例:
  atb screenshot --full-page
  atb screenshot --selector '#chart' --out chart.png
  atb screenshot --tab all --out 'shots/{index}-{tab}.png'   # 多标签必须带占位符")]
pub(crate) struct ScreenshotArgs {
    #[arg(long)]
    pub(crate) tab: Option<String>,
    #[arg(long)]
    pub(crate) browser: Option<String>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long)]
    pub(crate) target: Option<String>,
    #[arg(long)]
    pub(crate) selector: Option<String>,
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    #[arg(long, default_value = "png")]
    pub(crate) format: String,
    #[arg(long)]
    pub(crate) quality: Option<u8>,
    #[arg(long)]
    pub(crate) full_page: bool,
    /// 整页截图的高度上限（设备像素）。仅在「页面禁止调试器注入、走拼图回落」时生效，
    /// 默认 20000；超出会截断并在结果里标 truncated。
    #[arg(long)]
    pub(crate) max_height: Option<u32>,
    #[arg(long, default_value_t = 30.0)]
    pub(crate) timeout: f64,
}

#[derive(Debug, Args)]
pub(crate) struct SavePdfArgs {
    #[arg(long)]
    pub(crate) tab: Option<String>,
    #[arg(long)]
    pub(crate) browser: Option<String>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    #[arg(long, default_value = "a4")]
    pub(crate) paper: String,
    #[arg(long)]
    pub(crate) landscape: bool,
    #[arg(long, default_value_t = 1.0)]
    pub(crate) scale: f64,
    #[arg(long = "no-print-background")]
    pub(crate) no_print_background: bool,
    /// 图片型 PDF 回落时整页拼图的高度上限（设备像素），默认 20000。
    #[arg(long)]
    pub(crate) max_height: Option<u32>,
    #[arg(long, default_value_t = 30.0)]
    pub(crate) timeout: f64,
}
