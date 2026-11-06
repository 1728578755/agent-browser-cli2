mod cli;
mod config;
mod hmac;
mod html;
mod launch;
mod open_target;
mod preflight;
mod protocol;
mod server;
mod settings;
mod target_spec;

use std::process::ExitCode;

/// 统一出口：任何失败都输出结构化 JSON，并给出契约内的退出码。
///
/// 以前是 `fn main() -> Result<()>`：漏网的 `Err` 会变成 stderr 上的纯文本
/// `Error: ...` + 退出码 1，调用方 Agent 的 JSON 解析器直接崩。
fn main() -> ExitCode {
    // daemon 是常驻多任务进程，任意 task 的 panic 都只应记日志，
    // 绝不能触发 `process::exit(1)` 把整个 daemon 连同所有连接一起杀掉。
    if is_daemon_invocation() {
        install_daemon_panic_hook();
    } else {
        install_panic_hook();
    }
    let code = match cli::run() {
        Ok(()) => cli::exit_code(),
        Err(err) => cli::report_failure(&err),
    };
    ExitCode::from(code.clamp(0, 255) as u8)
}

/// 这次进程是 `atb daemon` 吗？
///
/// 在 clap 解析之前判断，所以只看原始参数里有没有 `daemon` 这个子命令名
/// （`Daemon` 是最后一个子命令，默认 kebab-case 名称就是 `daemon`，无别名）。
/// 参数解析失败时也会落到 CLI 分支装普通 hook，行为与旧版一致。
fn is_daemon_invocation() -> bool {
    std::env::args_os()
        .skip(1)
        .any(|arg| arg.to_string_lossy().as_ref() == "daemon")
}

/// daemon 专用 hook：只记日志，不退出进程。
///
/// tokio 的 task 默认会在 panic 后中止该 task，但进程继续——这正是 daemon
/// 需要的语义：一条坏消息/一个坏标签页不该把其他连接和后台 job 一起带走。
fn install_daemon_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let message = if let Some(text) = info.payload().downcast_ref::<&str>() {
            (*text).to_string()
        } else if let Some(text) = info.payload().downcast_ref::<String>() {
            text.clone()
        } else {
            "未知 panic 载荷".to_string()
        };
        let location = info
            .location()
            .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()))
            .unwrap_or_else(|| "unknown".to_string());
        eprintln!(
            "daemon: task panic（已忽略，daemon 继续运行；该 task 可能已被中止）: {message}（{location}）"
        );
    }));
}

/// 把 panic 也变成契约内的结构化输出。
///
/// Rust 默认的 panic 是 stderr 上的一段纯文本 + 退出码 101，既不在对外契约的
/// 退出码集合（1/2/3/130）里，也没法被 Agent 的 JSON 解析器读懂——调用方只能看到
/// 「命令莫名其妙退出」。
///
/// hook 里必须避免再次 panic：只用 `writeln!`/`flush` 并且**忽略**错误（stdout 关了
/// 也不能让 hook 自己崩），最后用 `process::exit(1)` 给出契约内退出码。
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let message = if let Some(text) = info.payload().downcast_ref::<&str>() {
            (*text).to_string()
        } else if let Some(text) = info.payload().downcast_ref::<String>() {
            text.clone()
        } else {
            "未知 panic 载荷".to_string()
        };
        let location = info
            .location()
            .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()))
            .unwrap_or_else(|| "unknown".to_string());
        let payload = serde_json::json!({
            "ok": false,
            "error": format!("panic: {message}（{location}）"),
            "error_code": "internal_panic",
            "hint": "这是 atb 的 bug，请附上复现命令反馈",
        });
        {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "{payload}");
            let _ = stdout.flush();
        }
        std::process::exit(1);
    }));
}
