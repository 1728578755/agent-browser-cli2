use super::*;

/// 一个可执行「切前台」动作的候选会话：身份 + 通道 + 版本 + 能否 file://。
/// 类型别名只是为了让 `apply_tabs_snapshot` 里的 `Vec<...>` 可读。
pub(crate) type TabCandidate = (
    String,
    mpsc::UnboundedSender<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    u32,
    Option<bool>,
);

/// 用一份完整标签页快照重建会话表。
///
/// 三个入口共用这一份逻辑：扩展主动推送 `ext_ready` / `tabs_update`、daemon 在
/// `open` 之后主动拉取、以及 `/tabs` 发现列表可能过期时的兜底刷新。分开实现会
/// 出现「推送说没有、拉取说有」的抖动，最终表现成标签页随机消失。
// 11 个参数确实多，但它们就是「一次快照」的全部字段（身份 + 版本 + 标签页 +
// 已注册 id 表 + 推送方连接代际）。打成 struct 只会把调用点变成一串字段名，可读性并未变好。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_tabs_snapshot(
    state: &AppState,
    sender: &mpsc::UnboundedSender<String>,
    // 这份快照由哪条连接推送：写进每个 Session，供断开清理比对代际。
    conn_id: u64,
    browser_id: String,
    browser_label: Option<String>,
    profile_id: String,
    profile_label: Option<String>,
    extension_version: Option<String>,
    extension_protocol: u32,
    file_scheme_access: Option<bool>,
    tabs: Vec<crate::protocol::ExtTab>,
    registered: &std::sync::Mutex<HashSet<String>>,
) {
    let mut driver = state.driver.write().await;
    // A2：过期的「待生效默认目标」在这里统一清一次（6h TTL）；之后所有读取
    // 都走 `preferred_default_session()`，读到的都是仍然有效的记忆。
    driver.expire_preferred_default_session();
    let mut newly_registered: Vec<String> = Vec::new();
    let first_tab_id = tabs
        .first()
        .map(|t| strip_outer_quotes(&t.id.to_string()).to_string());
    let current: HashSet<String> = tabs
        .iter()
        .map(|t| {
            let tab_id = strip_outer_quotes(&t.id.to_string()).to_string();
            crate::protocol::make_session_key(&browser_id, &profile_id, &tab_id)
        })
        .collect();
    let now_epoch = epoch_secs();
    for session in driver.sessions.values_mut() {
        if session.info.tab_type == "ext_ws"
            && session.browser_id == browser_id
            && session.profile_id == profile_id
            && !current.contains(&session.session_key)
        {
            // 刚打开不到 5 秒的标签页给一个宽限期：扩展的标签列表可能慢一拍，
            // 立刻标成断开会让「open 之后马上 read」偶发失败。
            let fresh = session
                .info
                .connected_at
                .map(|at| now_epoch - at < 5.0)
                .unwrap_or(false);
            // 只在**首次**断开时写入时间戳：每次快照都覆盖成 now，
            // 会让 prune_sessions 的「断开超过 STALE_SESSION_TTL」永远为 0，
            // sessions 与 snapshots（整页 HTML）于是只增不减。
            if !fresh && session.disconnected_at.is_none() {
                session.disconnected_at = Some(Instant::now());
            }
        }
    }
    for tab in tabs {
        let mut info = tab.into_tab_info(&browser_id, &profile_id, profile_label.clone());
        // 可读的浏览器标识跟着标签页走：`tabs` / `browsers` / `--browser` 都靠它区分
        // 「Edge 那个」还是「Chrome 那个」，不用记 UUID。
        //
        // 刷新路径（refresh_tabs_via_extension）拿不到标签，传的是 None：
        // 这里必须**保留已有值**，否则每调一次 tabs 就把标签抹成 null。
        info.browser_label = browser_label.clone().or_else(|| {
            driver
                .sessions
                .get(&info.session_key)
                .and_then(|existing| existing.info.browser_label.clone())
        });
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
        newly_registered.push(session_key.clone());
        driver.latest_session_key = Some(session_key.clone());
        if should_set_default_session(&driver, &session_key) {
            driver.default_session_key = Some(session_key.clone());
            if driver.preferred_default_session() == Some(session_key.as_str()) {
                driver.clear_preferred_default_session();
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
                extension_protocol,
                file_scheme_access,
                info,
                sender: sender.clone(),
                disconnected_at: None,
                // 每次快照都覆写：同一 key 被**新连接**重注册后，归属就转到新代际，
                // 旧连接的断开清理会因此跳过它。
                conn_id,
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
    // 登记表合并放在 driver 锁之外：`session_registry` 是 std Mutex，
    // 与 driver 锁嵌套只会给将来埋锁序问题。
    //
    // 用 `HashSet` 而不是 `Vec`：连接是长命的，标签反复开关时 Vec 会
    // 只增不减（每次都对全表做一次 `iter().any()` 线性查重），Set 天然去重。
    if !newly_registered.is_empty() {
        let mut registry = registered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for key in newly_registered {
            registry.insert(key);
        }
    }
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
            request_extension_reload(state, &browser_id, version, first_tab_id, sender.clone())
                .await;
        }
    }
}

/// 主动向扩展拉一次完整标签页列表并落库。
///
/// 扩展的 `tabs_update` 是推送模型，Service Worker 被回收、定时器被节流时都可能延迟
/// 十几秒——表现为 `open` 之后新标签「看不见」。凡是要依赖标签列表的地方
/// （open 之后、tabs 命令）都先用这条拉取路径把状态对齐，再判断。
/// 用执行者会话的元信息，把刚打开的标签页登记成可操作会话。
///
/// 为什么不直接等扩展推送：MV3 的 Service Worker 会被节流，`tabs_update` 可能延迟
/// 十几秒；而 `openTab` 已经把 tab id 返回了，daemon 完全可以直接建立会话，
/// 让紧接着的 read/click 立刻可用。真实标题/URL 由后续快照覆盖。
pub(crate) async fn register_opened_session(
    state: &AppState,
    executor_key: &str,
    new_key: &str,
    tab_id: &str,
    url: &str,
    title: &str,
) -> bool {
    let agent = current_agent();
    {
        let mut opened = state.agent_opened.lock().await;
        let list = opened.entry(agent.clone()).or_default();
        if !list.iter().any(|key| key == new_key) {
            list.push(new_key.to_string());
            // 只保留最近 200 条，避免长跑 daemon 无限增长。
            if list.len() > 200 {
                let overflow = list.len() - 200;
                list.drain(0..overflow);
            }
        }
    }
    remember_agent_default(state, &agent, new_key).await;
    persist_agent_opened(state);
    let mut driver = state.driver.write().await;
    if let Some(existing) = driver.sessions.get(new_key) {
        if existing.is_active() {
            if agent == "default" {
                driver.default_session_key = Some(new_key.to_string());
                driver.clear_preferred_default_session();
            }
            return true;
        }
    }
    let Some(executor) = driver.sessions.get(executor_key) else {
        return false;
    };
    let executor_sender = executor.sender.clone();
    // 新会话挂在执行者那条连接上，代际必须一致：
    // 否则执行者连接断开时，这条「手工登记」的会话会因为 conn_id 不等而被漏清理。
    let executor_conn_id = executor.conn_id;
    let session = Session {
        session_key: new_key.to_string(),
        tab_id: tab_id.to_string(),
        browser_id: executor.browser_id.clone(),
        profile_id: executor.profile_id.clone(),
        profile_label: executor.profile_label.clone(),
        extension_version: executor.extension_version.clone(),
        extension_protocol: executor.extension_protocol,
        file_scheme_access: executor.file_scheme_access,
        info: TabInfo {
            id: tab_id.to_string(),
            window_id: None,
            tab_id: tab_id.to_string(),
            browser_id: executor.browser_id.clone(),
            profile_id: executor.profile_id.clone(),
            profile_label: executor.profile_label.clone(),
            browser_label: None,
            session_key: new_key.to_string(),
            url: url.to_string(),
            title: title.to_string(),
            group_title: None,
            active: false,
            tab_type: "ext_ws".to_string(),
            connected_at: Some(epoch_secs()),
        },
        sender: executor.sender.clone(),
        disconnected_at: None,
        conn_id: executor_conn_id,
    };
    driver.sessions.insert(new_key.to_string(), session);
    if agent == "default" {
        driver.default_session_key = Some(new_key.to_string());
        driver.clear_preferred_default_session();
    }
    drop(driver);
    // 这条会话挂在执行者的通道上，但没有经过任何一次快照，所以必须手工登记到
    // 该连接的登记表（与 同源）：否则扩展断开时它永远不会被标断开/清理。
    if let Some(registry) = connection_session_registry(state, &executor_sender) {
        let mut keys = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        keys.insert(new_key.to_string());
    }
    state.sessions_ready.notify_waiters();
    true
}

pub(crate) async fn refresh_tabs_via_extension(
    state: &AppState,
    preferred: Option<&str>,
    timeout: Duration,
) -> Result<usize> {
    // 候选执行者：优先 preferred，其余按「连接时间从早到晚」。
    // 新开的标签页还没加载完，向它发命令会失败（content script 尚未注入），
    // 而刷新的目的恰恰是「让新标签可见」——所以必须能退到更老的、已就绪的会话。
    let candidates: Vec<TabCandidate> = {
        let driver = state.driver.read().await;
        let mut sessions: Vec<&Session> = driver
            .sessions
            .values()
            .filter(|session| session.is_active())
            .collect();
        sessions.sort_by(|left, right| {
            let left_at = left.info.connected_at.unwrap_or(f64::INFINITY);
            let right_at = right.info.connected_at.unwrap_or(f64::INFINITY);
            left_at
                .partial_cmp(&right_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if let Some(key) = preferred {
            sessions.sort_by_key(|session| session.session_key != key);
        }
        sessions
            .into_iter()
            .take(5)
            .map(|session| {
                (
                    session.session_key.clone(),
                    session.sender.clone(),
                    session.browser_id.clone(),
                    session.profile_id.clone(),
                    session.profile_label.clone(),
                    session.extension_version.clone(),
                    session.extension_protocol,
                    session.file_scheme_access,
                )
            })
            .collect()
    };
    if candidates.is_empty() {
        // 旧实现裸 anyhow「没有已连接的标签页」：本质是桥断开（可重试），不该和
        // 「真没标签」共用一个失败语义。message 保持原文，只补结构化码。
        return Err(bridge_disconnected_error(
            "没有已连接的标签页，无法刷新标签列表",
        ));
    }

    let mut last_error: Option<String> = None;
    let mut timed_out = false;
    for (
        key,
        sender,
        browser_id,
        profile_id,
        profile_label,
        ext_version,
        ext_protocol,
        file_access,
    ) in candidates
    {
        // 外层再套一层超时：`extension_control` 自己的 timeout 只包住
        // 「最后等回包」，前面的 `session_lock(...).await` 与 `serial.lock().await`
        // 没有超时。后台 job / 长 `exec`（hold_lock=true，最长 600s）通常正持有那把
        // 锁，于是 `tabs` 会跟着卡到 CLI 的 30s 请求超时——用户看到超时，daemon 却正常。
        let outcome = tokio::time::timeout(
            timeout,
            extension_control(state, &key, "tabs", json!({}), timeout),
        )
        .await;
        let value = match outcome {
            Ok(Ok(value)) => value,
            Ok(Err(err)) => {
                last_error = Some(err.to_string());
                continue;
            }
            Err(_) => {
                // 拿不到会话锁 / 回包太慢：用现有快照，不当成失败。
                timed_out = true;
                last_error = Some(format!(
                    "刷新标签列表超时（{}ms 内含等待会话锁），沿用现有快照",
                    timeout.as_millis()
                ));
                continue;
            }
        };
        // `extension_control` 已经把扩展回包的 `data` 拆出来了：这里既可能是
        // 数组本身，也可能（旧路径）是包着 data 的对象，两种都接受。
        let list = if let Some(items) = value.as_array() {
            items.clone()
        } else {
            value
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let tabs: Vec<crate::protocol::ExtTab> = list
            .into_iter()
            .filter_map(|item| serde_json::from_value(item).ok())
            .collect();
        if tabs.is_empty() {
            last_error = Some("扩展返回的标签页列表为空".to_string());
            continue;
        }
        let count = tabs.len();
        // 复用这条连接自己的登记表：旧实现用函数内局部 `Vec`，返回即丢，
        // 于是「由 refresh 首次登记」的会话在 WS 断开时不会被标成 disconnected，
        // is_active 永远为真 → /health.ready 谎报、daemon 永不空闲退出。
        let registry = connection_session_registry(state, &sender).unwrap_or_else(|| {
            // 理论上不可达：sender 一定由 handle_socket 建好并先登记连接。
            // 真出现时用一个临时表，至少不 panic；断开清理那侧会按 sender 兜底。
            Arc::new(std::sync::Mutex::new(HashSet::new()))
        });
        // 代际跟着通道走。找不到登记项时保留旧值的语义没有意义：
        // 这条会话本来就是由 refresh 首次登记的，用 0 表示「无连接」，断开时按 sender 兜底。
        let conn_id = connection_conn_id(state, &sender).unwrap_or(0);
        apply_tabs_snapshot(
            state,
            &sender,
            conn_id,
            browser_id,
            None,
            profile_id,
            profile_label,
            ext_version,
            ext_protocol,
            file_access,
            tabs,
            &registry,
        )
        .await;
        return Ok(count);
    }
    // 全部候选都只是「等锁/等回包超时」：沿用手里的快照，不把刷新失败当成命令失败。
    if timed_out {
        return Ok(active_session_count(state).await);
    }
    Err(anyhow!(
        "{}",
        last_error.unwrap_or_else(|| "刷新标签列表失败".to_string())
    ))
}

/// 这条下行通道所属连接登记过的会话表。
///
/// refresh 路径手上只有 `Session.sender`，而「这条连接登记过哪些会话」在 `WsConn` 里；
/// 按 `same_channel` 匹配，找不到返回 `None`（sender 一定来自 handle_socket，
/// 正常不会发生）。
pub(crate) fn connection_session_registry(
    state: &AppState,
    sender: &mpsc::UnboundedSender<String>,
) -> Option<Arc<std::sync::Mutex<HashSet<String>>>> {
    let conns = ws_connections_lock(state);
    conns
        .values()
        .find(|conn| conn.sender.same_channel(sender))
        .map(|conn| conn.session_registry.clone())
}

/// 这条下行通道属于哪一代连接。
///
/// refresh 路径只有 `Session.sender`，但写快照时必须带上 `conn_id`：否则
/// 「由 refresh 首次登记的会话」代际是 0，连接断开时会被当成别人的会话跳过，
/// 于是永远留在 sessions 里（`/health.ready` 谎报 + 空闲自杀判不出来）。
pub(crate) fn connection_conn_id(
    state: &AppState,
    sender: &mpsc::UnboundedSender<String>,
) -> Option<u64> {
    let conns = ws_connections_lock(state);
    conns
        .iter()
        .find(|(_, conn)| conn.sender.same_channel(sender))
        .map(|(conn_id, _)| *conn_id)
}

/// 某个会话当前归属哪一代连接。
///
/// 结果回包路径不能拿 `driver` 锁（会和在途执行/回包形成环），只能读连接登记表：
/// 返回**登记过这个 key 的最大 conn_id**，即「最近一次重注册」的那一代。
pub(crate) fn session_owner_conn(state: &AppState, session_key: &str) -> Option<u64> {
    let conns = ws_connections_lock(state);
    conns
        .iter()
        .filter(|(_, conn)| {
            conn.session_registry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains(session_key)
        })
        .map(|(conn_id, _)| *conn_id)
        .max()
}

pub(crate) async fn active_session_count(state: &AppState) -> usize {
    let driver = state.driver.read().await;
    driver.sessions.values().filter(|s| s.is_active()).count()
}

pub(crate) async fn active_tabs(state: &AppState, wait_ready: bool) -> Vec<TabInfo> {
    active_tabs_filtered(state, wait_ready, None, None)
        .await
        .unwrap_or_default()
}

pub(crate) async fn active_tabs_filtered(
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
            .filter(|s| {
                browser
                    .map(|b| browser_selector_matches(s, b))
                    .unwrap_or(true)
            })
            .filter(|s| s.profile_label.as_deref() == Some(profile))
            .map(|s| s.profile_id.clone())
            .collect();
        if matched_profile_ids.len() > 1 {
            let mut ids: Vec<String> = matched_profile_ids.into_iter().collect();
            ids.sort();
            // 旧实现裸 anyhow 透传到 handler 变纯文本；typed 化后 /tabs 组装点
            // 能 downcast 出稳定码（与本文件 resolve_session 同一判据）。
            return Err(target_select_error(
                "target_ambiguous",
                format!(
                    "profile label {profile:?} 存在歧义，匹配到多个 profile: {}",
                    ids.join(", ")
                ),
                "改用完整 profile_id，或先给 profile 起唯一 label（profile-label set）再选",
            ));
        }
    }
    let mut tabs: Vec<TabInfo> = driver
        .sessions
        .values()
        .filter(|s| s.is_active())
        .filter(|s| {
            browser
                .map(|b| browser_selector_matches(s, b))
                .unwrap_or(true)
        })
        .filter(|s| {
            profile
                .map(|p| s.profile_id == p || s.profile_label.as_deref() == Some(p))
                .unwrap_or(true)
        })
        // 数据模型保留完整 URL：截断只发生在展示层（`tabs` 输出），
        // 否则 snapshot / @e 校验都要额外发一次 location.href 才能拿到真 URL。
        .map(|s| s.info.clone())
        .collect();
    // `--browser Edge` 与 `--browser Edge-Beta` 是子串关系时，旧实现静默把两个
    // 浏览器混在同一个列表里返回。与 profile label 的处理对齐：明确报歧义。
    if let Some(browser) = browser {
        let matched: Vec<&Session> = driver
            .sessions
            .values()
            .filter(|s| s.is_active() && browser_selector_matches(s, browser))
            .filter(|s| {
                profile
                    .map(|p| s.profile_id == p || s.profile_label.as_deref() == Some(p))
                    .unwrap_or(true)
            })
            .collect();
        let candidates = ambiguous_browser_candidates(&matched);
        if candidates.len() > 1 {
            // typed 化：与 resolve_session 同判据，/tabs 组装点 downcast 出稳定码。
            return Err(target_select_error(
                "target_ambiguous",
                browser_ambiguity_message(browser, &candidates),
                "写更精确的 browser 标签或完整 browser_id；browsers 命令可列出全部实例",
            ));
        }
    }
    // 会话存在 HashMap 里，直接收集的顺序每次都可能不同：`tabs` 会乱序，
    // `--tab @1` 也会在不同调用间指向不同标签。统一按 browser → profile → tab id 排。
    sort_tabs_canonical(&mut tabs);
    Ok(tabs)
}

/// `--browser` 命中了多个**不同** `browser_id` 时的候选。
///
/// 返回空 = 不歧义。展示名优先用 label：用户写 `--browser Edge` 时，报错里
/// 必须能看见 `Edge` 与 `Edge-Beta` 才能自己改对，光给 UUID 没有意义。
pub(crate) fn ambiguous_browser_candidates(sessions: &[&Session]) -> Vec<(String, String)> {
    let mut seen: Vec<(String, String)> = Vec::new();
    for session in sessions {
        if seen.iter().any(|(id, _)| id == &session.browser_id) {
            continue;
        }
        let name = session
            .info
            .browser_label
            .clone()
            .unwrap_or_else(|| session.browser_id.clone());
        seen.push((session.browser_id.clone(), name));
    }
    seen.sort();
    seen
}

pub(crate) fn browser_ambiguity_message(selector: &str, candidates: &[(String, String)]) -> String {
    let list = candidates
        .iter()
        .map(|(id, name)| format!("{name}（{id}）"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "--browser {selector:?} 存在歧义，匹配到多个浏览器: {list}；请写更精确的标签或完整 browser_id"
    )
}

pub(crate) async fn extension_profile_capabilities(state: &AppState) -> Vec<Value> {
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
pub(crate) async fn request_extension_reload(
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

pub(crate) async fn has_extension_connection(state: &AppState) -> bool {
    let driver = state.driver.read().await;
    driver
        .sessions
        .values()
        .any(|session| session.info.tab_type == "ext_ws" && session.is_active())
}

pub(crate) async fn wait_for_sessions(state: &AppState, timeout: Duration) -> bool {
    if has_active_sessions(state).await {
        return true;
    }
    // 冷启动时扩展最长要 ~10s 才能重连（退避 0.3→0.6→1.2→2.5→5s），5s 的窗口刚好错过，
    // 于是 daemon 重启后的第一条命令会报「没有可用的浏览器标签页」。
    // 只在「一个会话都没有 + 扩展端口在监听（说明浏览器在那儿）」时把窗口放宽到 12s；
    // 浏览器确实没开时端口不通，仍然按原超时快速失败，不会白等。
    // 只对「动作档」（open/exec/动作，>= WAIT_SESSIONS_ACTION）放宽：
    // 只读档（tabs/lookup 的 800ms）保持快速返回，否则 `atb tabs` 会多等十几秒。
    let timeout = if timeout >= WAIT_SESSIONS_ACTION
        && timeout < EXTENSION_COLD_START_WAIT
        && extension_port_listening(state)
    {
        EXTENSION_COLD_START_WAIT
    } else {
        timeout
    };
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

/// 扩展冷启动（daemon 刚拉起 / SW 刚被回收）时的最大等待窗口。
pub(crate) const EXTENSION_COLD_START_WAIT: Duration = Duration::from_secs(12);

/// 扩展 WebSocket 端口是否有进程在监听。
///
/// 用一个 120ms 的本地连接探测：连得上说明浏览器侧扩展进程还在，只是 WS 尚未重连；
/// 连不上（浏览器没开）时不该让命令白等十几秒。
pub(crate) fn extension_port_listening(state: &AppState) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], state.extension_port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(120)).is_ok()
}

pub(crate) async fn has_active_sessions(state: &AppState) -> bool {
    let driver = state.driver.read().await;
    driver.sessions.values().any(|s| s.is_active())
}

/// 清理长时间断开的会话及其 snapshot 缓存，避免长跑 daemon 里 sessions/snapshots 只增不减。
pub(crate) fn prune_sessions(driver: &mut DriverState) -> Vec<String> {
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
        if driver.preferred_default_session() == Some(key.as_str()) {
            driver.clear_preferred_default_session();
        }
    }
    // 在途执行的清理由调用方用 execs 锁完成（避免两把锁嵌套在同步函数里）。
    stale
}

/// 会话销毁后统一回收它的全部会话级状态。
///
/// **前提**：调用方已确认该会话不会再被使用（标签已关闭 / 被 prune / 连接已断）。
/// 锁序：先 driver（读完即释放），再 session_locks。
/// `open` 之前把调用方「已失效的默认目标」清掉。
///
/// `open` 的目标是 URL 而不是某个标签页，不存在「把命令打到别人标签上」的风险；
/// 但 `select_tab` 对具名 agent 的失效默认会直接报 `target_lost`，于是
/// 「默认标签被关掉之后再也 open 不了」（e2e 实测：cleanup 后 open 本地页直接失败）。
/// 这里只清失效项，不做任何回退决策——回退仍由 `select_tab` 负责。
pub(crate) async fn forget_stale_agent_default(state: &AppState, agent_name: &str) {
    let key = {
        let defaults = state.agent_defaults.lock().await;
        match defaults.get(agent_name).and_then(|t| t.session_key.clone()) {
            Some(key) => key,
            None => return,
        }
    };
    let stale = {
        let driver = state.driver.read().await;
        !driver.sessions.get(&key).is_some_and(Session::is_active)
    };
    if !stale {
        return;
    }
    let mut defaults = state.agent_defaults.lock().await;
    if let Some(target) = defaults.get_mut(agent_name) {
        // 二次确认：判断与清除之间别的请求可能已经把默认换成新的会话。
        if target.session_key.as_deref() == Some(key.as_str()) {
            target.session_key = None;
        }
    }
}

pub(crate) async fn forget_session(state: &AppState, session_key: &str) {
    forget_session_inner(state, session_key, false).await;
}

/// 扩展连接断开时的回收：与 [`forget_session`] 只差**默认目标的记忆**（A2）。
///
/// `session_key` 是确定性拼接（`browser:profile:tabId`），断线重连后同一个 key 会回来。
/// 真实关闭（`close --tab` / 标签关闭事件 / prune）用 `forget_session`，把默认目标彻底忘掉；
/// 只是断线时则把「原来的默认目标」记进 `preferred_default_session_key`：
/// - 断线瞬间 `default_session_key` 照旧清空（`/health` 不会指着一条死会话）；
/// - 重连后同一个 key 出现在快照里，`apply_tabs_snapshot`（`sessions.rs` 里
///   `should_set_default_session` 那段）会把它设回默认并清掉 preferred；
/// - 标签真的被关掉时，那条路径同样会清掉 preferred（`forget_session` / `prune_sessions`）。
pub(crate) async fn forget_session_after_disconnect(state: &AppState, session_key: &str) {
    forget_session_inner(state, session_key, true).await;
}

/// 断线时是否要把「当前默认目标」记进 preferred（A2）。
///
/// 只有「这条会话就是默认」且「还没有别的待生效目标」时才记：
/// 已经有一个 preferred 时说明用户刚 `open` 了另一个标签，那个意图更晚、更明确。
pub(crate) fn remember_default_on_disconnect(driver: &DriverState, session_key: &str) -> bool {
    driver.default_session_key.as_deref() == Some(session_key)
        && driver.preferred_default_session().is_none()
}

async fn forget_session_inner(state: &AppState, session_key: &str, after_disconnect: bool) {
    {
        let mut driver = state.driver.write().await;
        // 必须在清 default **之前**记：断线后它已经不存在了，读不到。
        if after_disconnect && remember_default_on_disconnect(&driver, session_key) {
            driver.set_preferred_default_session(session_key.to_string());
        }
        driver.sessions.remove(session_key);
        driver.snapshots.remove(session_key);
        if driver.default_session_key.as_deref() == Some(session_key) {
            driver.default_session_key = None;
        }
        if driver.latest_session_key.as_deref() == Some(session_key) {
            driver.latest_session_key = None;
        }
        // 断线路径**不清** preferred：它可能就是上面刚记下的「原默认目标」，
        // 要留给重连后的快照消费。真实关闭路径保持原语义（忘干净）。
        if !after_disconnect && driver.preferred_default_session() == Some(session_key) {
            driver.clear_preferred_default_session();
        }
    }
    {
        // 有人还持有这把锁时**不能** remove：`session_key` 是确定性拼接，
        // 同一标签重连后 key 不变，但旧任务仍可能持着旧 Mutex 在注入。此时若删掉，
        // 新任务会通过 `entry().or_insert_with()` 建出第二把锁，同一标签就被并发注入。
        // 判据：登记表是唯一持有者（strong_count == 1）才安全删除。
        let mut locks = state.session_locks.lock().await;
        let exclusively_owned = locks
            .get(session_key)
            .map(|lock| Arc::strong_count(lock) == 1)
            .unwrap_or(false);
        if exclusively_owned {
            locks.remove(session_key);
        }
    }
    state.page_scale.lock().await.remove(session_key);
}

/// 取（或创建）某个会话的串行锁。
pub(crate) async fn session_lock(
    state: &AppState,
    session_key: &str,
) -> Arc<tokio::sync::Mutex<()>> {
    let mut map = state.session_locks.lock().await;
    map.entry(session_key.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// 会话锁表的周期 GC 间隔。
pub(crate) const SESSION_LOCKS_GC_INTERVAL: Duration = Duration::from_secs(300);

/// 回收没人持有的会话锁，返回删除条数。
///
/// `session_key` 是确定性拼接，长跑 daemon 上每开过一批标签就会留下一个 `Arc<Mutex<()>>`；
/// `forget_session` 只在「断开清理能走到」时删，被 prune 掉、被 refresh 重建、
/// 或清理路径提前返回的会话都会留下孤儿锁，只增不减。
///
/// 判据与 `forget_session` 一致：`strong_count == 1` 表示只有登记表持有它，
/// 没有人正在等或正在用——此时删除不会让两个任务各自建出一把锁。
pub(crate) async fn gc_session_locks(state: &AppState) -> usize {
    let mut locks = state.session_locks.lock().await;
    let before = locks.len();
    locks.retain(|_, lock| Arc::strong_count(lock) > 1);
    before - locks.len()
}

pub(crate) async fn select_tab(state: &AppState, selector: SessionSelector) -> Result<String> {
    wait_for_sessions(state, WAIT_SESSIONS_ACTION).await;
    let agent = current_agent();
    // 每个调用方有自己的默认标签页：A 智能体 open 不会改掉 B 的默认目标。
    let agent_target = state.agent_defaults.lock().await.get(&agent).cloned();
    // 不是自己的「待生效会话」不许命中：A open 出来的 preferred 只在
    // preferred_owner == A 时有效，否则 B 的第一条命令会落到 A 刚打开的标签上。
    let agent_target = agent_target.map(|mut target| {
        if target.preferred_key.is_some()
            && target.preferred_owner.as_deref() != Some(agent.as_str())
        {
            target.preferred_key = None;
        }
        target
    });
    // 绝大多数命令都打在同一个标签上，默认会话早已指向它；这种情况下只拿读锁，
    // 不再为了「把同一个值再写一遍」抢排他锁，多个命令就能并发通过选标签这一步。
    let (session_key, needs_write, clear_preferred) = {
        let driver = state.driver.read().await;
        let (key, clear_preferred) = resolve_session(&driver, &selector, agent_target.as_ref())?;
        let current = agent_target
            .as_ref()
            .and_then(|target| target.session_key.as_deref())
            .or(Some(driver.default_session_key.as_deref().unwrap_or("")));
        // 命中「待生效会话」时也必须写一次：`remember_agent_default` 会清掉
        // preferred_key / preferred_owner，而全局 preferred 要在下面一并清空；
        // 不写的话下一条命令还会落回同一个标签。
        let needs_write = current != Some(key.as_str()) || clear_preferred;
        (key, needs_write, clear_preferred)
    };
    if needs_write {
        remember_agent_default(state, &agent, &session_key).await;
        let mut driver = state.driver.write().await;
        if agent == "default" {
            driver.default_session_key = Some(session_key.clone());
        }
        // 命中后无论哪个 agent 都清空：旧实现把清理写在 `agent == "default"`
        // 里面，于是非 default 的 agent 用完这个"待生效会话"后它还在，
        // 下一个没有默认目标的 agent 又会被带到同一个标签，并被写成自己的默认。
        if clear_preferred {
            driver.clear_preferred_default_session();
        }
        drop(driver);
    }
    Ok(session_key)
}

/// 多浏览器并存时的「隐式默认」提示（A2）。
///
/// 只在「本次没有显式指定目标」（没有 --tab / --browser / --profile）**且**当前同时连着
/// ≥2 个浏览器时返回一句话；其它情况返回 `None`。纯说明性字段：不改变选标签结果、
/// 不影响既有字段与退出码。
pub(crate) fn multi_browser_hint(
    driver: &DriverState,
    session_key: &str,
    selector: &SessionSelector,
) -> Option<String> {
    if selector.tab_id.is_some() || selector.browser.is_some() || selector.profile.is_some() {
        return None;
    }
    let mut browsers: HashSet<&str> = HashSet::new();
    for session in driver
        .sessions
        .values()
        .filter(|session| session.is_active())
    {
        browsers.insert(session.browser_id.as_str());
    }
    if browsers.len() < 2 {
        return None;
    }
    let session = driver.sessions.get(session_key)?;
    // 优先用扩展上报的可读标签（`Edge · Windows`），没有就退到 profile label / browser_id。
    let label = session
        .info
        .browser_label
        .clone()
        .or_else(|| session.profile_label.clone())
        .unwrap_or_else(|| session.browser_id.clone());
    Some(format!(
        "当前连接了多个浏览器，隐式默认是 {label}；可用 --browser/--tab 明确指定"
    ))
}

/// 把 [`multi_browser_hint`] 的结果挂到响应 payload 的**顶层**（只加字段，不改既有内容）。
pub(crate) async fn attach_multi_browser_hint(
    state: &AppState,
    payload: &mut Value,
    selector: &SessionSelector,
) {
    let Some(raw) = payload.get("session_key").and_then(Value::as_str) else {
        return;
    };
    let session_key = raw.to_string();
    let hint = {
        let driver = state.driver.read().await;
        multi_browser_hint(&driver, &session_key, selector)
    };
    if let Some(hint) = hint {
        if let Some(object) = payload.as_object_mut() {
            object.insert("multi_browser_hint".to_string(), json!(hint));
        }
    }
}

/// 「本工具打开过哪些标签页」落盘。
///
/// daemon 空闲 5 分钟会退出，内存里的归属记录随之丢失；下次 `cleanup` 就清不到
/// 之前打开的标签页。落盘后即使 daemon 重启，收尾仍然有效（已关闭的会被自动剔除）。
pub(crate) fn persist_agent_opened(state: &AppState) {
    if write_agent_opened(state) {
        return;
    }
    // 撞锁了（`open_many` 6 路并发很常见）：旧实现静默跳过，记录就永久漏写。
    // 现在置 dirty 并在独立任务里补写，不阻塞发起命令的请求。
    AGENT_OPENED_DIRTY.store(true, Ordering::SeqCst);
    schedule_agent_opened_retry(state.clone(), AGENT_OPENED_RETRY_LIMIT);
}

/// 本进程是否还有「内存已更新、磁盘未更新」的记录。
///
/// 只用来在补写路径里判断是否还值得再试；真正的"磁盘是否最新"以写入结果为准。
static AGENT_OPENED_DIRTY: AtomicBool = AtomicBool::new(false);
const AGENT_OPENED_RETRY_DELAY: Duration = Duration::from_millis(50);
const AGENT_OPENED_RETRY_LIMIT: u8 = 3;

fn agent_opened_path() -> Option<PathBuf> {
    config::user_config_dir()
        .ok()
        .map(|dir| dir.join("agent-tabs.json"))
}

/// 把当前表原子地写盘；返回 true 表示"磁盘已是最新（或无需写）"。
fn write_agent_opened(state: &AppState) -> bool {
    let Some(path) = agent_opened_path() else {
        return true;
    };
    // 落盘是尽力而为：拿不到锁就交给重试，绝不能阻塞命令。
    let bytes = match state.agent_opened.try_lock() {
        Ok(map) => match serde_json::to_vec(&*map) {
            Ok(bytes) => bytes,
            Err(err) => {
                eprintln!("agent-tabs: 序列化失败，跳过本次落盘: {err}");
                return true;
            }
        },
        Err(_) => return false,
    };
    // 临时文件 + rename：`std::fs::write` 是先截断再写，进程在中间被杀
    // 就留下半截 JSON。临时名带 pid，避免同一进程内并发写互相截断。
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    if let Err(err) = std::fs::write(&tmp, &bytes) {
        eprintln!("agent-tabs: 写临时文件失败（下次重试）: {err}");
        return false;
    }
    if let Err(err) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("agent-tabs: 原子替换失败（下次重试）: {err}");
        return false;
    }
    AGENT_OPENED_DIRTY.store(false, Ordering::SeqCst);
    true
}

fn schedule_agent_opened_retry(state: AppState, remaining: u8) {
    if remaining == 0 || tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(AGENT_OPENED_RETRY_DELAY).await;
        if !AGENT_OPENED_DIRTY.load(Ordering::SeqCst) {
            return;
        }
        if !write_agent_opened(&state) {
            schedule_agent_opened_retry(state, remaining - 1);
        }
    });
}

pub(crate) fn load_agent_opened() -> HashMap<String, Vec<String>> {
    let Some(path) = agent_opened_path() else {
        return HashMap::new();
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(err) => {
            eprintln!("agent-tabs: 读取失败，本次不恢复历史（不影响启动）: {err}");
            return HashMap::new();
        }
    };
    match serde_json::from_str::<HashMap<String, Vec<String>>>(&text) {
        Ok(map) => map,
        Err(err) => {
            // 旧实现 `unwrap_or_default()` 把损坏当成"空历史"，之后第一条 open
            // 会用空表覆盖整个文件 —— 历史一次性丢光，`cleanup` 从此关不掉之前
            // 打开的标签。现在把原文件改名保留证据，只损失这一次的恢复能力。
            let backup = path.with_extension("json.corrupt");
            // Windows 上 rename 到已存在的目标会失败，先清掉上一份证据。
            let _ = std::fs::remove_file(&backup);
            let moved = std::fs::rename(&path, &backup);
            eprintln!(
                "agent-tabs: {} 解析失败（{err}），已{}，本次按空历史启动",
                path.display(),
                match moved {
                    Ok(()) => format!("改名保留为 {}", backup.display()),
                    Err(move_err) => format!("尝试改名保留失败（{move_err}），请手工检查该文件"),
                }
            );
            HashMap::new()
        }
    }
}

/// 记录某个调用方的默认目标（并清掉它的「待生效」标记）。
pub(crate) async fn remember_agent_default(state: &AppState, agent: &str, session_key: &str) {
    let mut defaults = state.agent_defaults.lock().await;
    let entry = defaults.entry(agent.to_string()).or_default();
    entry.session_key = Some(session_key.to_string());
    entry.preferred_key = None;
    entry.preferred_owner = None;
}

/// `--browser` 的匹配规则。
///
/// 支持三种写法：完整 browser_id（`browser-55312e9f-...`）、id 前缀（≥8 字符，避免误配）、
/// 以及可读标签子串（`Edge` / `工作机`，大小写不敏感）。多浏览器同时连着时，
/// 人只记得住标签，记不住 UUID —— 但标签是「展示层」的东西，不能反过来当主键，
/// 所以仍然按 id 精确匹配优先，标签只做补充。
pub(crate) fn browser_selector_matches(session: &Session, selector: &str) -> bool {
    if session.browser_id == selector {
        return true;
    }
    if selector.len() >= 8 && session.browser_id.starts_with(selector) {
        return true;
    }
    let needle = selector.to_lowercase();
    session
        .info
        .browser_label
        .as_deref()
        .map(|label| label.to_lowercase().contains(&needle))
        .unwrap_or(false)
}

/// 目标选择失败的结构化错误（v8 / 实测）。
///
/// 旧实现用裸 `anyhow!` 拼中文句子，CLI 只能按「未连接」这类子串猜退出码，
/// 文案一改映射就失效；且 `no_tabs` 一个码背了「桥断开 / 真没标签 / 标签属于
/// 另一个 browser / 扩展过旧」四种病因，调用方无法决定重试策略。
/// 现在错误本体带 `code` / `hint` / `retryable` / `retry_after_ms`：
/// - `Display` 只输出自然语言 message，旧调用点的 `err.to_string()` 文本不变（向后兼容）；
/// - 组装点用 [`session_error_json`] downcast 后输出结构化字段，不再发明第二套文本解析。
#[derive(Debug)]
pub(crate) struct SessionSelectError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) hint: String,
    pub(crate) retryable: bool,
    pub(crate) retry_after_ms: Option<u64>,
}

impl std::fmt::Display for SessionSelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SessionSelectError {}

impl SessionSelectError {
    /// 包成 anyhow 穿过既有的 `Result<_>`（anyhow）签名，不改动几十处调用点。
    pub(crate) fn into_anyhow(self) -> anyhow::Error {
        anyhow::Error::new(self)
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut value = json!({
            "ok": false,
            "error": self.message,
            "error_code": self.code,
            "retryable": self.retryable,
        });
        if !self.hint.is_empty() {
            value["hint"] = json!(self.hint);
        }
        if let Some(delay) = self.retry_after_ms {
            value["retry_after_ms"] = json!(delay);
        }
        value
    }
}

/// 组装点统一出口：typed 错误展开成结构化 JSON；其余错误退回纯文本（文本不变）。
///
/// 旧实现各 handler 手写 `json!({"ok":false,"error":err.to_string()})`，typed 信息
/// 在这里被剥掉；统一走本函数后，新增结构化字段不用再逐个 handler 补。
pub(crate) fn session_error_json(err: &anyhow::Error) -> Value {
    if let Some(typed) = err.downcast_ref::<SessionSelectError>() {
        return typed.to_json();
    }
    json!({ "ok": false, "error": err.to_string() })
}

/// 目标类错误（不可重试）的简写。
pub(crate) fn target_select_error(
    code: &'static str,
    message: impl Into<String>,
    hint: impl Into<String>,
) -> anyhow::Error {
    SessionSelectError {
        code,
        message: message.into(),
        hint: hint.into(),
        retryable: false,
        retry_after_ms: None,
    }
    .into_anyhow()
}

/// 桥断开类错误：唯一「等一等就能好」的病因，带 retryable + retry_after_ms（约定 3000ms）。
pub(crate) fn bridge_disconnected_error(message: impl Into<String>) -> anyhow::Error {
    SessionSelectError {
        code: "extension_disconnected",
        message: message.into(),
        hint:
            "扩展桥未连接通常是暂态：确认浏览器已打开后等 3 秒重试；持续失败用 status 检查扩展连接"
                .to_string(),
        retryable: true,
        retry_after_ms: Some(3000),
    }
    .into_anyhow()
}

/// 标签已单独关闭（不是整个浏览器下线）：对同一 tab_id 重试没有意义。
pub(crate) fn tab_closed_error(message: impl Into<String>) -> anyhow::Error {
    SessionSelectError {
        code: "tab_closed",
        message: message.into(),
        hint: "该标签已关闭：重新 open 或用 tabs 另选目标；不要对同一 tab_id 重试".to_string(),
        retryable: false,
        retry_after_ms: None,
    }
    .into_anyhow()
}

/// driver 快照里是否存在「扩展桥活着」的会话。
///
/// 同步版本：调用方都已持有 driver 锁，不能再走拿锁的 `has_extension_connection`。
pub(crate) fn has_live_bridge(driver: &DriverState) -> bool {
    driver
        .sessions
        .values()
        .any(|s| s.info.tab_type == "ext_ws" && s.is_active())
}

/// 统计某个 browser 名下已断开的标签会话数（：跨 browser / 扩展重载后整批失效）。
pub(crate) fn dead_tab_count_of_browser(driver: &DriverState, browser_id: &str) -> usize {
    driver
        .sessions
        .values()
        .filter(|s| s.browser_id == browser_id && !s.is_active())
        .count()
}

/// 纯计算版本的选择逻辑：不修改状态，只返回（会话 ID, 是否需要清掉 preferred）。
pub(crate) fn resolve_session(
    driver: &DriverState,
    selector: &SessionSelector,
    agent: Option<&AgentTarget>,
) -> Result<(String, bool)> {
    // 只用于报错文案（`target_lost` 里要说明是哪个调用方丢了目标）。
    // task-local 未设置时（单测、后台任务）退回 "default"，与 `current_agent` 的语义一致。
    let agent_name = current_agent();
    let mut matches: Vec<&Session> = driver
        .sessions
        .values()
        .filter(|s| s.is_active())
        .filter(|s| {
            selector
                .browser
                .as_deref()
                .map(|b| browser_selector_matches(s, b))
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
            // 旧实现只有一句「会话ID xxx 未连接；当前没有可操作标签页」：跨 browser /
            // 扩展重载后整批 tab_id 失效时，这句把「整个实例下线」说成「目标写错」，
            // 误导排障。现在按全表（含已断开）细分病因，各自给出码与策略。
            let stale_matches: Vec<&Session> = driver
                .sessions
                .values()
                .filter(|s| s.tab_id == tab_id || s.session_key == tab_id)
                .collect();
            if stale_matches.iter().any(|s| s.is_active()) {
                // 会话其实活着，只是被 --browser/--profile 过滤掉了：目标属于另一个 browser。
                // 防：调用方误以为 id 写错去换 id，其实是过滤条件指错了浏览器。
                return Err(target_select_error(
                    "session_mismatch",
                    format!("会话ID {tab_id} 未在当前 --browser/--profile 过滤范围内"),
                    "该标签属于另一个 browser/profile：去掉 --browser/--profile，或改用它所属的 browser（tabs 可看归属）",
                ));
            }
            if !stale_matches.is_empty() {
                // 全表命中但都已断开：整个浏览器实例下线（扩展重载 / 浏览器重启），
                // 名下标签整批失效——不是单个标签被关，也不是 id 写错。
                let browser_id = stale_matches[0].browser_id.as_str();
                let dead = dead_tab_count_of_browser(driver, browser_id);
                return Err(target_select_error(
                    "session_gone",
                    format!(
                        "会话ID {tab_id} 未连接：其所属浏览器实例已下线（扩展重载或浏览器重启），名下 {dead} 个标签失效"
                    ),
                    "不要复用旧 tab_id：重新 open 目标页面，或执行 tabs 后从新列表里取 tab_id",
                ));
            }
            if !has_live_bridge(driver) {
                // 桥断开是唯一可重试的病因：旧文案「先 open 或检查扩展」两可，
                // 现在明确给 retryable + retry_after_ms。
                // message 保留「当前没有可操作标签页」子串：CLI 旧版兜底映射依赖它。
                return Err(bridge_disconnected_error(format!(
                    "会话ID {tab_id} 未连接；当前没有可操作标签页（扩展桥未连接，浏览器可能未打开）"
                )));
            }
            // 桥活着但没有这个 id：就是 id 写错 / 标签已单独关闭。保留原「当前可用」
            // 列表，省一轮 tabs 查询，也避免它自己编一个 id 继续试（原注释继续成立）。
            let mut available: Vec<String> = driver
                .sessions
                .values()
                .filter(|s| s.is_active())
                .map(|s| {
                    let title = truncate_for_tree(&s.info.title, 24);
                    format!("{}({})", s.tab_id, title)
                })
                .collect();
            available.sort();
            available.truncate(12);
            return Err(target_select_error(
                "target_not_found",
                format!(
                    "会话ID {tab_id} 未连接；当前可用: {}。用 `tabs --ids` 查看完整列表，或 --tab @1 按序号选",
                    available.join(", ")
                ),
                "从「当前可用」里挑一个 tab_id，或用 --tab @1 按序号选；若标签刚被关闭，重新 open",
            ));
        }
        if matches.len() > 1 {
            // 旧实现裸 anyhow：CLI 靠文本猜；现在给稳定码 target_ambiguous（改命令类，退出码 2）。
            return Err(target_select_error(
                "target_ambiguous",
                format!("ambiguous tab {tab_id}, matched {} sessions; please specify --profile or --browser", matches.len()),
                "补 --profile 或 --browser 消歧；lookup tab <id> 可先确认归属",
            ));
        }
        return Ok((matches[0].session_key.clone(), false));
    }

    // `--browser` 是前缀/子串判据，`Edge` 会同时命中 `Edge` 与 `Edge-Beta`。
    // 旧实现按 default → latest → connected_at 静默挑一个，用户完全看不出
    // 「我指定的是另一个浏览器」。与 profile label 对齐：命中多个不同 browser_id
    // 就报歧义并列出候选（含 label，才能自己改对）。
    if let Some(browser) = selector.browser.as_deref() {
        let candidates = ambiguous_browser_candidates(&matches);
        if candidates.len() > 1 {
            return Err(target_select_error(
                "target_ambiguous",
                browser_ambiguity_message(browser, &candidates),
                "写更精确的 browser 标签或完整 browser_id；browsers 命令可列出全部实例",
            ));
        }
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
            return Err(target_select_error(
                "target_ambiguous",
                format!(
                    "profile label {profile:?} 存在歧义，匹配到多个 profile: {}",
                    ids.join(", ")
                ),
                "改用完整 profile_id，或先给 profile 起唯一 label（profile-label set）再选",
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
        let session_key = preferred.map(|s| s.session_key.clone()).ok_or_else(|| {
            // 旧实现这里与「真没标签」共用一句「没有可用标签页」，调用方无从分辨
            // 是过滤条件写错还是浏览器空了。细分出 session_mismatch，
            // hint 直接指向 --browser/--profile 的修法。
            target_select_error(
                "session_mismatch",
                "没有匹配 --browser/--profile 的可用浏览器标签页",
                "检查 --browser / --profile 拼写；tabs --ids 可看每个标签的 browser/profile 归属",
            )
        })?;
        return Ok((session_key, false));
    }

    // 1) 调用方自己的默认目标（多智能体隔离的关键）
    if let Some(target) = agent {
        if let Some(key) = target.session_key.clone() {
            if driver.sessions.get(&key).is_some_and(Session::is_active) {
                return Ok((key, false));
            }
            // 专属默认存在但已失效（标签被关 / 浏览器下线 / 被 prune）。
            //
            // 显式声明了身份的 agent **不得**回退到全局 default/latest 或任意 HashMap 项
            // ：回退看起来「更友好」，实际是把 A 智能体的命令打到 B 的标签上：
            // 用户看到的是「我明明在操作 A 的页面，结果改的是 B」，且不会报错。
            // 显式失败 + 一条可执行的恢复路径（重新 open）才是可诊断的行为。
            //
            // 例外是 `default` 桶（裁决）：它是「没有声明身份的调用方」的共享桶，
            // 全局 default/latest 本来就是它自己的目标，回退不构成跨智能体串线；
            // 这里若也报 target_lost，单用户手动关掉默认标签后 `atb read` 会突然失效，
            // 属于破坏性变更。所以只有具名 agent 才享受「宁失败不串线」的严格语义。
            if agent_name != "default" {
                return Err(target_select_error(
                    "target_lost",
                    format!("调用方 {agent_name} 的默认标签页已失效（{key}）：标签已关闭或所属浏览器已下线"),
                    "重新 open 目标页面（会重建默认目标），或用 --tab/--browser 显式指定目标；不要依赖自动回退",
                ));
            }
        }
        if let Some(preferred) = target.preferred_key.clone() {
            if driver
                .sessions
                .get(&preferred)
                .is_some_and(Session::is_active)
            {
                return Ok((preferred, true));
            }
        }
    }

    // 优先使用“刚刚打开、尚未完成注册”的待生效会话，避免 open 后第一条命令打错标签页。
    if let Some(preferred) = driver.preferred_default_session().map(str::to_string) {
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
        .ok_or_else(|| {
            // 旧实现固定说「没有可用的浏览器标签页」，四种病因一个码。
            // 现在按快照细分：桥活着 = 真没标签（open 即解）；桥断开 = 唯一可重试病因，
            // 给 retryable + retry_after_ms。两条 message 都保留旧子串
            // 「没有可用的浏览器标签页」，CLI 旧版兜底映射不受影响。
            if has_live_bridge(driver) {
                target_select_error(
                    "no_target_tabs",
                    "没有可用的浏览器标签页（扩展已连接，但没有存活的标签页会话）",
                    "先打开一个页面：`atb open https://example.com`",
                )
            } else {
                bridge_disconnected_error(
                    "没有可用的浏览器标签页（扩展桥未连接，浏览器可能未打开）",
                )
            }
        })?;
    Ok((session_key, false))
}
