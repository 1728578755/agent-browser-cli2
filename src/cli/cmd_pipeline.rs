use super::*;
#[allow(unused_imports)]
use clap::{Args, Parser, Subcommand};

pub(crate) fn submit_job(op: &str, args: Value) -> Result<Value> {
    // 后台任务走的是同一套参数，必须做和同步路径完全相同的收尾：路径绝对化、
    // 预检、目标解析。否则 `screenshot --async --out shots/a.png` 会被
    // daemon 按自己的 cwd（home）解析，`upload --async` 则拿相对路径直接报不存在。
    let mut args = args;
    if let Err(problem) = finalize_page_payload(op, &mut args) {
        return Ok(problem.to_json());
    }
    // 后台任务按「一条命令一个目标」建模；多目标提交会被拆成 N 个 job，
    // 但调用方只拿到一个 id，结果必然对不上。这里直接说清楚怎么改。
    if let Some(tab) = args.get("switch_tab_id").and_then(Value::as_str) {
        // 解析已由 finalize_page_payload 完成，这里只判定形态。
        let spec =
            TargetSpec::parse(tab).map_err(|err| usage_failure(err.code, err.message, err.hint))?;
        if spec.is_multi() || spec.needs_daemon_resolution() {
            return Ok(preflight::Problem::new(
                "unsupported_combination",
                "--async 不能和 all / @N / url: 这类需要解析的 --tab 一起用",
                "要么去掉 --async（多标签会并发执行并一次返回），要么用明确的 tab id 逐个提交",
            )
            .with_example("atb exec --tab all 'return document.title'")
            .to_json());
        }
    }
    request(
        "POST",
        "/job/submit",
        Some(json!({ "op": op, "args": args })),
        15.0,
    )
}

pub(crate) fn run_job_command(args: JobCommand) -> Result<()> {
    match args.action {
        JobAction::List(args) => {
            ensure_server()?;
            print_json(request(
                "GET",
                &format!("/job/list?limit={}", args.limit),
                None,
                10.0,
            )?);
        }
        JobAction::Get(args) => {
            ensure_server()?;
            print_json(request(
                "GET",
                &format!("/job/get/{}", query_escape(&args.id)),
                None,
                10.0,
            )?);
        }
        JobAction::Cancel(args) => {
            // `job cancel --dry-run` 以前照样真的发取消请求：取消是不可撤销的
            // 状态变更，必须只输出计划。
            if DRY_RUN.load(Ordering::Relaxed) {
                print_json(json!({
                    "ok": true,
                    "result": {
                        "dry_run": true,
                        "command": "job cancel",
                        "id": args.id,
                        "would": [format!("POST /job/cancel/{}（协作式取消，已在执行的动作可能继续到结束）", args.id)],
                        "note": "去掉 --dry-run 才会真的提交取消",
                    },
                }));
                return Ok(());
            }
            ensure_server()?;
            print_json(request(
                "POST",
                &format!("/job/cancel/{}", query_escape(&args.id)),
                Some(json!({})),
                10.0,
            )?);
        }
        JobAction::Wait(args) => wait_job(&args)?,
    }
    Ok(())
}

pub(crate) fn wait_job(args: &JobWaitArgs) -> Result<()> {
    // 负值 / nan / inf 以前被 `.max(0.1)` 静默夹成 0.1s：调用方写 `--timeout -5`
    // 本意是「立刻返回」，实际却报 job_wait_timeout，而退出码/文本都指向「job 太慢」
    // 。这是用法问题，必须在发请求前就以 exit 2 拒绝。
    if !args.timeout.is_finite() || args.timeout <= 0.0 {
        return Err(usage_failure(
            "invalid_argument",
            format!(
                "job wait --timeout 必须是大于 0 的有限秒数（收到 {}）",
                args.timeout
            ),
            "例如 --timeout 60；只想探一下状态用 `atb job get <id>`，不要给 0 或负数",
        ));
    }
    ensure_server()?;
    let timeout = args.timeout.max(0.1);
    let deadline = Instant::now() + secs(timeout);
    let interval = secs(args.interval.clamp(0.05, 5.0));
    let path = format!("/job/get/{}", query_escape(&args.id));
    loop {
        // 轮询期的传输错误不能直接冒成 exit 1：调用方会把它当成「执行失败」而不是
        // 「等待超时」，于是重试整条管道。
        let value = match request("GET", &path, None, 10.0) {
            Ok(value) => value,
            Err(err) => {
                if Instant::now() >= deadline {
                    print_json(json!({
                        "ok": false,
                        "error": format!("job {} 在 {}s 内未完成（最后一次轮询失败：{err}）", args.id, timeout),
                        "error_code": "job_wait_timeout",
                    }));
                    return Ok(());
                }
                sleep(interval);
                continue;
            }
        };
        let status = value
            .pointer("/job/status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        if status == "unknown" {
            // daemon 不认识这个 job（过期或 id 写错）= 用法问题，退出码 2。
            // 注意要在 print_json 之后设置：print_json 会按 error_code 重算退出码。
            print_json(json!({
                "ok": false,
                "error": format!("job {} 不存在或已过期", args.id),
                "error_code": "job_not_found",
                "hint": "用 `atb job list` 看最近的 job；daemon 只保留最近 100 条",
            }));
            FAILURE_EXIT_CODE.store(EXIT_USAGE, Ordering::Relaxed);
            return Ok(());
        }
        if status != "running" {
            print_json(value);
            // 只有 succeeded 才算成功；cancelled 用 130（与 shell 的 SIGINT 惯例一致），
            // 其余终态（failed）按普通失败处理，交给 run 末尾统一 exit。
            if status != "succeeded" {
                let code = if status == "cancelled" {
                    130
                } else {
                    EXIT_FAILURE
                };
                FAILURE_EXIT_CODE.store(code, Ordering::Relaxed);
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            print_json(json!({
                "ok": false,
                "error": format!("job {} 在 {}s 内未完成", args.id, timeout),
                "error_code": "job_wait_timeout",
                "job": value.get("job").cloned().unwrap_or(Value::Null)
            }));
            return Ok(());
        }
        sleep(interval);
    }
}

/// 管道请求的 HTTP 超时预算。
///
/// 旧实现给 `/run`、`/batch` 写死 600s：10 步 × 60s 的管道在 daemon 里还没跑完，
/// CLI 就先断开（reqwest 裸 "operation timed out"，退出码也归不到 job 超时），
/// 而 daemon 仍在后台继续跑——调用方看到的是「失败」，实际是孤儿化。
///
/// 口径：`30s 底座 + Σ(每步 timeout + 5s 余量)`，每步没写 `timeout` 按服务端默认
/// 60s 计，最后夹在 `[60, 3600]`——上界保证一个写错的超大 timeout 不会挂住 CLI。
pub(crate) fn pipeline_http_timeout(steps: &Value) -> f64 {
    // 单步没写 timeout 时的服务端默认值（与 daemon run/batch 的步默认一致）。
    const DEFAULT_STEP_TIMEOUT: f64 = 60.0;
    // 每步的调度/回包余量。
    const STEP_OVERHEAD: f64 = 5.0;
    let mut total = 30.0;
    for step in steps
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let step_timeout = step
            .get("timeout")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(DEFAULT_STEP_TIMEOUT);
        total += step_timeout + STEP_OVERHEAD;
    }
    // 上限必须高于「单步上限 × 合理步数」：`preflight::TIMEOUT_MAX_SECONDS` 是 3600s/步，
    // 原来这里和 `http::request` 都 clamp 到 3600s，等于「只要管道总预算超过一小时，
    // CLI 一定先断」—— 只修一半。24h 只作为防溢出的兜底，不再是实际约束。
    total.clamp(60.0, 86_400.0)
}

pub(crate) fn cmd_run(args: RunArgs, is_async: bool) -> Result<()> {
    let _ = is_async;
    if is_async {
        return Err(usage_failure(
            "unsupported_flag",
            "run 不支持 --async",
            "需要后台执行请用 --async 提交单步任务，或把管道拆成多个 job",
        ));
    }
    let payload = load_steps(&args.file, "run")?;
    // `--dry-run` 以前对 run 完全无效：`atb run steps.json --dry-run` 会真跑完整条管道。
    if DRY_RUN.load(Ordering::Relaxed) {
        print_json(plan_json("run", &payload));
        return Ok(());
    }
    ensure_server()?;
    let mut payload = payload;
    let Some(object) = payload.as_object_mut() else {
        return Err(usage_failure(
            "invalid_steps",
            "步骤文件必须是对象或数组",
            "顶层写成 {\"steps\":[...]}，或直接给一个数组",
        ));
    };
    // 文件里的 stop_on_error 以前用 `entry.or_insert` 写入，文件给了值就
    // 静默盖掉 --continue-on-error——同一个 flag 在两条来源上两种语义，调用方无从发现。
    // 现在：两边都显式且冲突 → exit 2；只有文件给 → 保留文件值；都没给 → 按 flag 推导。
    // （`--continue-on-error` 是布尔开关，只有「给了」= true 这一种显式形态。）
    let file_stop_on_error = object.get("stop_on_error").and_then(Value::as_bool);
    if args.continue_on_error && file_stop_on_error == Some(true) {
        return Err(usage_failure(
            "unsupported_combination",
            "run: --continue-on-error（遇错继续）与步骤文件里的 \"stop_on_error\": true（遇错即停）冲突",
            "二者只能留一个：去掉 --continue-on-error，或把步骤文件的 stop_on_error 改成 false / 删掉该键",
        ));
    }
    let stop_on_error = file_stop_on_error.unwrap_or(!args.continue_on_error);
    object.insert("stop_on_error".to_string(), json!(stop_on_error));
    let http_timeout = pipeline_http_timeout(&payload);
    let mut value = request("POST", "/run", Some(payload), http_timeout)?;
    // 回显生效值：两种来源合并后到底停不停，调用方不必自己按同样的规则再推一遍。
    if let Some(map) = value.as_object_mut() {
        let meta = map
            .entry("result_meta".to_string())
            .or_insert_with(|| json!({}));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert("stop_on_error".to_string(), json!(stop_on_error));
        }
    }
    print_json(value);
    Ok(())
}

pub(crate) fn cmd_batch(args: BatchArgs, is_async: bool) -> Result<()> {
    // `--async` 是全局 flag：`run` 早就显式拒绝，`batch` 却只写 `let _ = is_async`，
    // 于是同一个 flag 在两条管道上行为不同（一条报错、一条静默同步执行）。
    // 这正是 `reject_async_if_set` 要消灭的形态（v8）。
    reject_async_if_set("batch", is_async)?;
    let payload = load_steps(&args.file, "batch")?;
    // 同 run：`--dry-run` 必须只输出计划。
    if DRY_RUN.load(Ordering::Relaxed) {
        print_json(plan_json("batch", &payload));
        return Ok(());
    }
    ensure_server()?;
    let mut payload = payload;
    let Some(object) = payload.as_object_mut() else {
        return Err(usage_failure(
            "invalid_steps",
            "步骤文件必须是对象或数组",
            "顶层写成 {\"steps\":[...]}，或直接给一个数组",
        ));
    };
    object.insert(
        "concurrency".to_string(),
        json!(args.concurrency.clamp(1, 32)),
    );
    // 同 run：超时按步骤预算算，不再写死 600s。
    let http_timeout = pipeline_http_timeout(&payload);
    print_json(request("POST", "/batch", Some(payload), http_timeout)?);
    Ok(())
}

/// 读步骤文件并规范化成 `{steps:[...]}`。两个管道命令共用同一套报错。
fn load_steps(path: &Path, op: &str) -> Result<Value> {
    let raw = std::fs::read_to_string(path).map_err(|err| {
        usage_failure(
            "file_not_found",
            format!("读取步骤文件失败: {} ({err})", path.display()),
            "确认路径存在；相对路径按当前工作目录解析",
        )
    })?;
    let mut payload: Value = serde_json::from_str(&raw).map_err(|err| {
        usage_failure(
            "invalid_steps",
            format!("步骤文件不是合法 JSON: {} ({err})", path.display()),
            "用 `atb run --help` 看步骤格式，或先 `atb run steps.json --dry-run` 校验",
        )
    })?;
    if payload.is_array() {
        payload = json!({ "steps": payload });
    }
    if payload.get("steps").and_then(Value::as_array).is_none() {
        return Err(usage_failure(
            "invalid_steps",
            format!("{op}: 步骤文件缺少 steps 数组"),
            "顶层写成 {\"steps\":[{\"op\":\"read\",\"args\":{}}]}",
        ));
    }
    Ok(payload)
}

/// `--dry-run` 的计划输出：字段与真实执行保持一致，Agent 可以直接用同一套解析。
///
/// **每一步都要真的跑一遍 preflight**。`--dry-run` 的对外承诺是「预检失败时命令不会
/// 发到浏览器」，而旧实现只回显 `op`/`args`：选择器写错、op 不认识时照样返回
/// `ok:true`，真实执行才失败——「假绿」比不检查更糟（v8）。
fn plan_json(op: &str, payload: &Value) -> Value {
    let mut steps: Vec<Value> = Vec::new();
    let mut first_problem: Option<Value> = None;
    if let Some(items) = payload.get("steps").and_then(Value::as_array) {
        for (index, step) in items.iter().enumerate() {
            // 步骤形状必须在本地校验：以前只要 preflight 放行就 ok:true，
            // 非对象 step / 缺 op / 未知 op 都会在 dry-run 里「假绿」，真跑时 daemon
            // 直接 422 反序列化失败，调用方根本不知道为什么。
            let mut step_op = String::new();
            let mut step_args = json!({});
            // 每个分支都必定赋值，先不初始化：避免 clippy 的 unused_assignments。
            let problem: Option<preflight::Problem>;
            match step.as_object() {
                None => {
                    problem = Some(
                        preflight::Problem::new(
                            "invalid_steps",
                            format!(
                                "第 {} 步不是 JSON 对象（收到 {}）",
                                index + 1,
                                json_kind(step)
                            ),
                            "每一步写成 {\"op\":\"read\",\"args\":{...}}",
                        )
                        .with_field("steps"),
                    );
                }
                Some(map) => {
                    step_op = map
                        .get("op")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    step_args = map.get("args").cloned().unwrap_or_else(|| json!({}));
                    if step_op.is_empty() {
                        problem = Some(
                            preflight::Problem::new(
                                "invalid_steps",
                                format!("第 {} 步缺少 op", index + 1),
                                format!(
                                    "每个步骤必须写 op；可用：{}",
                                    crate::server::JOB_OPS.join(", ")
                                ),
                            )
                            .with_field("steps"),
                        );
                    } else if !crate::server::JOB_OPS.contains(&step_op.as_str()) {
                        problem = Some(
                            preflight::Problem::new(
                                "unsupported_op",
                                format!("第 {} 步的 op {step_op:?} 不合法", index + 1),
                                format!(
                                    "可用 op：{}；查看某一步的参数用 atb schema",
                                    crate::server::JOB_OPS.join(", ")
                                ),
                            )
                            .with_field("steps"),
                        );
                    } else if !step_args.is_object() {
                        problem = Some(
                            preflight::Problem::new(
                                "invalid_steps",
                                format!(
                                    "第 {} 步的 args 必须是 JSON 对象（收到 {}）",
                                    index + 1,
                                    json_kind(&step_args)
                                ),
                                "写成 \"args\":{...}；没有参数时写 \"args\":{} 或省略",
                            )
                            .with_field("steps"),
                        );
                    } else {
                        problem = preflight::preflight_payload(&step_op, &step_args).err();
                    }
                }
            }
            if let Some(problem) = &problem {
                if first_problem.is_none() {
                    first_problem = Some(problem.to_json());
                }
            }
            steps.push(json!({
                "index": index,
                "op": step_op,
                "args": step_args,
                "ok": problem.is_none(),
                "problem": problem.map(|problem| problem.to_json()).unwrap_or(Value::Null),
            }));
        }
    }
    let mut plan = json!({
        "ok": first_problem.is_none(),
        "dry_run": true,
        "op": op,
        "count": steps.len(),
        "total": steps.len(),
        "stopped_at": Value::Null,
        "steps": steps,
    });
    if let Some(problem) = first_problem {
        // 顶层带上第一个问题，调用方不必自己遍历 steps 就能拿到 error_code/hint。
        plan["error_code"] = problem
            .get("error_code")
            .cloned()
            .unwrap_or_else(|| json!("invalid_steps"));
        plan["error"] = problem.get("error").cloned().unwrap_or(Value::Null);
        if let Some(hint) = problem.get("hint") {
            plan["hint"] = hint.clone();
        }
        plan["problem"] = problem;
    }
    plan
}

/// 给「步骤不是对象」之类的类型错误一个人读的类型名。
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "布尔值",
        Value::Number(_) => "数字",
        Value::String(_) => "字符串",
        Value::Array(_) => "数组",
        Value::Object(_) => "对象",
    }
}
