use super::*;

/// 当前标签所属站点的 content-script 匹配模式：`*://host/*`。
///
/// 只在没有显式 `--match` 时用来收窄持久化注入，避免 `net hook --persist`
/// 把脚本注册到所有网站。
pub(crate) async fn current_site_match_pattern(
    state: &AppState,
    session_key: &str,
) -> Option<String> {
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
/// 以前手写切分有两个 bug：`https://example.com?q=1` 会把 query 当成 host，
/// 生成非法 pattern；`host.starts_with("chrom")` 又会误杀 `chromium.org`。
/// 现在交给 `url` crate 解析，过滤条件改为按 scheme 判断。
pub(crate) fn site_match_pattern(raw_url: &str) -> Option<String> {
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
pub(crate) async fn net_profile_key(state: &AppState, session_key: &str) -> Result<String> {
    let driver = state.driver.read().await;
    let session = driver
        .sessions
        .get(session_key)
        .ok_or_else(|| anyhow!("会话ID {session_key} 未连接"))?;
    Ok(format!("{}:{}", session.browser_id, session.profile_id))
}

pub(crate) fn net_config_default() -> Value {
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
/// **前提**：调用方已持有 `state.net_persist_lock`（读-改-写串行化）。
/// 这里不再自己拿锁：`net_value` 在整个「读 → 下发 → 写」期间持锁，若这里再锁一次
/// 就是自死锁。落盘只带当前 profile 的条目，其余 profile 由后台线程从磁盘读回后合并：
/// 旧实现克隆**所有** profile（mock body 可达 2MB）再同步写盘，全部发生在
/// async worker 上，配置一多就会卡住整个 runtime。
pub(crate) async fn store_net_config_locked(state: &AppState, profile_key: &str, config: &Value) {
    {
        let mut driver = state.driver.write().await;
        driver
            .net_hooks
            .insert(profile_key.to_string(), config.clone());
    }
    let entry = config.clone();
    let key = profile_key.to_string();
    // 读-改-写必须串行：两个 profile 并发保存时，后写的那份不能把先写的覆盖掉。
    // （同一个 profile 由调用方的 net_persist_lock 串行，其它 profile 由这里的进程内
    // 单一后台任务串行；两侧都遵守锁序，不会互相覆盖。）
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
///
/// **前提**：调用方已持有 `state.net_persist_lock`（同 `store_net_config_locked`）。
pub(crate) async fn remove_net_config_locked(state: &AppState, profile_key: &str) {
    {
        let mut driver = state.driver.write().await;
        driver.net_hooks.remove(profile_key);
    }
    let key = profile_key.to_string();
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
