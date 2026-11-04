use super::*;
#[allow(unused_imports)]
use clap::{Args, Parser, Subcommand};

#[derive(Debug)]
pub(crate) struct SkillPlanItem {
    pub(crate) path: PathBuf,
    pub(crate) action: String,
    pub(crate) will_write: bool,
    pub(crate) note: String,
}

pub(crate) fn install_skill(args: InstallSkillArgs) -> Result<()> {
    // 全局 `--dry-run` 与命令自带的 `--dry-run` 等价：以前全局 flag 被
    // 静默忽略，`atb --dry-run install-skill` 会真的拷贝目录、重建软链接。
    let dry = args.dry_run || DRY_RUN.load(Ordering::Relaxed);
    let source = resolve_skill_source()?;
    let home = home_dir()?;
    let main_target = home.join(".agents/skills/atb");
    let mut plan = Vec::new();
    plan.push(SkillPlanItem {
        path: main_target.clone(),
        action: if main_target.exists() {
            "update_main"
        } else {
            "create_main"
        }
        .to_string(),
        will_write: true,
        note: format!("复制内置 skill 目录: {}", source.display()),
    });

    for parent in [
        home.join(".codex/skills"),
        home.join(".claude/skills"),
        home.join(".config/agents/skills"),
        home.join(".cursor/skills"),
        home.join(".gemini/skills"),
    ] {
        let target = parent.join("atb");
        if !parent.exists() {
            plan.push(SkillPlanItem {
                path: target,
                action: "skip_missing_parent".to_string(),
                will_write: false,
                note: "父目录不存在，跳过软链接".to_string(),
            });
            continue;
        }
        let meta = fs::symlink_metadata(&target).ok();
        if let Some(meta) = meta {
            if meta.file_type().is_symlink() {
                plan.push(SkillPlanItem {
                    path: target,
                    action: "update_symlink".to_string(),
                    will_write: true,
                    note: format!("更新软链接到 {}", main_target.display()),
                });
            } else {
                plan.push(SkillPlanItem {
                    path: target,
                    action: "skip_existing_entity".to_string(),
                    will_write: false,
                    note: "目标已存在且不是软链接，必须手动处理，--yes 也不会覆盖".to_string(),
                });
            }
        } else {
            plan.push(SkillPlanItem {
                path: target,
                action: "create_symlink".to_string(),
                will_write: true,
                note: format!("创建软链接到 {}", main_target.display()),
            });
        }
    }

    print_skill_plan(&plan, dry);
    if dry {
        // 既有文本计划保留（人读），另外补一份机器可读结果：只增字段。
        let items: Vec<Value> = plan
            .iter()
            .map(|item| {
                json!({
                    "action": item.action,
                    "path": item.path.display().to_string(),
                    "will_write": item.will_write,
                    "note": item.note,
                })
            })
            .collect();
        let would_write = plan.iter().filter(|item| item.will_write).count();
        print_json(json!({
            "ok": true,
            "result": {
                "dry_run": true,
                "command": "install-skill",
                "source": source.display().to_string(),
                "target": main_target.display().to_string(),
                "would_write": would_write,
                "plan": items,
            },
        }));
        return Ok(());
    }
    if !args.yes && !confirm_install()? {
        write_stdout("已取消安装");
        return Ok(());
    }

    copy_dir_clean(&source, &main_target)?;
    for item in plan
        .iter()
        .filter(|item| item.action.ends_with("symlink") && item.will_write)
    {
        if let Some(parent) = item.path.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::symlink_metadata(&item.path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            remove_path(&item.path)?;
        }
        create_symlink_dir(&main_target, &item.path)?;
    }
    write_stdout(&format!(
        "安装完成: {}",
        main_target.join("SKILL.md").display()
    ));
    Ok(())
}

pub(crate) fn print_skill_plan(plan: &[SkillPlanItem], dry_run: bool) {
    write_stdout("atb skill 安装计划");
    if dry_run {
        write_stdout("模式: dry-run，不写文件、不创建软链接");
    }
    for item in plan {
        write_stdout(&format!(
            "- [{}] {} -> {} ({})",
            if item.will_write { "write" } else { "skip" },
            item.action,
            item.path.display(),
            item.note
        ));
    }
}

pub(crate) fn confirm_install() -> Result<bool> {
    // 裸 `print!` 在 `| head -0` 这类管道里会 EPIPE panic（同类）。
    {
        let stdout = io::stdout();
        let mut lock = stdout.lock();
        let _ = write!(lock, "确认执行以上安装计划？输入 yes 继续: ");
        let _ = lock.flush();
    }
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim() == "yes")
}

pub(crate) fn resolve_skill_source() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(package_dir) = config::env_compat("PACKAGE_DIR") {
        candidates.push(PathBuf::from(package_dir).join("skills/atb"));
    }
    if let Ok(exe) = env::current_exe() {
        for ancestor in exe.ancestors().take(6) {
            candidates.push(ancestor.join("skills/atb"));
        }
    }
    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd.join("skills/atb"));
    }
    for candidate in candidates {
        if candidate.join("SKILL.md").is_file() {
            return Ok(candidate);
        }
    }
    Err(usage_err!("找不到内置 skill 目录 skills/atb"))
}

pub(crate) fn copy_dir_clean(source: &Path, target: &Path) -> Result<()> {
    if fs::symlink_metadata(target)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(usage_err!("主安装目录不能是软链接: {}", target.display()));
    }
    // 先拷到同级临时目录，成功后再 rename 换上去：中途失败时旧目录仍然完整，
    // 不会留下「目录已被清空、新文件还没拷完」的半成品。
    let tmp = target.with_file_name(format!(
        ".{}.tmp-{}",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("atb"),
        std::process::id()
    ));
    if fs::symlink_metadata(&tmp).is_ok() {
        fs::remove_dir_all(&tmp).with_context(|| format!("清理临时目录失败: {}", tmp.display()))?;
    }
    let copied = (|| -> Result<()> {
        fs::create_dir_all(&tmp).with_context(|| format!("创建临时目录失败: {}", tmp.display()))?;
        copy_dir_contents(source, &tmp)
    })();
    if let Err(err) = copied {
        let _ = fs::remove_dir_all(&tmp);
        return Err(err);
    }
    // 旧目录先改名成 `.bak-<ts>`，新目录就位后再删备份。
    // 旧实现是「先 remove 再 rename」：一旦 rename 失败（杀毒软件占着文件、跨设备、
    // 权限不足），安装目录就彻底不存在了——技能文件全丢，且没有回滚路径。
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("atb")
        .to_string();
    let backup = if fs::symlink_metadata(target).is_ok() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0);
        let backup = target.with_file_name(format!(".{name}.bak-{stamp}"));
        if fs::symlink_metadata(&backup).is_ok() {
            let _ = fs::remove_dir_all(&backup);
        }
        fs::rename(target, &backup)
            .with_context(|| format!("备份旧安装目录失败: {}", target.display()))?;
        Some(backup)
    } else {
        None
    };
    if let Err(err) = fs::rename(&tmp, target) {
        // 回滚：把旧目录放回原位，保证安装目录至少和安装前一致。
        if let Some(backup) = &backup {
            let _ = fs::rename(backup, target);
        }
        return Err(err).with_context(|| {
            format!(
                "替换主安装目录失败: {}（新内容已保留在 {}，旧目录已回滚）",
                target.display(),
                tmp.display()
            )
        });
    }
    if let Some(backup) = backup {
        // 新目录已就位：备份删不掉只影响磁盘占用，不应把「安装成功」报成失败。
        // 删不掉时保留 .bak 目录，用户手工清理即可。
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(())
}

/// 递归复制目录内容（不做清理），供 `copy_dir_clean` 往临时目录里写。
pub(crate) fn copy_dir_contents(source: &Path, target: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let src = entry.path();
        let dst = target.join(entry.file_name());
        let meta = entry.file_type()?;
        if meta.is_dir() {
            fs::create_dir_all(&dst).with_context(|| format!("创建目录失败: {}", dst.display()))?;
            copy_dir_contents(&src, &dst)?;
        } else if meta.is_file() {
            fs::copy(&src, &dst).with_context(|| format!("复制文件失败: {}", src.display()))?;
        }
    }
    Ok(())
}

pub(crate) fn remove_path(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        // 指向目录的软链在 Windows 上必须走 RemoveDirectory：用 remove_file 会报
        // 「拒绝访问 (os error 5)」。以前 install-skill 更新已存在的软链必然失败，
        // 计划里的 update_symlink 一项永远跑不完（主目录已更新、命令却返回非 0）。
        fs::remove_file(path)
            .or_else(|_| fs::remove_dir(path))
            .with_context(|| format!("删除软链接失败: {}", path.display()))?;
    } else if meta.is_file() {
        fs::remove_file(path)?;
    } else if meta.is_dir() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn create_symlink_dir(source: &Path, target: &Path) -> Result<()> {
    std::os::unix::fs::symlink(source, target)
        .with_context(|| format!("创建软链接失败: {}", target.display()))
}

#[cfg(windows)]
pub(crate) fn create_symlink_dir(source: &Path, target: &Path) -> Result<()> {
    std::os::windows::fs::symlink_dir(source, target)
        .with_context(|| format!("创建软链接失败: {}", target.display()))
}

pub(crate) fn run_broadcast(args: BroadcastArgs) -> Result<()> {
    let mut payload_args = match args.args_json.as_deref() {
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) => value,
            // 与 net --body 同一口径：非法 JSON 是用法错（退出码 2），
            // 以前是 anyhow → 退出码 1，调用方会当成可重试。
            Err(err) => {
                return Err(usage_failure(
                    "invalid_json",
                    format!(
                        "--args 需要合法 JSON（第 {} 行第 {} 列）: {}",
                        err.line(),
                        err.column(),
                        err
                    ),
                    "--args 需要是 JSON 对象，例如 '{\"max_chars\":2000}'；脚本内容请用 --script/--file",
                ));
            }
        },
        None => json!({}),
    };
    if !payload_args.is_object() {
        return Err(usage_failure(
            "invalid_json",
            "--args 需要是 JSON 对象",
            "例如 '{\"max_chars\":2000}'",
        ));
    }
    let script = match args.file.as_ref() {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("读取脚本文件失败: {}", path.display()))?,
        ),
        None => args.script.clone(),
    };
    if let Some(script) = script {
        payload_args["script"] = json!(script);
    }
    if args.op == "exec" && payload_args.get("script").is_none() {
        return Err(usage_err!("broadcast exec: 需要脚本（--script 或 --file）"));
    }
    // 下发给所有浏览器/Profile 之前先在本地把参数查一遍：以前一个
    // `--script ''` 会被广播到全部目标，每个都失败一次才回来。
    // preflight_payload 对不支持的动作直接放行（只查通用数值字段），keep daemon 兜底。
    if let Err(problem) = preflight::preflight_payload(&args.op, &payload_args) {
        return usage_error(&problem);
    }
    let payload = json!({
        "op": args.op,
        "args": payload_args,
        "browsers": args.browsers,
        "profiles": args.profiles,
        "concurrency": args.concurrency,
    });
    ensure_server()?;
    let value = request("POST", "/broadcast", Some(payload), args.timeout)?;
    print_json(value);
    Ok(())
}

pub(crate) fn run_downloads(args: DownloadsArgs) -> Result<()> {
    ensure_server()?;
    let mut payload = json!({
        "action": "list",
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
    });
    if let Some(limit) = args.limit {
        payload["limit"] = json!(limit);
    }
    if let Some(state) = args.state {
        payload["state"] = json!(state);
    }
    if let Some(id) = args.id {
        payload["id"] = json!(id);
    }
    print_json(request("POST", "/downloads", Some(payload), args.timeout)?);
    Ok(())
}

/// `download` 接受的下载目标前缀：只有这四类能被扩展直接抓取/落盘。
///
/// 其余输入（`list`、`foo.png`、`file://…`）在 daemon 侧只会变成一句
/// `Invalid URL`，调用方看不出该改用哪条命令，所以提前在 CLI 侧拦下（退出码 2）。
fn is_download_target(target: &str) -> bool {
    const SCHEMES: [&str; 4] = ["http://", "https://", "data:", "blob:"];
    let lower = target.to_ascii_lowercase();
    SCHEMES.iter().any(|scheme| lower.starts_with(scheme))
}

/// 子命令名被当成位置参数时的替代命令。
///
/// `atb download list` / `... wait` 是实测最常见的两种误用：脑子里想的是
/// `downloads` 族命令，手上却敲在 `download` 后面。
fn download_suggestion(target: &str) -> Option<&'static str> {
    match target {
        "wait" => Some("atb download-wait"),
        "list" | "get" | "cancel" | "cancel-all" => Some("atb downloads"),
        _ => None,
    }
}

pub(crate) fn run_download(args: DownloadArgs) -> Result<()> {
    // 预检：target 必须是可下载链接。放在 ensure_server 之前——命令本身写错了，
    // 没必要先把 daemon 拉起来（也避免误导成「服务端拒绝了」）。
    let url = args.url.trim().to_string();
    if !is_download_target(&url) {
        let message = format!(
            "download 需要一个 http(s) 可下载链接（收到 \"{url}\"）；查看下载列表请用 atb downloads"
        );
        let hint = "URL 需以 http:// / https:// / data: / blob: 开头；\
                    查下载列表用 atb downloads，等下载完成用 atb download-wait";
        return Err(match download_suggestion(&url) {
            Some(suggestion) => {
                usage_failure_with_suggestion("invalid_argument", message, hint, suggestion)
            }
            None => usage_failure("invalid_argument", message, hint),
        });
    }
    ensure_server()?;
    let mut payload = json!({
        "action": "start",
        "url": url,
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
    });
    if let Some(filename) = args.filename {
        payload["filename"] = json!(filename);
    }
    if let Some(dir) = args.dir {
        // 相对目录以当前 shell 的 cwd 为基准（daemon 的 cwd 是 home，会把文件下到别处）。
        payload["dir"] = json!(absolutize_path(&dir));
    }
    print_json(request("POST", "/downloads", Some(payload), args.timeout)?);
    Ok(())
}

pub(crate) fn run_download_wait(args: DownloadWaitArgs) -> Result<()> {
    // 区间与服务端 `downloads.rs` 的 clamp 保持一致（1~3600s / 0.05~5s）：服务端越界是
    // **静默截断**且不回包，等于悄悄改掉用户给的参数（0.5s 变 1s、0.001s 变 0.05s）。
    // 提前拒绝并写清合法区间，比让它默默变成另一个值更符合契约。
    if !args.wait_timeout.is_finite() || !(1.0..=3600.0).contains(&args.wait_timeout) {
        return Err(usage_failure(
            "invalid_argument",
            "--timeout 必须在 1~3600 秒之间（服务端会区间截断，这里提前报错）",
            "默认 120 秒；按文件大小和带宽给一个覆盖得住的值",
        ));
    }
    if !args.interval.is_finite() || !(0.05..=5.0).contains(&args.interval) {
        return Err(usage_failure(
            "invalid_argument",
            "--interval 必须在 0.05~5 秒之间（服务端会区间截断，这里提前报错）",
            "默认 0.5 秒；调小会增加 daemon 的轮询次数",
        ));
    }
    ensure_server()?;
    let payload = json!({
        "action": "wait",
        "id": args.id,
        "wait_timeout": args.wait_timeout,
        "interval": args.interval,
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
    });
    let value = request(
        "POST",
        "/downloads",
        Some(payload),
        args.wait_timeout + 15.0,
    )?;
    let ok = value
        .get("result")
        .and_then(|result| result.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // 只有真正的「等到超时」才是退出码 3。`interrupted`（用户取消/页面关闭）与
    // 扩展未连接之类的硬失败都是 1——以前一律改写 3，调用方会误判成「再等等就好」。
    let state = value
        .get("result")
        .and_then(|result| result.get("state"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    print_json(value);
    if !ok {
        // 不要 std::process::exit：那会绕过 run 末尾的 AGENT_BROWSER_CLI_EXIT0 兼容开关。
        let code = if state == "timeout" {
            EXIT_JOB_TIMEOUT
        } else {
            EXIT_FAILURE
        };
        FAILURE_EXIT_CODE.store(code, Ordering::Relaxed);
    }
    Ok(())
}

pub(crate) fn run_target_command(
    path: &str,
    op: &str,
    args: TargetCommandArgs,
    is_async: bool,
) -> Result<()> {
    ensure_server()?;
    let payload = json!({
        "target": args.target,
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "monitor": args.monitor,
        "wait_js": args.wait_js,
        "wait_timeout": args.wait_timeout,
        "wait_interval": args.wait_interval,
        "timeout": args.timeout,
        "frame_id": args.frame,
    });
    if is_async {
        print_json(submit_job(op, payload)?);
    } else {
        dispatch_page(
            op,
            path,
            payload,
            args.tab.as_deref(),
            args.browser.as_deref(),
            args.profile.as_deref(),
            args.timeout + args.wait_timeout + 5.0,
        )?;
    }
    Ok(())
}

pub(crate) fn run_fill_command(args: FillCommandArgs, is_async: bool) -> Result<()> {
    ensure_server()?;
    let has_value = args.value.is_some();
    let value = args.value.unwrap_or_default();
    let payload = json!({
        "target": args.target,
        "value": value,
        "has_value": has_value,
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "append": args.append,
        "clear": args.clear,
        "monitor": args.monitor,
        "wait_js": args.wait_js,
        "wait_timeout": args.wait_timeout,
        "wait_interval": args.wait_interval,
        "timeout": args.timeout,
        "frame_id": args.frame,
    });
    if is_async {
        print_json(submit_job("fill", payload)?);
    } else {
        dispatch_page(
            "fill",
            "/fill",
            payload,
            args.tab.as_deref(),
            args.browser.as_deref(),
            args.profile.as_deref(),
            args.timeout + args.wait_timeout + 5.0,
        )?;
    }
    Ok(())
}

pub(crate) fn run_send_keys_command(args: SendKeysCommandArgs, is_async: bool) -> Result<()> {
    ensure_server()?;
    let payload = json!({
        "keys": args.keys,
        "frame_id": args.frame,
        "target": args.target,
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "monitor": args.monitor,
        "wait_js": args.wait_js,
        "wait_timeout": args.wait_timeout,
        "wait_interval": args.wait_interval,
        "timeout": args.timeout,
    });
    if is_async {
        print_json(submit_job("send-keys", payload)?);
    } else {
        dispatch_page(
            "send-keys",
            "/send-keys",
            payload,
            args.tab.as_deref(),
            args.browser.as_deref(),
            args.profile.as_deref(),
            args.timeout + args.wait_timeout + 5.0,
        )?;
    }
    Ok(())
}

pub(crate) fn run_screenshot_command(args: ScreenshotArgs, is_async: bool) -> Result<()> {
    ensure_server()?;
    let payload = json!({
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "target": args.target,
        "selector": args.selector,
        "out": args.out,
        "format": args.format,
        "quality": args.quality,
        "full_page": args.full_page,
        "max_height_px": args.max_height,
        "timeout": args.timeout,
    });
    if is_async {
        print_json(submit_job("screenshot", payload)?);
    } else {
        dispatch_page(
            "screenshot",
            "/screenshot",
            payload,
            args.tab.as_deref(),
            args.browser.as_deref(),
            args.profile.as_deref(),
            args.timeout + 5.0,
        )?;
    }
    Ok(())
}

pub(crate) fn run_save_pdf_command(args: SavePdfArgs, is_async: bool) -> Result<()> {
    ensure_server()?;
    let payload = json!({
        "switch_tab_id": args.tab,
        "browser": args.browser,
        "profile": args.profile,
        "out": args.out,
        "paper": args.paper,
        "landscape": args.landscape,
        "scale": args.scale,
        "print_background": !args.no_print_background,
        "max_height_px": args.max_height,
        "timeout": args.timeout,
    });
    if is_async {
        print_json(submit_job("save-pdf", payload)?);
    } else {
        dispatch_page(
            "save-pdf",
            "/save-pdf",
            payload,
            args.tab.as_deref(),
            args.browser.as_deref(),
            args.profile.as_deref(),
            args.timeout + 5.0,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 复现「Windows 上更新已有软链接会失败」：指向目录的软链必须能被 remove_path 删掉，
    // 且不能连带删掉目标目录。没建链权限（Windows 未开开发者模式 / 非管理员）时跳过，
    // 不让环境差异变成假失败。
    #[test]
    fn remove_path_deletes_dir_symlink_without_touching_target() {
        let root = std::env::temp_dir().join(format!("atb-rmlink-{}", std::process::id()));
        let target = root.join("target");
        let link = root.join("link");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&target).unwrap();
        if create_symlink_dir(&target, &link).is_err() {
            let _ = fs::remove_dir_all(&root);
            return;
        }
        remove_path(&link).unwrap();
        assert!(
            fs::symlink_metadata(&link).is_err(),
            "软链接应已被删除: {}",
            link.display()
        );
        assert!(target.is_dir(), "目标目录不能被连带删除");
        let _ = fs::remove_dir_all(&root);
    }
}
