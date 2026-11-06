use super::*;
#[allow(unused_imports)]
use clap::{Args, Parser, Subcommand};

/// 把 `--header "Name: Value"` 解析成 JSON 对象。
///
/// 漏冒号必须报错、不能静默跳过：`--header Authorization` 被丢掉时请求会**匿名**发出，
/// 调用方看到的是 401/403，很难联想到是参数少了冒号。
pub(crate) fn parse_headers(pairs: &[String]) -> Result<Value> {
    let mut map = serde_json::Map::new();
    // HTTP 头名大小写不敏感：`-H "A: 1" -H "a: 2"` 以前会留下两个键（daemon 侧
    // 谁生效取决于实现），实际发出去的头等于「看运气」。重复一律报错。
    let mut seen: Vec<String> = Vec::new();
    for pair in pairs {
        let Some((key, value)) = pair.split_once(':') else {
            return Err(usage_err!(
                "--header / --request-header 需要 `Name: Value` 形式（缺少冒号）: {pair}"
            ));
        };
        let key = key.trim();
        let lower = key.to_ascii_lowercase();
        if seen.contains(&lower) {
            return Err(usage_err!(
                "--header 重复的键: {key}（同一 header 请合并成一个值）"
            ));
        }
        seen.push(lower);
        map.insert(key.to_string(), json!(value.trim()));
    }
    Ok(Value::Object(map))
}

/// 解析 `--body`：`{`/`[` 开头的一律按 JSON 严格解析，其余按纯文本字符串发送。
///
/// 早先是 `if let Ok(...)` 失败后落到 `json!(text)`，把坏 JSON 当字符串发出去，
/// 服务端收到字符串而非对象，表现为「mock 规则不生效」/「请求体字段全 undefined」。
/// 现在与 `run_broadcast` 的 `--args` 一致：解析失败直接报错，错误信息带行列位置。
pub(crate) fn parse_body(text: &str) -> Result<Value> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return match serde_json::from_str::<Value>(text) {
            Ok(value) => Ok(value),
            // 用法错（退出码 2），不是执行失败：以前走 anyhow → 退出码 1 +
            // 纯文本，调用方会当成「可重试」。四个 --body 调用点统一继承。
            Err(err) => Err(usage_failure(
                "invalid_json",
                format!(
                    "--body 以 {{ 或 [ 开头但不是合法 JSON（第 {} 行第 {} 列）: {}",
                    err.line(),
                    err.column(),
                    err
                ),
                "检查引号/逗号/括号是否配对；要按纯文本发送，请不要以 { 或 [ 开头",
            )),
        };
    }
    Ok(json!(text))
}

pub(crate) fn run_net_command(cmd: NetCommand) -> Result<()> {
    let (payload, timeout) = match cmd.action {
        NetAction::Fetch(args) => {
            // 枚举值本地校验：非法值以前会原样下发，daemon 静默回落默认策略，
            // 调用方以为「omit 掉了 cookie」，实际按 include 发了。
            let credentials = args.credentials.to_ascii_lowercase();
            if !matches!(credentials.as_str(), "include" | "omit" | "same-origin") {
                return Err(usage_failure(
                    "invalid_argument",
                    format!("net fetch --credentials 的值 {:?} 不合法", args.credentials),
                    "可选 include（默认，带 cookie）/ omit（不带）/ same-origin（仅同源带）",
                ));
            }
            let mut payload = json!({
                "action": "fetch",
                "url": args.url,
                "method": args.method,
                "response_type": args.response_type,
                "max_chars": args.max_chars,
                "credentials": credentials,
                "parse_json": !args.no_parse_json,
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
            // 只把**显式给出**的开关放进 payload：服务端对 None 的处理是「沿用已存值」。
            // 旧实现每次都给 capture_bodies/max_log 默认值，第二次 `net hook --persist`
            // 会把之前持久化的设置静默压回 false/200（端到端）。
            let mut payload = json!({
                "action": "hook",
                "persist": args.persist,
                "frame_id": args.frame,
                "switch_tab_id": args.tab,
                "browser": args.browser,
                "profile": args.profile,
            });
            if let Some(capture) = args.capture_bodies {
                payload["capture_bodies"] = json!(capture);
            }
            if let Some(max_log) = args.max_log {
                payload["max_log"] = json!(max_log);
            }
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
    // 所有参数解析/校验都在起 daemon 之前完成：坏参数应当 exit 2，
    // 而不是先把 daemon 拉起来再报错。
    ensure_server()?;
    print_json(request(
        "POST",
        "/net",
        Some(payload),
        http_budget(timeout),
    )?);
    Ok(())
}

pub(crate) fn run_network_command(args: NetworkCommand) -> Result<()> {
    ensure_server()?;
    match args.action {
        NetworkAction::Start(args) => print_json(request(
            "POST",
            "/network/start",
            Some(
                json!({ "switch_tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
            ),
            http_budget(args.timeout),
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
            http_budget(args.timeout),
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
            http_budget(args.timeout),
        )?),
        NetworkAction::Clear(args) => print_json(request(
            "POST",
            "/network/clear",
            Some(
                json!({ "switch_tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
            ),
            http_budget(args.timeout),
        )?),
        NetworkAction::Stop(args) => print_json(request(
            "POST",
            "/network/stop",
            Some(
                json!({ "switch_tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
            ),
            http_budget(args.timeout),
        )?),
    }
    Ok(())
}

pub(crate) fn run_console_command(args: ConsoleCommand) -> Result<()> {
    ensure_server()?;
    match args.action {
        ConsoleAction::Start(args) => print_json(request(
            "POST",
            "/console/start",
            Some(
                json!({ "switch_tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
            ),
            http_budget(args.timeout),
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
            http_budget(args.timeout),
        )?),
        ConsoleAction::Clear(args) => print_json(request(
            "POST",
            "/console/clear",
            Some(
                json!({ "switch_tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
            ),
            http_budget(args.timeout),
        )?),
        ConsoleAction::Stop(args) => print_json(request(
            "POST",
            "/console/stop",
            Some(
                json!({ "switch_tab_id": args.tab, "browser": args.browser, "profile": args.profile }),
            ),
            http_budget(args.timeout),
        )?),
    }
    Ok(())
}
