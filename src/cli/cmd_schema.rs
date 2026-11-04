use super::*;

// `atb schema`：把全部子命令的参数（名/类型/默认值/可选值）与主要 error_code
// 输出成机器可读 JSON，让 Agent 一次拿到用法，不必逐条 `--help` / 试错。
//
// 实现用静态表而不是给 `CommandKind` 加方法：表就放在这一个文件里，
// 新增/改参数时对照 `args.rs` / `args_more.rs` / `args_net.rs` 逐个抄即可。
// 结构约定（稳定契约，不要随意改键名）：
// `{"ok":true,"command_count":N,"commands":{"open":{"args":{...},"error_codes":[...]}}}`
// 参数项形状：`{"type":"string|number|bool|path|int","default":...,"optional":true,"variadic":true,"enum":[...]}`
// flag 的键名带 `--` 前缀；位置参数不带。

/// 必填位置参数。
fn required(ty: &str) -> Value {
    json!({ "type": ty, "required": true })
}

/// 可选位置参数。
fn optional(ty: &str) -> Value {
    json!({ "type": ty, "optional": true })
}

/// 可变长位置参数（可给多个）。
fn variadic(ty: &str, required: bool) -> Value {
    json!({ "type": format!("{ty}+"), "variadic": true, "required": required })
}

/// 布尔 flag（开关，缺省 false）。
fn flag() -> Value {
    json!({ "type": "bool", "default": false })
}

/// 有默认值的标量参数。
fn with_default(ty: &str, default: Value) -> Value {
    json!({ "type": ty, "default": default })
}

/// 可选参数（无默认值，不给就用 daemon 侧行为）。
fn opt(ty: &str) -> Value {
    json!({ "type": ty, "optional": true })
}

/// 枚举参数。
fn with_enum(ty: &str, default: &str, values: &[&str]) -> Value {
    json!({ "type": ty, "default": default, "enum": values })
}

/// 把 b 合并进 a（浅合并，键冲突以 b 为准），返回合并结果。
/// 旧签名是 `(&mut Value, Value)` 且无返回值——调用方全都把它当表达式用，
/// 拿到的是 `()`，编译期就是 26 个 E0308；改成拿所有权、返回 Value。
fn merge_args(base: Value, extra: Value) -> Value {
    let mut base = base;
    if let (Some(a), Some(b)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in b {
            a.insert(key.clone(), value.clone());
        }
    }
    base
}

/// 页面命令通用的三件套 + 超时（--tab/--browser/--profile/--timeout）。
fn page_args(timeout: f64) -> Value {
    json!({
        "--tab": opt("string"),
        "--browser": opt("string"),
        "--profile": opt("string"),
        "--timeout": with_default("number", json!(timeout)),
    })
}

/// 单条命令的 schema 条目：名称、说明、参数、主要 error_code。
struct CmdSchema {
    name: &'static str,
    description: &'static str,
    args: Value,
    error_codes: &'static [&'static str],
}

/// 常用错误码集中在此，保持与 daemon / CLI 预检实际返回的一致口径
/// （对照 `exit.rs::is_usage_error_code` 与 daemon 各端点的 error_code 字段）。
fn schema_entries() -> Vec<CmdSchema> {
    let mut list: Vec<CmdSchema> = Vec::new();
    let mut push = |name: &'static str,
                    description: &'static str,
                    mut args: Value,
                    error_codes: &'static [&'static str]| {
        // 参数表统一带一个空对象兜底：没有参数的命令输出 "args": {}。
        if args.as_object().is_none() {
            args = json!({});
        }
        list.push(CmdSchema {
            name,
            description,
            args,
            error_codes,
        });
    };

    // ---- 标签页与目标 ----
    push(
        "tabs",
        "列出全部标签页（含 browser_id/profile_id/tab_id）",
        json!({
            "--browser": opt("string"),
            "--ids": flag(),
            "--profile": opt("string"),
            "--full": flag(),
            // --fields / --limit / --mine：精简输出三件套（未知字段 exit 2）。
            "--fields": opt("string"),
            "--limit": with_default("int", json!(0)),
            "--mine": flag(),
        }),
        &["no_tabs", "invalid_api_token", "unsupported_flag"],
    );
    push(
        "tabtree",
        "按 browser → profile → 标签页树形展示归属",
        json!({
            "--tab": opt("string"),
            "--browser": opt("string"),
            "--profile": opt("string"),
            "--full": flag(),
        }),
        &[],
    );
    push(
        "lookup",
        "按 tab/browser/profile 反查归属",
        json!({
            "<subcommand>": { "type": "enum", "required": true, "enum": ["tab", "browser", "profile"] },
            "<arg>": required("string"),
            // 由反射测试补齐：`lookup tab` 支持按 browser/profile 缩小范围（LookupTabArgs），
            // 旧表漏了这两个 flag，Agent 会以为 lookup 不能带目标限定。
            "--browser": opt("string"),
            "--profile": opt("string"),
        }),
        &["target_not_found"],
    );
    push(
        "profile-label",
        "给 Chrome Profile 起可读别名",
        json!({
            "<subcommand>": { "type": "enum", "required": true, "enum": ["set", "clear"] },
            "<label>": optional("string"),
            "--profile": opt("string"),
            "--browser": opt("string"),
            "--tab": opt("string"),
            "--timeout": with_default("number", json!(30.0)),
        }),
        &["target_not_found"],
    );
    push("browsers", "列出已连接的浏览器实例", json!({}), &[]);

    // ---- 页面读取 ----
    push(
        "scan",
        "内容感知：取正文、列表、页面文本",
        merge_args(
            json!({
                "--tabs-only": flag(),
                "--text-only": flag(),
                "--fast": flag(),
                "--max-nodes": opt("number"),
            }),
            page_args(60.0),
        ),
        &["no_tabs", "target_not_found", "target_out_of_range"],
    );
    push(
        "read",
        "结构化读页面：标题、正文、表单、计数",
        merge_args(
            json!({
                "--selector": opt("string"),
                "--max-chars": with_default("number", json!(20000)),
                "--links": flag(),
                "--no-forms": flag(),
                "--include-hidden": flag(),
                "--budget-ms": opt("number"),
                "--max-nodes": opt("number"),
                // daemon 侧 `ReadRequest.inner_text_limit`（DOM 节点数阈值，非字符数）。
                // 不传或传 0 → 页面用默认 30000；正整数由 daemon clamp 到 1000..=500000。
                // 字符量看 --max-chars（daemon clamp 1000..=400000）；本项越小越偏向
                // 近似可见文本（visible-approx：更快但可能少内容），是精度/速度旋钮。
                "--inner-text-limit": opt("int"),
                "--frame": opt("int"),
            }),
            page_args(30.0),
        ),
        &["no_tabs", "target_not_found", "target_out_of_range"],
    );
    push(
        "find",
        "按 CSS / 可见文本 / XPath 找元素",
        merge_args(
            json!({
                "<selector>": optional("string"),
                "--text": opt("string"),
                "--xpath": opt("string"),
                "--limit": with_default("number", json!(50)),
                "--exact": flag(),
                "--include-hidden": flag(),
                "--frame": opt("int"),
            }),
            page_args(20.0),
        ),
        &["no_tabs", "target_not_found", "target_out_of_range"],
    );
    push(
        "snapshot",
        "生成带 @e 引用的可点击树",
        merge_args(
            json!({
                "--offset": with_default("number", json!(0)),
                "--limit": with_default("number", json!(200)),
                "--details": flag(),
                "--js": flag(),
                "--interactive-only": flag(),
                "--max-nodes": with_default("number", json!(0)),
                // 保留无障碍树里的 generic 容器节点（默认跳过）。
                "--no-skip-generic": flag(),
            }),
            page_args(30.0),
        ),
        &["no_tabs", "target_not_found", "target_out_of_range"],
    );
    push(
        "frames",
        "列出页面内 iframe",
        page_args(20.0),
        &["no_tabs", "target_not_found"],
    );

    // ---- 页面执行 ----
    let wait_common = json!({
        "--monitor": flag(),
        "--wait-js": opt("string"),
        "--wait-timeout": with_default("number", json!(3.0)),
        "--wait-interval": with_default("number", json!(0.1)),
    });
    push(
        "exec",
        "执行任意 JS 并回传结果",
        merge_args(
            json!({
                "<script>": with_default("string", json!("")),
                "--file": opt("path"),
                "--max-bytes": opt("number"),
                "--world": with_default("string", json!("main")),
                "--frame": opt("int"),
                "--all-frames": flag(),
            }),
            merge_args(wait_common.clone(), page_args(60.0)),
        ),
        &[
            "no_tabs",
            "target_not_found",
            "target_out_of_range",
            "extension_outdated",
            "invalid_world",
            "invalid_frame",
            // 位置参数脚本与 --file 同时给。
            "unsupported_combination",
        ],
    );
    push(
        "click",
        "点击元素（CSS 选择器或 @e 引用）",
        merge_args(
            json!({
                "<target>": required("string"),
                "--frame": opt("int"),
            }),
            merge_args(wait_common.clone(), page_args(30.0)),
        ),
        &[
            "no_tabs",
            "target_not_found",
            "target_out_of_range",
            "extension_outdated",
        ],
    );
    push(
        "mouse-click",
        "按坐标或元素发真实鼠标事件",
        merge_args(
            json!({
                "<target>": required("string"),
                "--frame": opt("int"),
            }),
            merge_args(wait_common.clone(), page_args(30.0)),
        ),
        &[
            "no_tabs",
            "target_not_found",
            "target_out_of_range",
            "extension_outdated",
        ],
    );
    push(
        "fill",
        "填写输入框（默认覆盖，--append 追加，--clear 清空）",
        merge_args(
            json!({
                "<target>": required("string"),
                "<value>": optional("string"),
                "--append": flag(),
                "--clear": flag(),
                "--frame": opt("int"),
            }),
            merge_args(wait_common.clone(), page_args(30.0)),
        ),
        &[
            "no_tabs",
            "target_not_found",
            "target_out_of_range",
            "extension_outdated",
        ],
    );
    push(
        "send-keys",
        "向焦点元素或 --target 元素发送按键",
        merge_args(
            json!({
                "<keys>": required("string"),
                "--target": opt("string"),
                "--frame": opt("int"),
            }),
            merge_args(wait_common.clone(), page_args(30.0)),
        ),
        &[
            "no_tabs",
            "target_not_found",
            "target_out_of_range",
            "extension_outdated",
        ],
    );
    push(
        "upload",
        "给文件输入框设置本地文件",
        merge_args(
            json!({
                "<target>": required("string"),
                "<files>": variadic("path", true),
            }),
            page_args(30.0),
        ),
        &["file_not_found", "no_tabs", "target_not_found"],
    );
    push(
        "inject",
        "注入 JS；--persist 命名后每次导航自动重放",
        merge_args(
            json!({
                "<script>": optional("string"),
                "--file": opt("path"),
                "--persist": opt("string"),
                "--remove": opt("string"),
                "--list": flag(),
                "--world": with_default("string", json!("main")),
                "--match": variadic("string", false),
                "--frame": opt("int"),
                "--all-frames": flag(),
            }),
            page_args(30.0),
        ),
        &["no_tabs", "target_not_found", "invalid_argument"],
    );

    // ---- 动作类（ActionArgs）----
    let action_args = merge_args(
        json!({
            "<target>": optional("string"),
            "--index": opt("int"),
            "--value": opt("string"),
            "--label": opt("string"),
            "--dy": opt("number"),
            "--to": opt("string"),
            "--frame": opt("int"),
        }),
        page_args(15.0),
    );
    for (name, description) in [
        ("hover", "悬停元素"),
        ("select", "选择下拉框选项（--value 或 --label）"),
        ("check", "勾选或取消勾选 checkbox / radio"),
        ("scroll", "滚动页面（--to top/bottom 或 --dy 像素）"),
    ] {
        push(
            name,
            description,
            action_args.clone(),
            &[
                "no_tabs",
                "target_not_found",
                "target_out_of_range",
                "extension_outdated",
            ],
        );
    }

    // ---- 等待 / 挑战 / 反检测 ----
    push(
        "wait",
        "等待元素、文本或 JS 条件成立（默认 10s）",
        merge_args(
            json!({
                "--selector": opt("string"),
                "--text": opt("string"),
                "--js": opt("string"),
                "--visible": flag(),
                "--interval": with_default("number", json!(0.1)),
                "--frame": opt("int"),
            }),
            page_args(10.0),
        ),
        &["no_tabs", "target_not_found", "wait_timeout"],
    );
    push(
        "challenge",
        "挑战页/验证码：识别、等待自动放行、需要人时转人工",
        merge_args(
            json!({
                "<action>": with_enum("string", "status", &["status", "wait", "solve", "auto", "click", "widget", "focus"]),
                "--timeout": opt("number"),
                "--interval": with_default("number", json!(0.25)),
                "--focus-after": with_default("number", json!(8.0)),
                "--no-focus": flag(),
                "--reload-after": with_default("number", json!(0.0)),
                "--at": opt("string"),
                // 挑战页识别后的确认等待（毫秒，0 = 不等待），对应服务端 confirm_ms。
                "--confirm-ms": opt("number"),
                "--frame": opt("int"),
            }),
            page_args(15.0),
        ),
        &[
            "no_tabs",
            "target_not_found",
            "challenge_blocked",
            "wait_timeout",
        ],
    );
    push(
        "stealth",
        "自动化痕迹：审计或清除本工具留下的痕迹",
        merge_args(
            json!({
                "<action>": with_enum("string", "audit", &["audit", "scrub"]),
                "--frame": opt("int"),
            }),
            page_args(15.0),
        ),
        &["no_tabs", "target_not_found"],
    );

    // ---- 标签页生命周期 ----
    push(
        "open",
        "打开 URL 或本地文件；多个目标并发打开",
        json!({
            "<targets>": variadic("string", true),
            "--background": flag(),
            "--window": flag(),
            "--focus": flag(),
            "--tab": opt("string"),
            "--browser": opt("string"),
            "--profile": opt("string"),
            "--session": opt("string"),
            "--group-title": opt("string"),
            "--timeout": with_default("number", json!(10.0)),
            "--challenge": with_enum("string", "auto", &["auto", "wait", "off"]),
            "--challenge-timeout": with_default("number", json!(15.0)),
            // --wait-for / --warm 与 --group-title 同批加入（open 体验改进），
            // schema 表必须同步，否则 Agent 会漏用这两个 flag。
            "--wait-for": opt("string"),
            "--warm": flag(),
        }),
        &[
            "invalid_open_target",
            "invalid_timeout",
            // --challenge-timeout 越界/非法。
            "invalid_argument",
            "no_tabs",
            "extension_outdated",
            "challenge_blocked",
        ],
    );
    push(
        "close",
        "关闭指定标签页（--tab 必填；id / id 列表 / group:<标题>，group 需配 --yes 显式确认；all/url:/title: 仍拒绝）",
        json!({
            "--tab": required("string"),
            "--browser": opt("string"),
            "--profile": opt("string"),
            "--timeout": with_default("number", json!(30.0)),
            // group:<标题> 关闭是误伤高危动作，必须 --yes 二次确认。
            "--yes": flag(),
        }),
        &[
            "target_not_found",
            "target_out_of_range",
            "unsupported_combination",
        ],
    );
    push(
        "cleanup",
        "关闭本智能体通过 open 打开的标签页",
        json!({
            "--all-agents": flag(),
            "--dry-run": flag(),
        }),
        &[],
    );
    push(
        "back",
        "浏览器后退",
        page_args(30.0),
        &["no_tabs", "target_not_found"],
    );
    push(
        "reload",
        "重新加载当前标签页",
        page_args(30.0),
        &["no_tabs", "target_not_found"],
    );

    // ---- 采集 / 文件 ----
    push(
        "screenshot",
        "截图当前页面",
        merge_args(
            json!({
                "--target": opt("string"),
                "--selector": opt("string"),
                "--out": opt("path"),
                "--format": with_enum("string", "png", &["png", "jpeg", "webp"]),
                "--quality": opt("int"),
                "--full-page": flag(),
                "--max-height": opt("int"),
            }),
            page_args(30.0),
        ),
        &["output_path_collision", "no_tabs", "target_not_found"],
    );
    push(
        "save-pdf",
        "把当前页面另存为 PDF",
        merge_args(
            json!({
                "--out": opt("path"),
                "--paper": with_default("string", json!("a4")),
                "--landscape": flag(),
                "--scale": with_default("number", json!(1.0)),
                "--no-print-background": flag(),
                "--max-height": opt("int"),
            }),
            page_args(30.0),
        ),
        &["output_path_collision", "no_tabs", "target_not_found"],
    );
    push(
        "cookies",
        "读取/清空当前页面 Cookie",
        merge_args(
            json!({
                "--url": opt("string"),
                "--clear": flag(),
                "--domain": opt("string"),
            }),
            page_args(30.0),
        ),
        &["no_tabs", "target_not_found"],
    );

    // ---- 网络 / 下载 ----
    push(
        "network",
        "抓取页面网络请求",
        json!({
            "<subcommand>": { "type": "enum", "required": true, "enum": ["start", "list", "detail", "clear", "stop"] },
            // 由反射测试补齐：这些 flag 分散在 network 的各子命令上
            // （start/clear/stop 用 TabOnlyArgs，list 另有 --filter/--limit，detail 取 request_id），
            // 旧表只写了 <subcommand>，Agent 会以为 network 不支持目标限定与过滤。
            "--filter": opt("string"),
            "--limit": with_default("number", json!(100)),
            "--tab": opt("string"),
            "--browser": opt("string"),
            "--profile": opt("string"),
            "--timeout": with_default("number", json!(30.0)),
        }),
        &["no_tabs", "target_not_found"],
    );
    push(
        "console",
        "抓取页面 console 输出",
        json!({
            "<subcommand>": { "type": "enum", "required": true, "enum": ["start", "list", "clear", "stop"] },
            // 由反射测试补齐：同上，start/clear/stop 用 TabOnlyArgs，
            // list 另有 --level/--limit，旧表只写了 <subcommand>。
            "--level": opt("string"),
            "--limit": with_default("number", json!(100)),
            "--tab": opt("string"),
            "--browser": opt("string"),
            "--profile": opt("string"),
            "--timeout": with_default("number", json!(30.0)),
        }),
        &["no_tabs", "target_not_found"],
    );
    push(
        "net",
        "网络拦截：fetch / hook / mock / log",
        json!({
            "<subcommand>": { "type": "enum", "required": true, "enum": ["fetch", "hook", "log", "mock", "routes", "unroute", "reset"] },
            // 只对 net fetch 生效的两个页面侧开关（其余子命令忽略）。
            "--credentials": with_enum("string", "include", &["include", "omit", "same-origin"]),
            "--no-parse-json": flag(),
            // 由反射测试补齐：net 是**扁平表项 + 子命令**，下面这些 flag 分散在
            // fetch / hook / log / mock / unroute 上（另一半 tab/browser/profile/frame
            // 是各子命令共用的目标限定）。旧表只列了 fetch 的两个页面侧开关，
            // Agent 照表用 --method / --header / --persist 时根本不知道它们存在。
            // fetch / mock
            "--method": opt("string"),
            "--header": variadic("string", false),
            "--max-chars": with_default("number", json!(200000)),
            "--response-type": with_enum("string", "text", &["text", "base64"]),
            // mock
            "--status": opt("int"),
            "--abort": flag(),
            "--request-header": variadic("string", false),
            "--request-body": opt("string"),
            "--response-status": opt("int"),
            "--response-body": opt("string"),
            // hook
            "--capture-bodies": flag(),
            "--persist": flag(),
            "--max-log": opt("int"),
            // log
            "--debug-pass": flag(),
            "--clear": flag(),
            // log / mock / unroute（mock 默认 page）
            "--engine": with_enum("string", "page", &["page", "cdp", "both"]),
            // fetch / log / routes / reset / hook / mock 共用
            "--body": opt("string"),
            "--match": variadic("string", false),
            "--limit": with_default("number", json!(50)),
            "--tab": opt("string"),
            "--browser": opt("string"),
            "--profile": opt("string"),
            "--frame": opt("int"),
            "--timeout": with_default("number", json!(30.0)),
        }),
        &["no_tabs", "target_not_found", "invalid_argument"],
    );
    push(
        "downloads",
        "查看下载列表",
        merge_args(
            json!({
                "--limit": opt("number"),
                "--state": opt("string"),
                "--id": opt("int"),
            }),
            page_args(20.0),
        ),
        &["no_tabs", "target_not_found"],
    );
    push(
        "download",
        "发起下载并保存到本地",
        merge_args(
            json!({
                "<url>": required("string"),
                "--filename": opt("string"),
                "--dir": opt("string"),
            }),
            page_args(30.0),
        ),
        &["no_tabs", "target_not_found"],
    );
    push(
        "download-wait",
        "等待下载完成",
        merge_args(
            json!({
                "--id": opt("int"),
                "--timeout": with_default("number", json!(120.0)),
                "--interval": with_default("number", json!(0.5)),
            }),
            json!({
                "--tab": opt("string"),
                "--browser": opt("string"),
                "--profile": opt("string"),
            }),
        ),
        &["no_tabs", "wait_timeout"],
    );

    // ---- 编排 ----
    push(
        "job",
        "管理 --async 提交的后台任务",
        json!({
            "<subcommand>": { "type": "enum", "required": true, "enum": ["list", "get", "wait", "cancel"] },
            "<id>": optional("string"),
            "--timeout": opt("number"),
            "--limit": opt("number"),
            "--interval": opt("number"),
        }),
        &["job_not_found", "wait_timeout", "invalid_argument"],
    );
    push(
        "run",
        "按步骤文件串行执行多步操作",
        json!({
            "<file>": required("path"),
            "--continue-on-error": flag(),
        }),
        // --continue-on-error 与文件里的 stop_on_error 冲突；invalid_steps = 文件格式错。
        &["file_not_found", "invalid_steps", "unsupported_combination"],
    );
    push(
        "batch",
        "按步骤文件并发执行（跨标签）",
        json!({
            "<file>": required("path"),
            "--concurrency": with_default("number", json!(4)),
        }),
        &["file_not_found"],
    );
    push(
        "broadcast",
        "对所有或指定 browser/profile 并发执行同一操作",
        json!({
            "<op>": required("string"),
            "--script": opt("string"),
            "--file": opt("path"),
            "--args": opt("string"),
            "--browser": variadic("string", false),
            "--profile": variadic("string", false),
            "--concurrency": opt("number"),
            "--timeout": with_default("number", json!(180.0)),
        }),
        &["invalid_argument", "no_tabs"],
    );

    // ---- 浏览器进程 ----
    push(
        "launch",
        "用「不弹调试横幅」的参数启动浏览器",
        json!({
            "<urls>": variadic("string", false),
            "--browser": opt("string"),
            "--user-data-dir": opt("path"),
            "--restart": flag(),
            "--no-restore": flag(),
            "--force": flag(),
            "--timeout": with_default("number", json!(20.0)),
            "--arg": variadic("string", false),
        }),
        &["file_not_found", "invalid_argument"],
    );

    // ---- daemon / 诊断 ----
    push("status", "健康状态（退出码即结论）", json!({}), &[]);
    push(
        "logs",
        "查看 daemon 日志尾部",
        json!({ "--tail": with_default("number", json!(100)) }),
        &["file_not_found"],
    );
    push(
        "schema",
        "输出全部命令的机器可读 schema",
        json!({ "--json": with_default("bool", json!(true)) }),
        &[],
    );
    push(
        "capabilities",
        "一次探测 daemon/扩展版本与能力门禁",
        json!({}),
        &["daemon_not_running", "daemon_token_mismatch"],
    );
    push(
        "doctor",
        "环境自检；--fix 尝试自动修复",
        json!({
            "--fix": flag(),
            "--force": flag(),
        }),
        &[],
    );
    push(
        "install-skill",
        "把随包的 SKILL.md 安装到 ~/.agents/skills",
        json!({
            "--dry-run": flag(),
            "--yes": flag(),
        }),
        &["file_not_found"],
    );
    push(
        "set-extension-port",
        "修改扩展 WebSocket 端口并重启 daemon",
        json!({ "<port>": required("int") }),
        &["invalid_argument"],
    );
    push(
        "trust-extension",
        "管理 WebSocket 信任配置",
        json!({
            "<extension_id>": optional("string"),
            "--remove": flag(),
            "--list": flag(),
            "--clear": flag(),
            "--token": opt("string"),
            "--clear-token": flag(),
        }),
        &["invalid_argument"],
    );
    push("stop", "停止后台 daemon", json!({}), &[]);
    push(
        "restart",
        "重启 daemon",
        // 由反射测试补齐：`restart --force` 是唯一的手工救援路径（旧 daemon 占端口
        // 且 token 失配时），旧表写成空对象，Agent 根本发现不了它。
        json!({ "--force": flag() }),
        &[],
    );
    push("daemon", "前台运行 daemon（调试用）", json!({}), &[]);

    // ---- 批量抓取 ----
    push(
        "scrape",
        "批量抓取：分批开页 → 执行 JS → 收集 → 关页，失败退避重试",
        json!({
            "--urls": required("string"),
            "--js-file": opt("path"),
            "--out": required("path"),
            "--concurrency": with_default("number", json!(4)),
            "--retry": with_default("number", json!(3)),
            "--batch": with_default("number", json!(4)),
            "--timeout": with_default("number", json!(30.0)),
            "--group-title": opt("string"),
            // 默认每个 URL 只写一条最终记录；逐次写法只在诊断时用。
            "--record-attempts": flag(),
        }),
        &[
            "file_not_found",
            "no_tabs",
            "invalid_argument",
            "outcome_unknown",
        ],
    );

    list
}

pub(crate) fn cmd_schema(args: SchemaArgs, is_async: bool) -> Result<()> {
    reject_async_if_set("schema", is_async)?;
    // `--json` 目前是缺省行为：输出本来就是 JSON，这个 flag 只是显式声明，
    // 将来若加 `--markdown` 之类的别的格式，Agent 侧用法也不用变。
    let _ = args.json;
    let entries = schema_entries();
    let mut commands = serde_json::Map::new();
    for entry in entries {
        commands.insert(
            entry.name.to_string(),
            json!({
                "description": entry.description,
                "args": entry.args,
                "error_codes": entry.error_codes,
            }),
        );
    }
    let count = commands.len();
    print_json(json!({
        "ok": true,
        "command_count": count,
        "commands": commands,
    }));
    Ok(())
}

// ---------------------------------------------------------------------------
// 反射一致性测试
// ---------------------------------------------------------------------------
//
// 上面那张静态表是 `atb schema --json` 的唯一数据源，而 Agent 恰恰只信这张表：
// 表里漏一个 flag，Agent 就永远不知道它可用；表里多留一个已删除的 flag，Agent
// 照表调用会拿到 clap 的 invalid_argument（exit 2）。静态表与 clap 定义是两份
// 独立维护的真相，天然会漂移，所以这里用 `Cli::command()` 把真实命令树反射出来，
// 与表做**双向**比对，漂移即测试失败。
//
// 比对口径：
// 1. 顶层命令名集合必须完全相等（clap 自带的 `help` 除外）；
// 2. 每个命令的长参数（`--flag`）集合必须完全相等，双向都查；
// 3. 全局参数（`Cli` 上 `global = true` 的那几个 + clap 自带的 help/version）
//    对所有子命令都生效，逐条列进每个表项没有意义，双向排除；
// 4. 带子命令的表项（job / net / network / console / lookup / profile-label…）
//    按「整棵子树的并集」比对——表是扁平的，子命令参数只能摊进父表项。
#[cfg(test)]
mod schema_reflection_tests {
    use super::*;
    use clap::CommandFactory;
    use std::collections::{BTreeMap, BTreeSet};

    /// 全局参数：`Cli` 上的 `--async/--pretty/--dry-run/--agent/--idempotency-key`
    /// 加 clap 自动生成的 `--help/--version`。它们不属于任何单个命令的契约。
    const GLOBAL_FLAGS: &[&str] = &[
        "pretty",
        "dry-run",
        "agent",
        "async",
        "idempotency-key",
        "help",
        "version",
    ];

    fn is_global(long: &str) -> bool {
        GLOBAL_FLAGS.contains(&long)
    }

    /// 表项里的长参数集合。键名约定带 `--` 前缀；`<subcommand>` / `<target>`
    /// 这类位置参数不是 flag，不参与比对。
    fn table_flags(args: &serde_json::Value) -> BTreeSet<String> {
        args.as_object()
            .map(|map| {
                map.keys()
                    .filter(|key| key.starts_with("--"))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// clap 命令树里全部长参数（含所有后代子命令），不排除全局参数。
    fn clap_flags(command: &clap::Command) -> BTreeSet<String> {
        fn walk(command: &clap::Command, out: &mut BTreeSet<String>) {
            for arg in command.get_arguments() {
                if let Some(long) = arg.get_long() {
                    out.insert(format!("--{long}"));
                }
            }
            for sub in command.get_subcommands() {
                walk(sub, out);
            }
        }
        let mut out = BTreeSet::new();
        walk(command, &mut out);
        out
    }

    /// `--name` 是否属于全局参数白名单。
    fn is_global_flag(flag: &str) -> bool {
        flag.strip_prefix("--").map(is_global).unwrap_or(false)
    }

    fn joined(set: &BTreeSet<String>) -> String {
        if set.is_empty() {
            "（无）".to_string()
        } else {
            set.iter().cloned().collect::<Vec<_>>().join(", ")
        }
    }

    #[test]
    fn schema_matches_clap_definitions() {
        let cli = Cli::command();
        let entries = schema_entries();

        // ---- 1) 顶层命令集合 ----
        let clap_names: BTreeSet<String> = cli
            .get_subcommands()
            .map(|sub| sub.get_name().to_string())
            .filter(|name| name != "help")
            .collect();
        let table_names: BTreeSet<String> =
            entries.iter().map(|entry| entry.name.to_string()).collect();

        // 表项重名会让 `atb schema` 的后一项静默覆盖前一项（command_count 也会少算），
        // 所以顺手断言「表项数 == 命令名去重数」。
        assert_eq!(
            entries.len(),
            table_names.len(),
            "schema 表里有重复命令：{} 条表项但只有 {} 个不同命令：{:?}",
            entries.len(),
            table_names.len(),
            entries.iter().map(|entry| entry.name).collect::<Vec<_>>()
        );

        let only_in_table: Vec<&str> = table_names
            .difference(&clap_names)
            .map(String::as_str)
            .collect();
        let only_in_clap: Vec<&str> = clap_names
            .difference(&table_names)
            .map(String::as_str)
            .collect();
        assert!(
            only_in_table.is_empty() && only_in_clap.is_empty(),
            "顶层命令集合与 clap 定义不一致：\n  表里多出（clap 没这个命令）：{:?}\n  表里缺少（clap 有但 schema 没列）：{:?}",
            only_in_table,
            only_in_clap
        );

        // ---- 2) 每个命令的长参数集合（双向） ----
        let clap_by_name: BTreeMap<String, BTreeSet<String>> = cli
            .get_subcommands()
            .filter(|sub| sub.get_name() != "help")
            .map(|sub| (sub.get_name().to_string(), clap_flags(sub)))
            .collect();

        let mut problems: Vec<String> = Vec::new();
        for entry in &entries {
            // 命令本身缺失已在第 1 步报过，这里只管参数。
            let Some(clap_all) = clap_by_name.get(entry.name) else {
                continue;
            };
            let actual = table_flags(&entry.args);
            // 表里允许写全局参数（如 cleanup 的 --dry-run 既是全局也真有本地定义）。
            let mut acceptable = clap_all.clone();
            for global in GLOBAL_FLAGS {
                acceptable.insert(format!("--{global}"));
            }
            // 必须逐条列进表里的是「非全局参数」。
            let expected: BTreeSet<String> = clap_all
                .iter()
                .filter(|flag| !is_global_flag(flag.as_str()))
                .cloned()
                .collect();
            for flag in actual.difference(&acceptable) {
                problems.push(format!(
                    "命令 `{}`：表里多出参数 {flag}（clap 里没有这个长参数，Agent 照表调用必然 exit 2）；表里是 {}，clap 是 {}",
                    entry.name,
                    joined(&actual),
                    joined(&expected)
                ));
            }
            for flag in expected.difference(&actual) {
                problems.push(format!(
                    "命令 `{}`：表里缺少参数 {flag}（clap 有，schema 漏了）；表里是 {}，clap 是 {}",
                    entry.name,
                    joined(&actual),
                    joined(&expected)
                ));
            }
        }
        assert!(
            problems.is_empty(),
            "schema 表与 clap 定义漂移，共 {} 处：\n{}",
            problems.len(),
            problems.join("\n")
        );
    }
}
