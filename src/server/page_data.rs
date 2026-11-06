use super::*;

pub(crate) async fn get_html(
    state: &AppState,
    session_key: &str,
    _cutlist: bool,
    maxchars: usize,
    text_only: bool,
    timeout: Duration,
    node_budget: usize,
) -> Result<String> {
    // 纯文本走 page_read 的单遍 TreeWalker：optHTML 的文本分支要为每个节点
    // 读 getComputedStyle / getBoundingClientRect，十万级页面上是秒级开销。
    if text_only {
        let opts = json!({
            "maxChars": maxchars,
            "forms": false,
            "links": false,
            "maxNodes": node_budget.max(1),
            "budgetMs": 1500,
        });
        let script = format!("{}\nreturn __abcRead({});", html::js_page_read(), opts);
        let response =
            execute_raw_js_with_session(state, &script, timeout, Some(session_key)).await?;
        let payload = response.result.data.unwrap_or(Value::Null);
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // 降级路径也必须再过一次 maxchars —— 页面侧 maxChars 只是请求值，
        // 老版本扩展 / 文本抽取都可能给回超预算的内容。
        return Ok(truncate_output(clean_text(&text), maxchars));
    }
    // 十万级页面上，optHTML 的调用开销和 122KB 结果的跨进程传输都要 1~2 秒，
    // 而调用方真正要的是「页面里有什么」：直接走单遍文本抽取。
    // node_budget = 0 表示调用方不关心页面规模（monitor 的前后快照），跳过探测。
    const HUGE_PAGE_ELEMENTS: usize = 60_000;
    let element_count = if node_budget == 0 {
        0
    } else {
        page_element_count(state, session_key, timeout)
            .await
            .unwrap_or(0)
    };
    if element_count > HUGE_PAGE_ELEMENTS {
        let opts = json!({
            "maxChars": maxchars,
            "forms": false,
            "links": false,
            "maxNodes": 120_000,
            "budgetMs": 1500,
        });
        let script = format!("{}\nreturn __abcRead({});", html::js_page_read(), opts);
        let response =
            execute_raw_js_with_session(state, &script, timeout, Some(session_key)).await?;
        let text = response
            .result
            .data
            .unwrap_or(Value::Null)
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        return Ok(truncate_output(
            format!(
                "<!-- scan: {element_count} elements > {HUGE_PAGE_ELEMENTS}, returned text instead of HTML (use read / scan --fast) -->\n{}",
                clean_text(&text)
            ),
            maxchars,
        ));
    }
    let opt = html::js_opt_html();
    let page_script = format!(
        "{opt}\nreturn optHTML(false, {});",
        json!({ "nodeBudget": node_budget, "tooLarge": if node_budget > 0 { 60_000 } else { 0 } })
    );
    let response =
        execute_raw_js_with_session(state, &page_script, timeout, Some(session_key)).await?;
    let mut page = response
        .result
        .data
        .unwrap_or(Value::Null)
        .as_str()
        .unwrap_or_default()
        .to_string();
    // 超大页面：页面侧拒绝序列化，这里改走单遍文本抽取（几十毫秒）。
    if let Some(detail) = page.strip_prefix("__ABC_SCAN_ERROR__") {
        // 页面侧把异常兜成了哨兵（超深嵌套导致 RangeError、页面 API 被冻结等）。
        // 必须转成明确的错误，而不是把哨兵字符串当 HTML 返回给调用方。
        return Err(anyhow!("scan: 页面侧处理失败（{}）", detail.trim()));
    }
    if let Some(count) = page.strip_prefix("__ABC_TOO_LARGE__") {
        let elements = count.trim().parse::<usize>().unwrap_or_default();
        let opts = json!({
            "maxChars": maxchars,
            "forms": false,
            "links": false,
            "maxNodes": 120_000,
            "budgetMs": 1500,
        });
        let script = format!("{}\nreturn __abcRead({});", html::js_page_read(), opts);
        let response =
            execute_raw_js_with_session(state, &script, timeout, Some(session_key)).await?;
        let text = response
            .result
            .data
            .unwrap_or(Value::Null)
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        return Ok(truncate_output(
            format!(
                "<!-- scan: {elements} elements > 60000, returned text instead of HTML (use read / scan --fast) -->\n{}",
                clean_text(&text)
            ),
            maxchars,
        ));
    }
    page = html::optimize_html_for_tokens(&page);
    // 列表识别不在这里做：它需要对全站 querySelectorAll('*') 并对多个容器强制触发布局，
    // 是 scan 最大的无谓开销。若将来作为独立能力（scan --list）引入，必须把结果返回给调用方，
    // 而不是白跑一次。
    // `maxchars` 是字符预算，`String::len()` 是字节数：中文页按字节比会在远未超预算时
    // 就进 truncate，白跑一次 O(n) 计数（结果不变，纯浪费）。
    page = truncate_output(page, maxchars);
    Ok(page)
}

/// 统一的 `maxchars` 二次截断。
///
/// 主路径与三条降级路径（text_only / 超大页面 / 页面侧 TOO_LARGE）都走这里，
/// 避免「只有主路径截断、降级路径原样返回超长内容」。
/// 截断后的文本以 `[TRUNCATED]` 结尾（由 `html::smart_truncate` 追加），
/// 消费方（monitor 等）可据此把回包标成 `truncated:true`；
/// 结果字符数（含标记）不超过 `maxchars`。
pub(crate) fn truncate_output(text: String, maxchars: usize) -> String {
    if maxchars == 0 || text.chars().count() <= maxchars {
        return text;
    }
    html::smart_truncate(text, maxchars)
}

pub(crate) fn clean_text(input: &str) -> String {
    // 输出只做「去空白行 + 行首尾裁剪 + \n 归一」，长度必然不超过输入：
    // 先按输入长度预留容量，避免旧写法 `collect::<Vec<_>>().join("\n")`
    // 先分配一个 &str 数组、再反复扩容拼接（旧报告 #10 的「微小重分配」）。
    let mut out = String::with_capacity(input.len());
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

/// 内部扩展命令的显式标记。
///
/// 只有 daemon 自己构造的控制帧才带这个字段；用户脚本里自然写出的
/// `{"cmd":"..."}` 不再被当成控制命令（以前会绕过 `wrap_user_script` 直接下发）。
pub(crate) const INTERNAL_CMD_MARKER: &str = "__atb_internal_cmd";

/// 允许走「内部命令」通道的 cmd 白名单（与 `assets/atb_bridge/background.js`
/// 的 `handleExtMessage` 分发保持一致；新增扩展命令时两边一起加）。
const INTERNAL_CMD_WHITELIST: &[&str] = &[
    "status",
    "setPort",
    "setProfileLabel",
    "setWsToken",
    "cookies",
    "cdp",
    "batch",
    "captureVisible",
    "captureFullPage",
    "blockingExtensions",
    "openTab",
    "closeTab",
    "networkStart",
    "networkList",
    "networkDetail",
    "networkClear",
    "networkStop",
    "consoleStart",
    "consoleList",
    "consoleClear",
    "consoleStop",
    "debugClearAll",
    "tabs",
    "inject",
    "injectProbe",
    "frames",
    "downloads",
    "pageOp",
    "netIntercept",
    "selfCheck",
    "management",
    "contentSettings",
    "cancel",
];

/// 这一段字符串是否是 daemon 内部的扩展控制命令。
///
/// 旧判据只要求「JSON 形状 + 含 cmd」，用户脚本 `{"cmd":"..."}`
/// 会被原样直送扩展、绕过用户脚本包装与错误上抛；现在必须同时满足
/// 1. 带显式内部标记 `__atb_internal_cmd: true`；
/// 2. `cmd` 在内部白名单内。
pub(crate) fn is_extension_json(script: &str) -> bool {
    let trimmed = script.trim();
    if !trimmed.starts_with('{') {
        return false;
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return false;
    };
    if value.get(INTERNAL_CMD_MARKER).and_then(Value::as_bool) != Some(true) {
        return false;
    }
    value
        .get("cmd")
        .and_then(Value::as_str)
        .is_some_and(|cmd| INTERNAL_CMD_WHITELIST.contains(&cmd))
}

/// 给内部扩展控制命令打上标记（构造点统一走这里）。
///
/// 仓库内的控制帧（`cdp.rs` / `open.rs` / `lifecycle.rs` 等）直接经
/// `execute_raw_js*` 下发、不经过 `exec_value`，无需改；凡是可能经过
/// `/exec`（`exec_value` → `execute_page_js*`）的调用必须用本函数构造。
#[allow(dead_code)]
pub(crate) fn internal_extension_cmd(mut command: Value) -> String {
    if let Value::Object(map) = &mut command {
        map.insert(INTERNAL_CMD_MARKER.to_string(), json!(true));
    }
    command.to_string()
}

/// 把用户脚本包成语义确定的 async IIFE。
///
/// 目的：不依赖扩展侧 `_air`（自动补 `return`）的启发式。
/// 例如 `a(); return b` 在扩展侧会被误改成 `return a();`，返回值变成 a 的结果。
/// 这里统一处理三种写法：`return x`、裸表达式、多语句 + `await`。
///
/// **先编译、后执行**：通道选择只依赖编译结果，不依赖运行期抛出的异常。
/// 旧实现是「eval 抛 SyntaxError 且消息含 return/await 就用 AsyncFunction 重跑」——
/// 用户脚本自己 `throw new SyntaxError('await ...')`、或先 POST 再抛错，都会导致整段脚本
/// 执行两次（副作用翻倍），第二次侥幸成功还会把失败报成成功。
/// 现在：
/// 1. 以 `return` 开头 → 脚本上下文必然 SyntaxError，直接走 AsyncFunction（静态判断）；
/// 2. 其余先用 `new Function` 编译一次（**只编译不执行**），编译不过 = 顶层 await
///    或真语法错误 → 交给 AsyncFunction 编译并执行一次；
/// 3. 编译通过才 eval 执行一次。`eval` 的 SyntaxError 一定发生在**执行之前**
///    （整段先编译），因此用前置探针区分「一行都没跑」与「用户自己抛的 SyntaxError」：
///    探针没置位就证明零副作用，可以安全换 AsyncFunction 跑一次（覆盖
///    `a(); return b` 这种脚本上下文非法的写法）；探针已置位则是用户代码运行时抛的，
///    原样上抛，绝不再跑第二次。
pub(crate) fn wrap_user_script(script: &str) -> String {
    format!(
        r#"(async () => {{
  const __abcCode = {code};
  const __abcTrimmed = String(__abcCode || '').trim();
  if (!__abcTrimmed) return undefined;
  const __abcAsync = Object.getPrototypeOf(async function () {{}}).constructor;
  let __abcCompilesAsPlain = !/^return\b/.test(__abcTrimmed);
  if (__abcCompilesAsPlain) {{
    try {{ new Function(__abcTrimmed); }} catch (__abcCompileError) {{ __abcCompilesAsPlain = false; }}
  }}
  if (!__abcCompilesAsPlain) {{
    // 只执行一次；编译不过就由 AsyncFunction 直接抛 SyntaxError。
    return await (new __abcAsync(__abcTrimmed))();
  }}
  let __abcStarted = false;
  try {{
    // 探针与用户代码拆成两次 eval，用户代码不再带任何前缀 ——
    // 旧实现 `eval('__abcStarted = true;\n' + code)` 会让首行 `"use strict"`
    // 不再是「指令序言」（整段退化成非严格模式），行号也整体 +1。
    eval('__abcStarted = true');
    const __abcValue = eval(__abcTrimmed);
    return __abcValue instanceof Promise ? await __abcValue : __abcValue;
  }} catch (__abcError) {{
    if (__abcError instanceof SyntaxError) {{
      // 区分两种 SyntaxError：
      // 1. eval 在**编译阶段**失败（顶层 return / 脚本上下文非法）⇒ 一行都没执行，
      //    换 AsyncFunction 安全；
      // 2. 用户代码运行时自己 throw 的 SyntaxError ⇒ 绝不能再跑第二次。
      // 判定：把同一段代码包进恒假的 if 块再 eval 一次 —— 只编译、不执行。
      let __abcEvalLegal = true;
      try {{ eval('if (false) {{' + __abcTrimmed + '\n}}'); }} catch (_) {{ __abcEvalLegal = false; }}
      if (__abcStarted && !__abcEvalLegal) {{
        return await (new __abcAsync(__abcTrimmed))();
      }}
    }}
    // 运行期 SyntaxError 是用户代码自己抛的，不能再跑第二次。
    throw __abcError;
  }}
}})()"#,
        code = serde_json::to_string(script).unwrap_or_else(|_| "\"\"".to_string())
    )
}

pub(crate) async fn touch(state: &AppState) {
    *state.last_activity.lock().await = Instant::now();
}
