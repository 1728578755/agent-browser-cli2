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
        agent_defaults: Arc::new(Mutex::new(HashMap::new())),
        agent_opened: Arc::new(Mutex::new(HashMap::new())),
        ext_restart_seq: Arc::new(Mutex::new(HashMap::new())),
        cleanup_done: Arc::new(AtomicBool::new(false)),
        ext_warnings: Arc::new(Mutex::new(Vec::new())),
        // / A1-09 新增状态字段：与生产构造点保持同步。
        idempotency: Arc::new(Mutex::new(HashMap::new())),
        late_results: Arc::new(Mutex::new(HashMap::new())),
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
            extension_protocol: crate::protocol::EXTENSION_PROTOCOL_VERSION,
            file_scheme_access: Some(true),
            info: TabInfo {
                id: tab_id.to_string(),
                window_id: None,
                tab_id: tab_id.to_string(),
                browser_id: browser_id.to_string(),
                profile_id: profile_id.to_string(),
                profile_label: None,
                browser_label: None,
                session_key,
                url: "https://example.com".to_string(),
                title: "test".to_string(),
                group_title: None,
                active: true,
                tab_type: "ext_ws".to_string(),
                connected_at: None,
            },
            sender,
            disconnected_at: None,
            conn_id: 0,
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
        HeaderValue::from_static("chrome-extension://abcdefghijklmnopabcdefghijklmnop.example.com"),
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

/// / §4.4：坏 JSON 与**空内容**都必须变成 `Corrupt`，绝不能退化成「未配置」
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

/// `/reload-trust` 的判定核心——只踢掉白名单外的人，兼容模式不踢。
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
    let (sender, _receiver) = mpsc::unbounded_channel();
    ws_connections_lock(&state).insert(
        7,
        WsConn {
            extension_id: "x".to_string(),
            token_fingerprint: None,
            revoked,
            session_registry: Arc::new(std::sync::Mutex::new(HashSet::new())),
            sender,
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

/// challenge 的外层 `ok` 必须等于业务结论，且 `result` 内部结构不动。
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

/// 卡死阈值必须同时满足「不误伤正常长命令」与「晚于正常空闲」。
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

/// open 的挑战探测必须「正常页面零成本、可疑页面才付一次往返」。
#[test]
fn should_probe_challenge_only_for_suspicious_pages() {
    // 正常页面：标题能看出来，直接跳过。
    assert!(!should_probe_challenge(
        "https://example.com",
        "Example Domain"
    ));
    assert!(!should_probe_challenge(
        "https://a.com",
        "百度一下，你就知道"
    ));
    // CF 拦截页的稳定标题。
    assert!(should_probe_challenge("https://a.com", "Just a moment..."));
    assert!(should_probe_challenge(
        "https://a.com",
        "Attention Required! | Cloudflare"
    ));
    assert!(should_probe_challenge("https://a.com", "请稍候…"));
    // 标题为空：挑战页在响应头到达前就是空标题，值得探一次。
    assert!(should_probe_challenge("https://a.com", ""));
    assert!(should_probe_challenge("https://a.com", "   "));
    // 非 http(s) 一律不探：本地文件 / about:blank 不会挂挑战页。
    assert!(!should_probe_challenge(
        "file:///C:/a.html",
        "Just a moment..."
    ));
    assert!(!should_probe_challenge("about:blank", ""));
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
    // query 语义里裸 `+` 不是空格；空格只能来自 `%20`（A1-03）。
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
/// 改动这两个哨兵必须同步改 mock，否则守卫会红得莫名其妙。
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

/// `/reload-trust` 必须落在 API 鉴权层内，且回包报告检查了多少条连接。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reload_trust_requires_token_and_reports_checked() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = test_state();
    // 预置一条已登记连接：用来验证 `checked` 真的在数登记表，而不是写死 0。
    ws_connections_lock(&state).insert(1, {
        let (sender, _receiver) = mpsc::unbounded_channel();
        WsConn {
            extension_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            token_fingerprint: None,
            revoked: Arc::new(AtomicBool::new(false)),
            session_registry: Arc::new(std::sync::Mutex::new(HashSet::new())),
            sender,
        }
    });
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
        0,
        &std::sync::Mutex::new(HashSet::new()),
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
    fn record(id: &str, status: JobStatus, created_at: f64, finished_at: Option<f64>) -> JobRecord {
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
    // 以前 `starts_with("chrom")` 会误杀这两个正常域名。
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

/// `--browser Edge` 同时命中 `Edge` 与 `Edge-Beta` 时必须报歧义，
/// 而不是按 default → latest → connected_at 静默挑一个。
#[test]
fn browser_label_ambiguity_is_reported_not_guessed() {
    let mut driver = DriverState::default();
    insert_test_session(&mut driver, "browser-a", "profile-a", "1");
    insert_test_session(&mut driver, "browser-b", "profile-b", "2");
    let key_a = crate::protocol::make_session_key("browser-a", "profile-a", "1");
    let key_b = crate::protocol::make_session_key("browser-b", "profile-b", "2");
    driver.sessions.get_mut(&key_a).unwrap().info.browser_label = Some("Edge".to_string());
    driver.sessions.get_mut(&key_b).unwrap().info.browser_label = Some("Edge-Beta".to_string());

    let selector = SessionSelector::new(None, Some("Edge".to_string()), None);
    let err = resolve_session(&driver, &selector, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("歧义"), "{err}");
    assert!(
        err.contains("Edge-Beta"),
        "候选里必须能看见另一个浏览器: {err}"
    );

    // 更精确的标签仍然可选：歧义判定不能把正常用法一起挡掉。
    let selector = SessionSelector::new(None, Some("Edge-Beta".to_string()), None);
    assert_eq!(resolve_session(&driver, &selector, None).unwrap().0, key_b);
}

/// 旧报告 #6：身份头只接受 `[A-Za-z0-9._:-]{1,80}`，其余一律回落 default。
#[test]
fn agent_identity_header_shape_is_validated() {
    assert!(is_valid_agent_id("main-agent"));
    assert!(is_valid_agent_id("team:reviewer.v2_1"));
    assert!(!is_valid_agent_id(""));
    assert!(!is_valid_agent_id("客服"));
    assert!(!is_valid_agent_id("agent one"));
    assert!(!is_valid_agent_id(&"a".repeat(81)));
}

/// `ext_ready.restartSeq` 必须能被取出来；字段缺失/类型不对时当作没上报。
#[test]
fn restart_seq_is_parsed_from_ext_ready() {
    assert_eq!(
        parse_restart_seq(
            r#"{"type":"ext_ready","restartSeq":7,"browser_id":"b","profile_id":"p"}"#
        ),
        Some(("b".to_string(), "p".to_string(), 7))
    );
    // 缺 browser_id / profile_id 时用与反序列化同一套默认值。
    assert_eq!(
        parse_restart_seq(r#"{"type":"ext_ready","restartSeq":1}"#),
        Some((
            "browser-default".to_string(),
            "profile-default".to_string(),
            1
        ))
    );
    assert_eq!(parse_restart_seq(r#"{"type":"ext_ready"}"#), None);
    assert_eq!(
        parse_restart_seq(r#"{"type":"ext_ready","restartSeq":"3"}"#),
        None
    );
}

/// `multi_browser_hint` 是「只加字段」的能力，但判据必须精确：否则每条 read 都挂一句
/// 噪声提示，或者在真正的多浏览器场景下反而不提示。
#[test]
fn multi_browser_hint_requires_two_browsers_and_implicit_target() {
    let mut driver = DriverState::default();
    let key_a = crate::protocol::make_session_key("browser-a", "profile-1", "1");
    let _rx_a = insert_test_session(&mut driver, "browser-a", "profile-1", "1");

    let implicit = SessionSelector::new(None, None, None);
    assert!(
        multi_browser_hint(&driver, &key_a, &implicit).is_none(),
        "只有一个浏览器时不该提示"
    );

    let _rx_b = insert_test_session(&mut driver, "browser-b", "profile-2", "2");
    let hint = multi_browser_hint(&driver, &key_a, &implicit).expect("两个浏览器时必须提示");
    assert!(hint.contains("多个浏览器"), "提示文案应说明是多浏览器场景");

    // 显式指定目标时不需要提示：调用方已经知道自己在操作哪个。
    for selector in [
        SessionSelector::new(Some("1".to_string()), None, None),
        SessionSelector::new(None, Some("browser-a".to_string()), None),
        SessionSelector::new(None, None, Some("profile-1".to_string())),
    ] {
        assert!(
            multi_browser_hint(&driver, &key_a, &selector).is_none(),
            "显式目标不应附带隐式默认提示"
        );
    }
}
