use super::*;

#[derive(Debug, Serialize, Deserialize, Default)]
pub(crate) struct BrowserTarget {
    pub(crate) browser_id: String,
    pub(crate) browser_label: Option<String>,
    pub(crate) profile_id: String,
    pub(crate) profile_label: Option<String>,
    pub(crate) session_key: String,
    pub(crate) tab_id: String,
    pub(crate) url: String,
    pub(crate) title: String,
    pub(crate) extension_version: Option<String>,
}

/// 枚举当前所有 (browser, profile) 目标；每个组合取一个代表标签页。
pub(crate) async fn browser_targets(state: &AppState) -> Vec<BrowserTarget> {
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
            browser_label: session.info.browser_label.clone(),
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

pub(crate) async fn browsers(State(state): State<AppState>) -> Json<Value> {
    touch(&state).await;
    // daemon 刚启动 / 扩展刚重连时，会话表还是空的，直接读会返回 count:0，
    // 调用方据此判定「没有浏览器」——其实只是快照还没到（tabs 早已这样对齐）。
    // 只读档等待，避免浏览器真的没开时白等 5 秒。
    wait_for_sessions(&state, WAIT_SESSIONS_READ).await;
    // 再主动向扩展要一次标签快照；失败就用现有快照，不阻塞、不报错。
    let _ = refresh_tabs_via_extension(&state, None, Duration::from_millis(1500)).await;
    let targets = browser_targets(&state).await;
    let mut browsers: Vec<Value> = Vec::new();
    for target in &targets {
        let profile = json!({
            "profile_id": target.profile_id,
            "browser_label": target.browser_label,
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
                "browser_label": target.browser_label,
                "profiles": [profile],
            })),
        }
    }
    // count 的口径是**浏览器数量**（同一浏览器的多个 Profile 只算一个）：
    // 旧实现返回 targets.len()，多 Profile 场景下调用方会以为开了好几个浏览器。
    // targets 数组保持不变（向后兼容），Profile 数量放进 profile_count。
    let mut result = json!({
        "count": browsers.len(),
        "profile_count": targets.len(),
        "browsers": browsers,
        "targets": targets,
    });
    if targets.is_empty() && has_live_bridge(&*state.driver.read().await) {
        // 桥活着但一条快照都没有：是「还没到」而不是「没有浏览器」。给出可判定的
        // warning_code + retryable + retry_after_ms，调用方不用读中文就能决定重试。
        result["warning"] = json!("扩展已连接但标签快照尚未到达");
        result["warning_code"] = json!("snapshot_pending");
        result["retryable"] = json!(true);
        result["retry_after_ms"] = json!(1500);
        result["hint"] =
            json!("daemon 刚启动或扩展刚重连，稍后重试；atb status 可看 extension_connected");
    }
    Json(json!({
        "ok": true,
        "result": result,
    }))
}

/// `atb launch` 的服务端部分。
///
/// 浏览器必须由 daemon 拉起：daemon 是常驻进程，浏览器作为它的子进程不会随
/// 「一条命令结束」被回收（很多运行环境会把命令的整棵进程树一起收掉）。
#[derive(Debug, Deserialize)]
pub(crate) struct LaunchRequest {
    #[serde(default)]
    pub(crate) browser: Option<String>,
    #[serde(default)]
    pub(crate) user_data_dir: Option<String>,
    #[serde(default)]
    pub(crate) restart: bool,
    #[serde(default)]
    pub(crate) force: bool,
    #[serde(default = "default_true")]
    pub(crate) restore: bool,
    #[serde(default)]
    pub(crate) args: Vec<String>,
    #[serde(default)]
    pub(crate) urls: Vec<String>,
}

pub(crate) async fn launch_browser(
    State(state): State<AppState>,
    Json(req): Json<LaunchRequest>,
) -> Json<Value> {
    touch(&state).await;
    let kind = match req.browser.as_deref() {
        Some(raw) => match crate::launch::Kind::parse(raw) {
            Ok(kind) => Some(kind),
            Err(err) => {
                return Json(json!({
                    "ok": false,
                    "error": err.to_string(),
                    "error_code": "bad_browser",
                }))
            }
        },
        None => None,
    };
    let request = crate::launch::Request {
        kind,
        user_data_dir: req.user_data_dir.map(std::path::PathBuf::from),
        restart: req.restart,
        force: req.force,
        restore_session: req.restore,
        extra_args: req.args,
        urls: req.urls,
    };
    // 里面是枚举进程 / 等进程退出这类阻塞调用，放到阻塞线程池，别占住 async 运行时。
    match tokio::task::spawn_blocking(move || crate::launch::run(&request)).await {
        Ok(value) => Json(value),
        Err(err) => Json(json!({
            "ok": false,
            "error": format!("启动任务异常: {err}"),
            "error_code": "launch_failed",
        })),
    }
}
