mod cli;
mod config;
mod html;
mod launch;
mod open_target;
mod preflight;
mod protocol;
mod server;
mod target_spec;

use std::process::ExitCode;

/// 统一出口：任何失败都输出结构化 JSON，并给出契约内的退出码。
///
/// 以前是 `fn main() -> Result<()>`：漏网的 `Err` 会变成 stderr 上的纯文本
/// `Error: ...` + 退出码 1，调用方 Agent 的 JSON 解析器直接崩。
fn main() -> ExitCode {
    install_panic_hook();
    let code = match cli::run() {
        Ok(()) => cli::exit_code(),
        Err(err) => cli::report_failure(&err),
    };
    ExitCode::from(code.clamp(0, 255) as u8)
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
