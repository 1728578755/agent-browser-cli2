use super::*;
#[allow(unused_imports)]
use clap::{Args, Parser, Subcommand};

pub(crate) fn home_dir() -> Result<PathBuf> {
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
pub(crate) fn ws_trust_path() -> Result<PathBuf> {
    Ok(config::user_config_dir()?.join("ws-trust.json"))
}

/// Chrome 扩展 ID 的字母表是 a-p，固定 32 位；这里大小写不敏感并归一成小写。
pub(crate) fn normalize_extension_id(raw: &str) -> Result<String> {
    let id = raw.trim().to_ascii_lowercase();
    if id.len() == 32 && id.bytes().all(|byte| matches!(byte, b'a'..=b'p')) {
        return Ok(id);
    }
    Err(usage_err!(
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
pub(crate) fn load_ws_trust_json_inner(path: &Path, allow_empty: bool) -> Result<Value> {
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
        return Err(usage_err!(
            "信任配置为空（{}）：空文件不会被当成「未配置」，daemon 会拒绝所有 WebSocket 连接。\
             要回到兼容模式请删除该文件；要重新配置可运行 atb trust-extension <id>（会直接覆盖）",
            path.display()
        ));
    }
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("信任配置不是合法 JSON: {}", path.display()))?;
    if !value.is_object() {
        return Err(usage_err!(
            "信任配置必须是 JSON 对象（形如 {{\"extension_ids\": [...], \"token\": \"...\"}}）: {}",
            path.display()
        ));
    }
    Ok(value)
}

pub(crate) fn load_ws_trust_json(path: &Path) -> Result<Value> {
    load_ws_trust_json_inner(path, false)
}

pub(crate) fn load_ws_trust_json_for_update(path: &Path) -> Result<Value> {
    load_ws_trust_json_inner(path, true)
}

/// 先写同目录临时文件再原子替换：daemon 可能随时在读，不能让它读到半个 JSON。
///
/// 复用 `config::replace_file_atomically`（Windows 上是「备份 → 替换 → 删备份」，
/// 替换失败会把备份放回），**不能改成先删目标再 rename**：一旦中间失败/进程被杀，
/// `ws-trust.json` 就会消失，daemon 把 `NotFound` 当成「从没配过」→ 回到兼容模式，
/// 用户以为配好的白名单静默失效（fail-open）。
pub(crate) fn save_ws_trust_json(path: &Path, value: &Value) -> Result<()> {
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
pub(crate) fn trust_ids_of(value: &Value) -> Vec<String> {
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
pub(crate) fn trust_token_of(value: &Value) -> Option<String> {
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
pub(crate) enum TrustReload {
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
pub(crate) fn reload_daemon_trust() -> TrustReload {
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

pub(crate) fn print_trust_state(
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
                 确认 daemon 状态（atb status）后重试，或运行 restart 确保生效"
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

pub(crate) fn run_trust_extension(args: TrustExtensionArgs) -> Result<()> {
    let path = ws_trust_path()?;
    let other_than_list = args.extension_id.is_some()
        || args.remove
        || args.clear
        || args.token.is_some()
        || args.clear_token;
    let other_than_clear =
        args.extension_id.is_some() || args.remove || args.token.is_some() || args.clear_token;
    // `trust-extension --dry-run` 以前会真的改写 ws-trust.json。
    if DRY_RUN.load(Ordering::Relaxed) {
        // 复核（/7）：dry-run 也必须做本地参数校验，否则「计划 ok:true」之后
        // 去掉 --dry-run 才发现 ID 不合法 / --remove 少了 ID，等于假绿。
        let extension_id = match args.extension_id.as_deref() {
            Some(raw) => Some(normalize_extension_id(raw)?),
            None => None,
        };
        if args.remove && extension_id.is_none() {
            return Err(usage_err!(
                "--remove 需要同时给出扩展 ID：atb trust-extension <id> --remove"
            ));
        }
        if args.clear_token && args.token.is_some() {
            return Err(usage_err!("--token 和 --clear-token 不能同时使用"));
        }
        let action = if args.list {
            "list"
        } else if args.clear {
            "clear"
        } else if args.remove {
            "remove"
        } else if args.clear_token {
            "clear-token"
        } else if args.token.is_some() {
            "set-token"
        } else {
            "add"
        };
        print_json(json!({
            "ok": true,
            "dry_run": true,
            "op": "trust-extension",
            "action": action,
            "path": path,
            "extension_id": extension_id,
            "hint": "去掉 --dry-run 才会写盘；写盘后会对不可信连接触发一次重载",
        }));
        return Ok(());
    }
    if args.list {
        if other_than_list {
            return Err(usage_err!(
                "--list 只读，不能和 <id> / --remove / --clear / --token / --clear-token 同时使用"
            ));
        }
        let value = load_ws_trust_json(&path)?;
        return print_trust_state(&path, &value, "list", false, &TrustReload::Skipped);
    }
    if args.clear {
        if other_than_clear {
            return Err(usage_err!(
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
        return Err(usage_err!(
            "--remove 需要同时给出扩展 ID：atb trust-extension <id> --remove"
        ));
    }
    if args.clear_token && args.token.is_some() {
        return Err(usage_err!("--token 和 --clear-token 不能同时使用"));
    }
    if args.extension_id.is_none() && args.token.is_none() && !args.clear_token {
        return Err(usage_err!(
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
            return Err(usage_err!(
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
