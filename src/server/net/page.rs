use super::*;

pub(crate) async fn net_page_call(
    state: &AppState,
    session_key: &str,
    opts: &Value,
    timeout: Duration,
    frame_id: Option<i64>,
) -> Result<Value> {
    let script = format!("{}\nreturn __abcNet({});", html::js_page_net(), opts);
    let target = ExecTargetOptions {
        frame_id,
        ..Default::default()
    };
    let executed = execute_page_js_with_target(
        state,
        &script,
        SessionSelector::new(Some(session_key.to_string()), None, None),
        true,
        timeout,
        &target,
    )
    .await;
    match executed {
        Ok(result) => Ok(page_payload(result, "net")
            .get("net")
            .cloned()
            .unwrap_or(Value::Null)),
        Err(err) if is_csp_error(&err.to_string()) => {
            page_op_via_extension(state, session_key, "net", opts.clone(), frame_id, timeout).await
        }
        Err(err) => Err(err),
    }
}

/// 给「整个页面往返」再套一层总超时（的"别在持锁时无限等待"）。
///
/// `execute_page_js` 的 timeout 只包住「最后等回包」（），前面等会话锁
/// 没有超时：后台 job / 长 `exec`（hold_lock=true）正持锁时，net 命令会一直等，
/// 而 `net_value` 此时握着 `net_persist_lock`，后续 net 命令全部被堵。
/// 这里把整段限制在 timeout 内，超时如实报错（不伪装成功）。
pub(crate) async fn net_page_call_bounded(
    state: &AppState,
    session_key: &str,
    opts: &Value,
    timeout: Duration,
    frame_id: Option<i64>,
) -> Result<Value> {
    match tokio::time::timeout(
        timeout,
        net_page_call(state, session_key, opts, timeout, frame_id),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(anyhow!(
            "net: 页面往返超时（{}s，含等待会话锁）；可能有长命令正占用该标签",
            timeout.as_secs()
        )),
    }
}

/// 把页面级 net 动作下发到该 profile 下的**每个**活跃标签。
///
/// 当前标签的结果作为权威 payload 返回（它的报错最有诊断价值）；其余标签只收集
/// 失败：某一个标签连不上不该把整条命令判失败，但必须出现在 `page_failed` 里。
/// 返回 `Err` 仅当当前标签本身失败。
pub(crate) async fn net_page_broadcast(
    state: &AppState,
    profile_key: &str,
    current: &str,
    opts: &Value,
    timeout: Duration,
    frame_id: Option<i64>,
) -> Result<(Value, Vec<Value>)> {
    let payload = net_page_call_bounded(state, current, opts, timeout, frame_id).await?;
    let others: Vec<String> = profile_sessions(state, profile_key)
        .await
        .into_iter()
        .filter(|key| key != current)
        .collect();
    if others.is_empty() {
        return Ok((payload, Vec::new()));
    }
    // 页面脚本按标签隔离，并发下发互不干扰；串行 N 个标签会把 N×(最长 timeout)
    // 叠加到同一个请求上，CLI 侧会先超时。
    let deadline = tokio::time::Instant::now() + timeout;
    let mut pending: HashSet<String> = others.iter().cloned().collect();
    let mut set = tokio::task::JoinSet::new();
    for key in others {
        let task_state = state.clone();
        let opts = opts.clone();
        set.spawn(async move {
            let tab_id = tab_id_of_session(&key);
            let outcome = net_page_call_bounded(&task_state, &key, &opts, timeout, frame_id).await;
            (key, tab_id, outcome)
        });
    }
    let mut failures: Vec<Value> = Vec::new();
    loop {
        let joined = match tokio::time::timeout_at(deadline, set.join_next()).await {
            Ok(Some(joined)) => joined,
            Ok(None) | Err(_) => break,
        };
        match joined {
            Ok((key, _tab_id, Ok(_))) => {
                pending.remove(&key);
            }
            Ok((key, tab_id, Err(err))) => {
                pending.remove(&key);
                failures.push(json!({
                    "tab_id": tab_id,
                    "session_key": key,
                    "error": humanize_extension_error(&err.to_string()),
                }));
            }
            // panic 的任务拿不到 key，留给下面的"未完成"兜底。
            Err(err) => eprintln!("net: 页面规则下发任务异常退出: {err}"),
        }
    }
    // 预算用完后被 abort 的标签：规则状态未知，按失败如实上报，别让"没回来"
    // 看起来像"已经生效"。
    let mut leftovers: Vec<String> = pending.into_iter().collect();
    leftovers.sort();
    for key in leftovers {
        failures.push(json!({
            "tab_id": tab_id_of_session(&key),
            "session_key": key,
            "error": format!("timeout: {}s 内未收到该标签的确认", timeout.as_secs()),
        }));
    }
    Ok((payload, failures))
}
