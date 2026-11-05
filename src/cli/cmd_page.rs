use super::*;
#[allow(unused_imports)]
use clap::{Args, Parser, Subcommand};

/// 可选数值字段为 `None` 时不要下发 `null`。
///
/// `preflight` 把数值字段的 `null` 当成「用户写了 NaN/Inf，`json!` 序列化成了 null」
/// 并报 `invalid_number`（的检测手段）；但 `Option<usize>` 的 `None` 同样
/// 序列化成 null —— 那是「没传、用服务端默认」，不是错误。两者在 JSON 里无法区分，
/// 所以在构造 payload 时就把 None 摘掉，让 null 只剩「显式传入非有限值」一种来源。
fn omit_absent_numbers(payload: &mut Value, keys: &[&str]) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    for key in keys {
        if map.get(*key).is_some_and(Value::is_null) {
            map.remove(*key);
        }
    }
}

/// CLI 侧统一的 HTTP 超时预算：业务超时 + `ATB_HTTP_SLACK_SEC`（默认 10s）。
///
/// 以前每个命令各自写 `args.timeout` / `+5.0` / `+10.0`：同样是 30s 的业务超时，
/// cookies 只给 30s、snapshot 30s，daemon 还在等待时 CLI 已经断开并报「超时」
/// 。所有直接用 `args.timeout` 的客户端超时统一走这里。
pub(crate) fn http_budget(timeout: f64) -> f64 {
    if !timeout.is_finite() {
        return 30.0;
    }
    timeout.max(0.0) + crate::settings::http_slack_sec()
}

/// 写/破坏性命令是否强制要求显式 `--tab`（`ATB_REQUIRE_TAB=1`，默认关闭）。
///
/// 默认行为保持不变：不设环境变量时，缺省 `--tab` 仍作用于 daemon 当前标签页。
pub(crate) fn require_tab_enabled() -> bool {
    crate::settings::env_bool("ATB_REQUIRE_TAB", false)
}

/// 写类 op 缺少 `--tab` 时直接以用法错误拒绝。
///
/// 只在 `ATB_REQUIRE_TAB=1` 时生效：写/破坏性操作误作用到用户正在看的标签页
/// 代价很高，开启后强制调用方明确目标；关闭时完全沿用旧行为。
pub(crate) fn enforce_write_tab(op: &str, tab: Option<&str>) -> Result<()> {
    const WRITE_OPS: &[&str] = &[
        "click",
        "mouse-click",
        "fill",
        "send-keys",
        "select",
        "check",
        "hover",
        "scroll",
        "upload",
        "inject",
    ];
    if !require_tab_enabled() || !WRITE_OPS.contains(&op) {
        return Ok(());
    }
    if tab.map(str::trim).is_some_and(|value| !value.is_empty()) {
        return Ok(());
    }
    Err(usage_failure(
        "target_not_found",
        format!("{op}: 写/破坏性操作必须显式指定 --tab（已开启 ATB_REQUIRE_TAB）"),
        "先 atb tabs --ids 看清目标，再 --tab <tab_id>；确实要操作当前标签时显式写 --tab current",
    ))
}

/// 页面级 payload 的统一收尾：路径绝对化 → 参数预检 → 目标语法解析。
///
/// 同步路径（[`dispatch_page`]）与异步路径（`cmd_pipeline::submit_job`）**必须**走同一份
/// 逻辑：以前异步分支直接把原始 payload 交给 `/job/submit`，于是
/// `screenshot --async --out shots/a.png` 的路径被 daemon 按 `current_dir(home)` 解析成
/// `%USERPROFILE%\shots\a.png`，`upload --async` 则直接报文件不存在。
pub(crate) fn finalize_page_payload(
    op: &str,
    payload: &mut Value,
) -> std::result::Result<(), preflight::Problem> {
    // 相对路径以当前 shell 的 cwd 为基准绝对化后再下发。
    absolutize_payload_paths(
        payload,
        &["out", "files", "dir", "save_path", "output", "file"],
    );
    preflight::preflight_payload(op, payload)?;
    // 目标语法（`@x` / 空段 / 非法列表）在本地就能判定，异步提交也必须拦，
    // 否则错误会推迟到 job 结果里，退出码从 2 变成 0。
    if let Some(tab) = payload.get("switch_tab_id").and_then(Value::as_str) {
        if let Err(err) = TargetSpec::parse(tab) {
            return Err(spec_problem(err));
        }
    }
    Ok(())
}

/// 页面级命令的统一分发。
///
/// - 单目标：走原来的端点，行为与旧版本完全一致；
/// - 多目标（`--tab all` / `1,2,3` / `@1-3` / `url:` / `title:` / `group:`）：
///   一次请求打到 `/multi`，由 daemon 并发执行并按输入顺序汇总；
/// - `--dry-run`：只解析目标和校验参数，输出计划，不执行。
pub(crate) fn dispatch_page(
    op: &str,
    path: &str,
    payload: Value,
    tab: Option<&str>,
    browser: Option<&str>,
    profile: Option<&str>,
    timeout: f64,
) -> Result<()> {
    let mut payload = payload;
    // 写类命令的显式目标护栏：默认关闭，ATB_REQUIRE_TAB=1 时生效。
    enforce_write_tab(op, tab)?;
    if let Err(problem) = finalize_page_payload(op, &mut payload) {
        return usage_error(&problem);
    }
    let spec = match TargetSpec::parse(tab.unwrap_or("")) {
        Ok(spec) => spec,
        Err(err) => return usage_error(&spec_problem(err)),
    };
    let dry_run = DRY_RUN.load(Ordering::Relaxed);
    if dry_run || spec.is_multi() || spec.needs_daemon_resolution() {
        let mut args = payload;
        if let Some(object) = args.as_object_mut() {
            // 目标由 daemon 按解析结果注入，避免两边各算一遍还算出不同结果。
            object.remove("switch_tab_id");
            // 截图/PDF 的输出路径只有在**真的多目标**时才要求占位符。
            // 以前这段写在 `dry_run || is_multi()` 分支里，于是 `--tab @1 --out a.png`
            // 和任何 `--out a.png --dry-run` 都被判成冲突并以退出码 2 拒绝。
            if spec.is_multi() {
                if let Some(out) = object.get("out").and_then(Value::as_str) {
                    if !out.contains("{tab}") && !out.contains("{index}") {
                        return usage_error(
                            &preflight::Problem::new(
                                "output_path_collision",
                                format!("多目标写同一个输出文件 {out:?} 会互相覆盖"),
                                "在 --out 里加 {tab} 或 {index} 占位符，例如 shots/{index}-{tab}.png",
                            )
                            .with_field("out"),
                        );
                    }
                }
            }
        }
        let body = json!({
            "op": op,
            "args": args,
            "target": tab.unwrap_or(""),
            "browser": browser,
            "profile": profile,
            "dry_run": dry_run,
        });
        let value = request("POST", "/multi", Some(body), timeout)?;
        if dry_run {
            // dry-run 的字段形状要和真实执行一致，否则 Agent 用 dry-run 校准字段名后
            // 正式执行必然解析失败。
            print_json(normalize_dry_run(value));
            return Ok(());
        }
        print_json(value);
        return Ok(());
    }
    print_json(request("POST", path, Some(payload), timeout)?);
    Ok(())
}

/// 把 `/multi` 的 `targets`（计划）改写成与真实执行同形的 `results`。
fn normalize_dry_run(value: Value) -> Value {
    let Value::Object(mut map) = value else {
        return value;
    };
    let Some(targets) = map
        .remove("targets")
        .and_then(|value| value.as_array().cloned())
    else {
        return Value::Object(map);
    };
    let results: Vec<Value> = targets
        .into_iter()
        .map(|target| {
            json!({
                "index": target.get("index").cloned().unwrap_or(Value::Null),
                "tab_id": target.get("tab_id").cloned().unwrap_or(Value::Null),
                "title": target.get("title").cloned().unwrap_or(Value::Null),
                "url": target.get("url").cloned().unwrap_or(Value::Null),
                "ok": true,
                "planned": true,
            })
        })
        .collect();
    let count = results.len();
    map.insert("count".to_string(), json!(count));
    map.insert("results".to_string(), Value::Array(results));
    Value::Object(map)
}

pub(crate) fn cmd_scan(args: ScanArgs, is_async: bool) -> Result<()> {
    let _ = is_async;
    ensure_server()?;
    let mut payload = json!({
        "tabs_only": args.tabs_only,
        "text_only": args.text_only,
        "fast": args.fast,
        "max_nodes": args.max_nodes,
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "timeout": args.timeout,
    });
    omit_absent_numbers(&mut payload, &["max_nodes"]);
    if is_async {
        print_json(submit_job("scan", payload)?);
    } else {
        dispatch_page(
            "scan",
            "/scan",
            payload,
            args.tab.as_deref(),
            args.browser.as_deref(),
            args.profile.as_deref(),
            http_budget(args.timeout),
        )?;
    }
    Ok(())
}

pub(crate) fn cmd_exec(args: ExecArgs, is_async: bool) -> Result<()> {
    // 位置参数脚本与 --file 同时给时，旧实现静默只用 --file，用户写的那段
    // 脚本被无声丢弃（改成 --file 后忘了删旧参数就会踩）。两者的优先级不该靠猜，
    // 直接在发请求前以 exit 2 拒绝。位置参数缺省是空串，不算「给了」。
    if !args.script.is_empty() && args.file.is_some() {
        return Err(usage_failure(
            "unsupported_combination",
            "exec: 位置参数脚本与 --file 只能给一个",
            "要么 atb exec '<js>'，要么 atb exec --file script.js（`--file -` 读 stdin）",
        ));
    }
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
    let mut payload = json!({
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
    omit_absent_numbers(&mut payload, &["max_bytes"]);
    // `exec --async --dry-run` 以前会在 is_async 分支里直接提交 job：
    // dry-run 的承诺是「什么都不执行」，异步路径也必须先短路，且不能先拉起 daemon。
    if is_async && DRY_RUN.load(Ordering::Relaxed) {
        let script_bytes = payload
            .get("script")
            .and_then(Value::as_str)
            .map(str::len)
            .unwrap_or(0);
        print_json(json!({
            "ok": true,
            "result": {
                "dry_run": true,
                "command": "exec",
                "async": true,
                "would": ["后台执行脚本", "job 提交（POST /job/submit，op=exec）"],
                "script_bytes": script_bytes,
                "target": {
                    "tab": payload.get("switch_tab_id").cloned().unwrap_or(Value::Null),
                    "browser": payload.get("browser").cloned().unwrap_or(Value::Null),
                    "profile": payload.get("profile").cloned().unwrap_or(Value::Null),
                },
                "note": "去掉 --dry-run 才会真的提交 job；脚本内容已按当前参数读取完毕",
            },
        }));
        return Ok(());
    }
    ensure_server()?;
    if is_async {
        print_json(submit_job("exec", payload)?);
    } else {
        // 客户端要覆盖「页面执行 + 轮询等待」两段，并比服务端多留余量，
        // 让服务端的结构化超时错误先返回，而不是 reqwest 的裸 "operation timed out"。
        dispatch_page(
            "exec",
            "/exec",
            payload,
            args.tab.as_deref(),
            args.browser.as_deref(),
            args.profile.as_deref(),
            http_budget(args.timeout + args.wait_timeout),
        )?;
    }
    Ok(())
}

pub(crate) fn cmd_cookies(args: CookiesArgs, is_async: bool) -> Result<()> {
    let _ = is_async;
    ensure_server()?;
    let payload = json!({
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "url": args.url,
        "action": if args.clear { "clear" } else { "list" },
        "domain": args.domain,
    });
    if is_async {
        print_json(submit_job("cookies", payload)?);
    } else {
        dispatch_page(
            "cookies",
            "/cookies",
            payload,
            args.tab.as_deref(),
            args.browser.as_deref(),
            args.profile.as_deref(),
            http_budget(args.timeout),
        )?;
    }
    Ok(())
}

pub(crate) fn cmd_upload(args: UploadArgs, is_async: bool) -> Result<()> {
    let _ = is_async;
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
        dispatch_page(
            "upload",
            "/upload",
            payload,
            args.tab.as_deref(),
            args.browser.as_deref(),
            args.profile.as_deref(),
            http_budget(args.timeout),
        )?;
    }
    Ok(())
}

pub(crate) fn cmd_snapshot(args: SnapshotArgs, is_async: bool) -> Result<()> {
    let _ = is_async;
    ensure_server()?;
    let mut payload = json!({
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "offset": args.offset,
        "limit": args.limit,
        "details": args.details,
        "js": args.js,
        "interactive_only": args.interactive_only,
        "max_nodes": args.max_nodes,
        // X-15：daemon 的 /snapshot 早就有 skip_generic 字段，CLI 却一直没有下发通道，
        // 于是「不想被通用容器节点挤满 @e 编号」的用户无法表达。默认行为保持不变
        // （skip_generic=true），加 --no-skip-generic 才显式关闭。
        "skip_generic": !args.no_skip_generic,
        "timeout": args.timeout,
    });
    omit_absent_numbers(&mut payload, &["max_nodes"]);
    if is_async {
        print_json(submit_job("snapshot", payload)?);
    } else {
        dispatch_page(
            "snapshot",
            "/snapshot",
            payload,
            args.tab.as_deref(),
            args.browser.as_deref(),
            args.profile.as_deref(),
            http_budget(args.timeout),
        )?;
    }
    Ok(())
}

pub(crate) fn cmd_read(args: ReadArgs, is_async: bool) -> Result<()> {
    let _ = is_async;
    ensure_server()?;
    let mut payload = json!({
        "selector": args.selector,
        "max_chars": args.max_chars,
        // X-15：页面侧 page_read.js 的 innerTextLimit（默认 30000），None 不下发。
        "inner_text_limit": args.inner_text_limit,
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
    omit_absent_numbers(
        &mut payload,
        &["max_nodes", "budget_ms", "inner_text_limit"],
    );
    if is_async {
        print_json(submit_job("read", payload)?);
    } else {
        dispatch_page(
            "read",
            "/read",
            payload,
            args.tab.as_deref(),
            args.browser.as_deref(),
            args.profile.as_deref(),
            http_budget(args.timeout),
        )?;
    }
    Ok(())
}

pub(crate) fn cmd_find(args: FindArgs, is_async: bool) -> Result<()> {
    let _ = is_async;
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
        dispatch_page(
            "find",
            "/find",
            payload,
            args.tab.as_deref(),
            args.browser.as_deref(),
            args.profile.as_deref(),
            http_budget(args.timeout),
        )?;
    }
    Ok(())
}

pub(crate) fn cmd_inject(args: InjectArgs, is_async: bool) -> Result<()> {
    let _ = is_async;
    ensure_server()?;
    let (action, name, code) = if args.list {
        ("list", None, None)
    } else if let Some(name) = args.remove.clone() {
        ("remove", Some(name), None)
    } else {
        let code = match args.file.as_ref() {
            Some(path) => std::fs::read_to_string(path)
                .with_context(|| format!("读取脚本文件失败: {}", path.display()))?,
            None => args
                .script
                .clone()
                .ok_or_else(|| usage_err!("inject: 需要脚本内容，或 --file/--list/--remove"))?,
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
    // `--async inject` 以前静默同步执行（P3 CLI 一致性）：/job/submit 支持 inject，
    // 这里补上真正的后台提交。
    if is_async {
        print_json(submit_job("inject", payload)?);
        return Ok(());
    }
    dispatch_page(
        "inject",
        "/inject",
        payload,
        args.tab.as_deref(),
        args.browser.as_deref(),
        args.profile.as_deref(),
        http_budget(args.timeout),
    )?;
    Ok(())
}

pub(crate) fn cmd_frames(args: FramesArgs, is_async: bool) -> Result<()> {
    let _ = is_async;
    ensure_server()?;
    let payload = json!({
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
    });
    // 同 inject：frames 在 JOB_OPS 里，`--async frames` 应该真的异步。
    if is_async {
        print_json(submit_job("frames", payload)?);
        return Ok(());
    }
    dispatch_page(
        "frames",
        "/frames",
        payload,
        args.tab.as_deref(),
        args.browser.as_deref(),
        args.profile.as_deref(),
        http_budget(args.timeout),
    )?;
    Ok(())
}
