# 更新日志

所有重要变更都会记录在这里。日期使用北京时间自然日。

本文件遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 的结构组织条目。
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)（SemVer）。

## v0.3.8 - 2026-11-02

首次在 GitHub 公开发布（<https://github.com/1728578755/agent-browser-cli>）。相对 v0.3.7 补齐了页面读取、批量执行、网络拦截、挑战页处理、异步任务等能力，并统一了退出码契约。

### 新增

- **`read`**：返回标题、正文、元信息、表单与元素计数。支持 `--selector`、`--max-chars`、`--links`、`--include-hidden`（`textContent` 语义，含隐藏内容）、`--budget-ms`、`--max-nodes`、`--frame`；响应带 `text_mode`（`innerText` / 近似可见 / 全量文本）、`partial`、`stats{node_count,scanned,visited,elapsed_ms}`。
- **`find`**：支持 CSS、可见文本、XPath 三种定位，返回可操作信息与简短 CSS 路径。
- **`scan`**：新增 `--fast`（一次注入的紧凑文本抽取）与 `--max-nodes`；页面元素数超过 6 万时自动改走文本抽取，并在内容头部注明原因。
- **页面动作**：`hover`、`select`、`check`、`scroll`、`wait`（`--selector` / `--text` / `--js`）、`back`、`reload`，全部单次注入往返。`click` / `fill` / `send-keys` / `read` / `find` / `wait` 支持 `--frame`，指定 iframe 时强制走页面内动作（CDP 的 `DOM.resolveNode` 只对主框架有效）。
- **`wait` 单次注入**：`wait --selector` / `--text` 用页面内 MutationObserver 一次注入完成，返回 `mode:"observer"`；`wait --js` 的谓词在页面内编译一次，DOM 变化由 MutationObserver 触发、非 DOM 条件由定时器兜底，返回 `mode:"js-observer"`、`injections`（正常为 1）、`checks`。`exec --wait-js`、`click/fill --wait-js` 复用同一实现，输出仍是 `js_return: {result, wait}`，`wait` 内多了 `injections` / `checks` / `mode`。
- **`snapshot --js`**：DOM 版快照，不经 CDP，产出带 CSS 选择器的 `@e`；CDP 不可用时自动回退，给出 `mode:"js"` 与 `fallback_reason`。这类 `@e` 被 `click` / `fill` 使用时直接走页面内动作，不再为一次点击 attach debugger。`snapshot` 另支持 `--interactive-only`（只留可操作元素）与 `--max-nodes`（截断大树），AX 与 DOM 两种模式都生效。
- **严格 CSP 页面的无 eval 通道**：`eval` 被 CSP 拦下后，`read` / `find` / `snapshot` / 页面动作 / `wait` 原本直接失败。现在改由扩展注入固定的 `ops/page_*.js` 并调用其函数，不经过 eval，结果带 `via:"no-eval"`；GitHub 首页实测 `read` 269 字、`find` 19 个链接、`snapshot` 5 个 `@e`，`hover` / `scroll` / `wait` 全可用。
- **`inject`**：独立注入命令，支持 `--world main|isolated`、`--frame`、`--all-frames`、`--file`，语义与 `exec` 一致（`return`、裸表达式、`await` 均可）。`inject --persist <name> [--match <pattern>]` 在每次导航后自动重放，`--list` / `--remove` 管理。
- **`frames`**：列出标签页内所有 frame（`frame_id` / `parent` / `url`），配合 `exec --frame` 使用。
- **`exec` 增强**：支持 `--world isolated`（不受页面 CSP 限制，但看不到页面变量）、`--frame` / `--all-frames`、`--max-bytes N`（超限返回截断预览，带 `truncated:true` 与 `total_chars`）、`--file -`（从 stdin 读脚本）。
- **`run --file steps.json`**：一次进程执行多步，默认遇错即停，`--continue-on-error` 继续，返回逐步结果与耗时。
- **`batch`**：`POST /batch` + `batch <steps.json> --concurrency N`，一次请求内并发执行最多 N 步。与串行 `run` 分别对应「无依赖批量」和「有依赖流程」。
- **异步任务**：`--async` 可提交 `exec` / `scan` / `snapshot` / `click` / `mouse-click` / `fill` / `send-keys` / `upload` / `screenshot` / `save-pdf` / `read` / `find` / `hover` / `select` / `check` / `scroll` / `wait`，立即返回 `job_id`；新增 `job list|get|wait|cancel`，状态为 `running|succeeded|failed|cancelled`。`/health` 增加 `jobs:{running,total}`，任务运行期间 daemon 不因空闲退出。
- **`open` 多 URL 与前后台**：默认前台打开并切到新标签，`--background` 在后台创建；两者都会让新标签成为后续命令的默认目标，返回新增 `result.ready`。一次给多个 URL 时客户端并发打开并返回 `elapsed_ms`。
- **`cookies`**：读取当前页面或指定 URL 的 Cookie，不再需要 `exec '{"cmd":"cookies"}'` 这类逃生口。
- **`upload <selector|@e> <files...>`**：用 CDP `DOM.setFileInputFiles` 设置文件输入框，支持多文件。
- **`tabs --full`**：输出完整 URL（默认截断到 80 字符）。
- **全局 `--pretty`**：默认输出改为单行 compact JSON，节省 token。
- **`browsers`**：按 `browser_id + profile` 列出所有已连接目标与代表标签。
- **`broadcast <op>`**：一次请求在所有匹配的 browser/profile 上并发执行同一操作（`exec` / `read` / `find` / `scan` / `net` / `downloads` 等），逐目标返回结果与耗时。
- **下载**：`downloads` 列表、`download <url>` 开始、`download-wait --id` 等待完成；扩展启用 `downloads` 权限，等待在 daemon 侧轮询。
- **页面网络 `net`**：`net fetch`（页面上下文发请求、带 cookie）、`net hook`（包装 fetch/XHR 记录）、`net log`、`net mock`（按片段或正则返回伪造响应、或 abort）、`net routes`、`net unroute`、`net reset`。
- **`net` 规则按 `browser:profile` 共享**：同一 profile 下任意标签的 `net routes` / `net mock` / `net reset` 都作用于同一套规则，`--persist` 注册的规则在新标签加载时自动生效。
- **`net mock` 请求与响应改写**：`--request-header` / `--request-body` 放行真实请求但改请求，`--response-status` / `--response-body` 放行后改响应。`responseType=blob` 在页面引擎下也支持改写：显式给 `--response-body` 时构造新 Blob 并沿用原 `content-type`，只改状态与头时如实记 `blob-without-body`。
- **`net mock --engine cdp`**：走扩展的 `chrome.debugger` + `Fetch` 域做网络层拦截，能改 `responseType=blob` 的原始二进制体，也能拦 `<img>` / `<script>` / JSONP 这类不经过 fetch/XHR 的请求；不换 body 时用 `Fetch.continueResponse` 原地放行，避免压缩响应被重新编码。
- **`net log` 合并双源**：页面与 CDP 记录合并，条目带 `source:"page"|"cdp"`、按时间排序、统一过滤与截断，返回 `sources:{page,cdp}`；`--clear` 同时清空两边；换过 body 的条目带 `preview`（前 160 字符）；XHR 条目新增 `response_modified` / `response_rewrite_skipped` / `rewritten_status`，可直接确认改写是否生效；`--engine page|cdp|both` 可只看单侧；`--debug-pass` 打通到扩展的 `debugPass`，打开 CDP 放行日志，`net status` 可验证。
- **`net routes` / `net status` 诊断字段**：`net routes` 响应新增 `cdp:{enabled,count,rules,applied}` 与 `profile`，`net` 命令响应新增 `engine` / `profile`；`net status.patterns` 标注每条规则推导出的 `urlPattern` 及来源，带 `fallback` 标记，规则没生效时能直接看出是不是 pattern 推导的问题。
- **`challenge`**：`status` 识别 Cloudflare / Turnstile / reCAPTCHA / hCaptcha / PerimeterX / Akamai / DataDome / AWS WAF 与通用验证码，返回 `{detected,vendor,kind,blocking,interactive,auto,pending,frames,signals,hints}`；`wait` 等自动放行（页面内 MutationObserver，daemon 按 4 秒一片重注入，换页后自动重试）；`focus` 把标签页带到前台交给人工，`--no-focus` 不动系统焦点。不提供自动破解。
- **`challenge solve`**：一条命令完成「自动放行 / 等验证码 token / 转人工」。拦截页等它自动通过；Turnstile / reCAPTCHA / hCaptcha 等 `*[name$="-response"]` 隐藏字段写入 token 即判定完成（隐形挑战自动写入，需要人点时聚焦后等人点完），返回 `{passed,via:"token"|"clear"|"no-challenge",focused,elapsed_ms}`，超时返回 `passed:false,reason:"timeout"`。仍不提供自动破解。
- **`read` 带出挑战信息**：标题或正文命中挑战特征时附带 `challenge` 字段，调用方能区分「拦截页」和「页面没内容」。
- **`trust-extension`**：一条命令维护扩展白名单与握手 token（`<id>` / `--remove` / `--list` / `--clear` / `--token` / `--clear-token`），写入后 daemon 每次握手重新读取，无需重启。属于可选的加固手段。
- **受控标签页设置 `autoDiscardable:false`**：避免标签页被 Chrome 内存节省机制丢弃。
- **扩展资源同步**：`scripts/sync-extension-assets.mjs` 提供 `npm run sync:extension` / `npm run check:extension`，`ops/` 与 `assets/` 的同步由脚本完成并有 CI 守卫；改过 `assets/page_*.js` 后必须同步，否则扩展与 `ops/` 内的副本不同源。
- **`--help` 覆盖**：47 个子命令补齐说明（此前 49 个里只有 2 个有），并区分 `scan` / `read` / `snapshot` 这类易混命令。
- **扩展版本自检**：`/health` 的 `extension` 段给出 `bundled_version` / `connected_versions` / `stale` / `hint`，`status` 在过期时输出 `warning`，`doctor` 增加 `extension_fresh` 检查项。
- **扩展自动重载**：每分钟比对 `background.js` 内容哈希，发现磁盘改动就在空闲时 `chrome.runtime.reload()`（首次仍需在 `chrome://extensions` 手动重载一次）；扩展连上来时 daemon 比对版本，低于随包版本直接要求浏览器重载（`management.reload`），无需用户操作。
- **扩展诊断**：`injectProbe`（分别验证 MAIN world 纯函数、MAIN world eval、ISOLATED world eval）与 `selfCheck`（哈希基线、定时器状态、命令占用）。
- **`manifest.json` 增加 `minimum_chrome_version`**：依赖 `world:'MAIN'`、`storage.session` 与 alarms 的 30s 下限。
- **测试与夹具**：`tests/mock_extension.mjs`（不依赖 Chrome 的 WebSocket 假扩展与端到端自测，支持 `ABC_CLI=target/release/agent-browser-cli.exe` 指定被测二进制，并断言 `wait` 只注入一次）、`tests/extension_net_intercept.test.mjs`（用 `node:vm` 跑真实 `background.js`，假 `chrome.debugger` 记录 CDP 调用）、`tests/live_xhr_wait.mjs`、`tests/live_cdp_net.mjs`（15 条真机断言）、`tests/live_challenge.mjs`（22 条真机断言）、`tests/gen_huge.mjs`（26 万元素页面）、`tests/concurrency_bench.mjs`、`tests/check_page_script.mjs`，以及 `tests/fixtures/read_hidden.html` / `read_hidden_2.html` / `form.html`。
- **`docs/FEATURE_STATUS.md`**：记录已实现能力、下一步与边界、实测性能。

### 变更

- **退出码契约统一（会 break 脚本）**：`ok:false` 一律非 0，设置 `AGENT_BROWSER_CLI_EXIT0=1` 可恢复旧的「恒 0」行为。具体落地：
  - `status` 的 `ok` 跟随 `healthy`，daemon 或扩展不健康时退出码非 0；
  - `job wait`：`failed` → 非 0，`cancelled` → 130（沿用 shell 的 SIGINT 惯例），等待超时 → 3；
  - `download-wait` 超时 → 3；
  - `challenge wait` / `challenge solve` 超时 → 非 0，并带 `error_code: challenge_timeout`（此前是 `ok:false` + 退出码 0）。

  脚本里若判断 `$?`，请复核上述命令的退出码。
- **`--monitor` 输出结构化（会 break 读 `top_change` 的代码）**：`click` / `fill` / `mouse-click` / `send-keys --monitor` 与 `exec --js` 的监控输出统一，`change` 变成：

  ```json
  { "changed": 2, "added": 1, "removed": 0, "attributes": 0, "text": 1,
    "samples": [{ "type": "childList", "target": "div#app", "detail": "+span" }] }
  ```

  - `changed` 从 `0|1` 改为「变更记录条数」，仍可用 `changed > 0` 判断有无变化；
  - 新增 `added` / `removed` / `attributes` / `text` 与 `samples`（最多 8 条）；
  - 移除 `top_change`：旧实现把变更前页面前 2000 字符原始 HTML 塞进返回体，白烧上下文预算；需要预览请自行截取页面内容；
  - 监控期间发生导航时返回 `{"changed":1,"navigated":true,...零值键,"samples":[]}`，判断导航请用 `change.navigated === true`，不要靠 `samples` 为空推断；
  - 拿不到计数时才出现 `monitor` 字段，`monitor.reason ∈ {unsupported, navigated, superseded, injection_failed}`，此时动作本身已成功执行；
  - 监控改为页面内 MutationObserver，在脚本执行期间直接累计，不再「执行前后各抓一次整页 HTML 再 diff」，大页面上省掉两次全量序列化。
- **`challenge --timeout` 改为可选**：不传时由服务端按 action 取默认（`wait` 30s、`solve` 120s）。此前 CLI 恒定发 30s，`solve` 的 120s 永远用不到，需要人操作的验证码必被提前判超时。
- **持久注入改为 document_start 生效**：动态注册 `persist_boot.js`（ISOLATED，`runAt=document_start`），命中后立即在 MAIN world 执行；注册失败时退回「导航后重放」，两条路径互斥，不会重复 hook。扩展版本升至 2.5。
- **同一标签的命令每会话串行**：一把异步锁，不同标签仍然并发；同标签 50+ 并发不会互相踩踏。
- **`net unroute` 支持 `--engine page|cdp`**：此前无法单独删除 CDP 规则，只能 `net reset` 全清。
- **CDP 引擎的放行默认不写日志**：此前每条放行都记一条，200 条环形缓冲几秒被挤满，`net log` 里看不到真正被拦的请求。
- **`broadcast` 参数校验收紧**：`--args` 不是对象时直接报错，不再静默丢参后照常广播。
- **`downloads erase` 不带范围现在是错误**：需要显式 `confirmAll:true` 才允许清空全部下载历史。此前 `erase` 无参会执行 `chrome.downloads.erase({})`，清空所有记录。
- **`ref_relocated` 统一为布尔**：新选择器改由 `ref_selector` 返回（此前那条路径把选择器写进了 `ref_relocated`）。
- **`net hook --engine cdp` 直接报错**：该 action 只支持页面引擎，不再「走页面注入却回报 cdp」。
- **`net` 的持久化注入名带 profile 摘要**：`__net_hook` 不再使用，旧条目会被 best-effort 清理。
- **`--header` 缺冒号、`--data` 以 `{` / `[` 开头但非法 JSON 现在都直接报错**：不再静默丢头或把坏 JSON 当字符串发出去。
- **`AppState.driver` 从 `Mutex` 改为 `RwLock`**：tabs / browsers / health / 会话查询等只读路径并发，写路径（tabs_update、选默认会话、快照缓存、net 配置）保持排他。
- **`select_tab` 不再无条件抢写锁**：默认会话已指向目标标签时走纯读路径（抽出 `resolve_session` 做纯计算），同标签连续命令不再互相排他。
- **CLI 复用同一个 HTTP Client**：原先每条命令新建 2 个 blocking client、各带独立运行时线程；热路径探活改为 TCP 探测，少一次 HTTP 往返。
- **CDP 往返合并**：`snapshot --details` 每 40 个元素的 `resolveNode + callFunctionOn` 合并成一条 batch，200 个引用从 200 次往返降到 5 次；`mouseMoved/Pressed/Released` 合并为一条 batch，并补 `pointerType`。
- **`scan` 去掉无效路径**：移除「跑一次列表识别脚本然后丢弃结果」的无效路径（全量 `querySelectorAll('*')` + 20 个容器强制布局）；列表识别后续以 `scan --list` 形式回归并返回结果。
- **减少不必要的会话开销**：`read` / `find` / 页面动作 / `wait` 不再为检测新标签枚举全部会话；`tabs` 数据模型保留完整 URL、截断只发生在展示层，`snapshot` 与 `@e` 过期校验因此省掉两次 `location.href` 往返；`run` 把多步流程从 N 个进程降到 1 个。
- **扩展侧开销**：只有脚本涉及 `alert/confirm/prompt` 时才做弹窗抑制注入（原先每条 `exec` 固定 2 次）；只有脚本可能新建标签页时才等待 120ms（原先固定 200ms）；标签页列表同步改为 250ms 去抖，内容脚本按扩展版本只补注入一次，状态角标仅在页面可见时轮询；CDP attach 后调用 `Page.setWebLifecycleState('active')`，降低后台长任务被冻结的概率。
- **`batch` 并发优于串行**：12 标签 × 5 轮共 60 步（`read` 4KB + `exec` 15ms 忙等）实测并行 `batch` 82 ms、串行 `run` 542 ms，加速 6.6x；单标签 12 条并发命令 5 ms。
- **`cargo clippy` 与 `cargo fmt --check` 成为 CI 门禁**：clippy 补上 `-- -D warnings`，并清理仓库里遗留的 8 条告警（多余 cast、可折叠 `if`、`format!` 嵌套、`clamp` 写法、测试里的 `get().is_none()` 等）；此前 `cargo fmt --check` 对每次推送都是红的。
- **CI 增加 `npm ci --dry-run` 与 lock 版本一致性校验**。

### 修复

- **`net hook --persist` 刷新后 mock 规则凭空消失**：持久化时把页面返回的「规则条数」当成规则数组写进配置，刷新重装时因类型不符被页面侧丢弃，表现为「规则只在当前页面有效，一刷新就没了」。现在先取规则列表再兜底过滤，任意操作顺序（先 hook 后 mock、先 mock 后 hook）刷新都不再丢规则。
- **`net routes` 会误清正在生效的 CDP 拦截**：`net routes` 本意是查询，实际把整份配置当 CDP 规则数组下发，把 `--engine cdp` 的规则整批覆盖掉。现在查询不再有副作用。
- **`net` 规则活不过 daemon 重启**：配置只在内存里，重启后 `net routes` 读空，而页面里的 hook 还在（persist 注入挂在扩展侧），配置与实际不一致。现在落盘到 `<用户配置目录>/net-hooks.json`，启动时读回，`net reset` 会一并删除该 profile 的持久化条目。
- **响应改写会把响应体吞成空串**：规则只改 `response.status` 时，若原响应体读不出来（已消费 / 不透明响应），页面会收到「200 + 空 body」，日志却写着改写成功。现在放行原响应，并在日志标 `response_modified: false` + `response_rewrite_skipped`（fetch 与 XHR 两条路径都改）。
- **压缩响应改写可能破坏字节**：CDP 响应阶段只改状态码或响应头时，改用 `Fetch.continueResponse` 原地放行，不再 `getResponseBody + fulfillRequest` 重新编码；只有确实替换 body 时才去掉 `content-length` / `content-encoding`。
- **XHR 响应改写不可靠**：旧实现在 `loadend` 回调里改 `xhr.responseText`，而页面自己的 `onload` / `addEventListener` 往往先注册，读到的是原始响应。现在接管 `XMLHttpRequest.prototype` 的 `responseText` / `response` / `status` / `statusText` / `getAllResponseHeaders` / `getResponseHeader`，规则在 `send()` 前挂到实例上，任何时刻、任何监听器读到的都是改写值；覆盖 `text` / `json` / `arraybuffer` / `document`，`blob` 记 `response_rewrite_skipped`。
- **XHR mock 的响应头为空**：mock 以前只伪造 `status` / `responseText`，`getResponseHeader('content-type')` 拿不到默认头。现在与真实响应共用同一套访问器，默认 `content-type: application/json` 可见。
- **XHR 请求头被追加而不是覆盖**：原生 `setRequestHeader` 对同名头是追加，页面设过 `x-token: origin` 再被规则改写会变成 `origin, rewritten`。现在请求头缓冲到 `send()` 统一下发，规则可以真正覆盖。
- **`net mock --status` 默认值吞掉响应改写**：`--status` 原本默认 200，导致任何规则都被当成「伪造响应」，`--response-status` / `--response-body` 永远不生效。现在只有显式给 `--status` 或纯 mock 时才伪造。
- **`net mock --response-status` / `--response-body` 的 `-H` 被忽略**：响应改写只读 `response.headers`，`--header` 设在规则顶层时静默失效。现在顶层 `headers` 对 mock 与改写都生效，`response.headers` 优先级更高。
- **`net` 字符串 rule 解析失败后校验被绕过**：解析得 `null` 仍写入 `opts["rule"]`，而校验用 `is_none()` —— 键存在即通过，于是错误归因成「需要 rule」（用户明明传了）。
- **`net` 的 host 解析会把持久化注入注册到所有网站**：`https://x.com?q=1` 的 host 被切成 `x.com?q=1`（pattern 非法），且 `starts_with("chrom")` 误杀 `chromium.org`，最终兜底成 `<all_urls>`，与「收窄注入范围」的目标相反。现在按 scheme 判定，merged 为空时不注册并告警，不再兜底全站。
- **持久注入的 `matches` 损坏时全站注入**：`patterns` 存在但不是非空数组时，以前回落成 `<all_urls>`；现在不注入并 `console.warn`（fail-closed），`background.js` 与 `persist_boot.js` 行为一致。
- **持久注入的匹配器忽略端口**：`persist_boot.js` 曾把 match pattern 当通配转正则，`http://127.0.0.1:55086/` 这类带端口的 URL 匹配不上，表现为「刷新后规则消失」。现在按 scheme / host / path 三段匹配，host 只比 hostname（忽略端口）。
- **持久化注入名未按 profile 隔离**：扩展侧 `persistInjections` 的键就是 name（全局），profile B 的 `net hook --persist` 会覆盖 A 的条目，A 的 `net reset` 又删掉共享条目，daemon 配置、磁盘、扩展三方失同步。现在注入名带 profile 摘要，并 best-effort 清理旧名 `__net_hook`。
- **`net reset` 的页面调用失败被伪造成成功**：`.unwrap_or_else` 把失败包成 `ok:true`，错误信息完全丢失。现在如实上抛。
- **CSP 误判导致提交按钮被点两次**：`is_csp_error` 用裸子串 `csp` 匹配，`#csp-banner not found` 这类普通报错也被当成 CSP 拦截，于是换通道重试一次，点击被重复执行。现在只认真正的 CSP 错误特征。
- **`challenge` 的「N 秒后转人工」永不触发**：`focusAfterMs` 每片都从 0 重新计时，而单片等待上限只有 4 秒，等多久都到不了阈值，需要人点的验证码页一路静默到超时。现在按总经过时间累计，到点自动把标签页带到前台。
- **`challenge` 内外两层 `ok` 相反**：超时返回 `{"ok":true,"result":{"ok":false,...}}`，直接调 HTTP API 的集成方会看到成功。现在外层 `ok` 与业务结果一致。
- **导航中的空白文档被误判成「已放行」**：`challenge wait` 在页面 `readyState=loading` 时会把它当成「没有挑战」。现在检测返回 `pending`，`wait` 遇到 `pending` 继续等；命中「清除」后还要稳定 250ms 才算数。
- **`--frame` 在 CSP 回退路径上系统性丢失**（5 处）：`net` / `read` / `find` / `challenge` / `wait` 的回退调用全部硬编码 `frame_id=None`，CSP 页面上带 `--frame` 会静默落到主框架，读到错误框架的内容且无任何提示；`net route` 更会把规则装到主框架。现在正常路径与回退路径行为一致。
- **`exec` 永远返回 null**：扩展侧原来用「代码里出现 return 就当函数体编译」的启发式，而 daemon 已经把用户脚本包成 `(async () => {...})()`；按函数体编译后末尾没有 return，IIFE 的返回值被整段丢掉，所有 `exec` 都得到 `null` 并回退 CDP。现在先按表达式求值并等待 Promise，只有 eval 因语句级 return / 顶层 await 报 SyntaxError 时才退回函数体执行，且用「是否真的存在语句级 return」决定要不要补 return。
- **单行脚本返回值错误**：`a(); return b` 这类写法会被扩展侧「自动补 return」的启发式误改成只返回 `a()`。现在 daemon 侧统一做确定性包装（`return x` / 裸表达式 / 多语句 + await 三种写法语义一致），扩展侧也不再对含语句级 `return` 的代码做自动补全。
- **`exec --monitor` 吞掉页面脚本异常**：页面里抛错时命令仍返回 `ok:true`、退出码 0，调用方以为执行成功。现在照常上抛失败。
- **`exec --frame` 失败会静默落到主框架**：指定 frame 时若 frame 不存在，原先会走 CDP 回退并在主框架执行成功，看起来「执行了但结果不对」。现在带 frame 目标时禁用 CDP 回退，直接报 `No frame with id N`。
- **`exec --timeout` 与 `scan --timeout` 真正生效**：`exec` 默认上限从写死的 15 秒改为 60 秒（可按需放大到 600 秒），超时返回 `ok:false` + `exec_timeout`；`scan` 页面抓取超过上限返回 `scan_timeout`，CLI 侧超时比服务端多留余量，避免只看到 reqwest 的裸超时错误。
- **页面动作失败仍返回 `ok:true`**：`click` / `fill` / `hover` / `select` / `check` / `scroll` 命中不到目标时，页面动作返回 `{ok:false,error}`，但命令信封仍是成功。现在会向上抛出失败，无 eval 回退路径也补上了 `ensure_page_action_ok` 检查。
- **`send-keys --target`（无 `--frame`）的 focus 失败被吞**：按键照发并返回成功，实际打到了当时聚焦的元素。现在如实报错。
- **页面内点击重复触发**：`__abcAct('click')` 手动 dispatch 了一次 click 又调用 `el.click()`，页面会收到两次点击，提交类按钮被重复触发。现在只走原生 `el.click()`。
- **组合键中途失败时修饰键永不抬起**：此后该标签所有操作都等同按住 Ctrl。现在无论成败都释放。
- **注入失败信息误导**：CDP 回退失败时只报 `CDP fallback failed`，把真正的注入错误盖住。现在会同时带上首次注入的错误，并在页面返回 null 时再做一次探测，区分「页面 CSP 禁止 eval」和「注入被拒绝」。
- **错误信息嵌套**：CDP/扩展错误常是 `{"error":{"message":...}}` 形式的字符串，之前整段塞进提示；扩展再包一层时还会出现 `{\"error\":\"...\"}`。现在统一剥到最内层可读 message，CDP 与 DOM 回退都失败时给出合并说明。
- **`read` / `scan` 的 `--max-nodes` 被悄悄抬高**：给小于 1000 的值时被静默改成 60000，用户想省 token 反而拿到上限。现在按传入值生效。`--max-chars` 超出范围时也不再无声改写（部分路径仍会收到服务端上限，但不会出现「给了一个值、拿到另一个值却毫无提示」）。
- **超大页面 `scan` 近乎卡死**：26 万元素页面上，`optHTML` 逐节点 `getComputedStyle` + 整棵 DOM 序列化会让 `scan` 超过 230 秒仍未返回。现在 `optHTML` 有节点预算、元素数 > 5 万时跳过布局读数、> 6 万时 `scan` 直接返回文本；`scan --text-only` 改走 `page_read` 的单遍 TreeWalker（4.7s → 0.3s）。
- **`wait` 在 CSP 页面必然超时**：旧实现每 100ms 注入一次 eval 脚本，CSP 页面每次都失败，2 秒内白跑 46 次。现在改用页面内 MutationObserver，一次注入完成，同时也修掉了「后台标签页定时器被节流」的老问题。
- **后台标签页 `--wait-js` 必然超时**：等待循环从页面内 `setTimeout` 改为 daemon 侧轮询（隐藏标签页定时器会被 Chrome 节流到秒级/分钟级），输出形状保持兼容，新增 `wait.polls`。
- **`wait` 会堵住同一标签**：等待期间不再持有会话锁，`wait --js` 跑 30 秒时同标签的 `read` / `click` 仍能正常执行。
- **同标签并发互相干扰**：同一标签上并发注入多个脚本时，后一个可能打断前一个的上下文，现在每会话串行排队。
- **`@e` 引用跨导航直接失效**：原先只要 URL 变化就报 `ref expired`。现在按 role+name 在新页面自动重定位（最多扫 600 个可操作元素），成功后返回 `ref_relocated:true` 与新的 `ref_selector`；确实找不到才报 `ref_expired`，并说明旧/新 URL。SPA 重渲染导致 backendNodeId 失效时也会走同一套重定位。
- **`page_element_count` 的缓存可能存成「新 URL → 旧计数」**：导航竞态下会读到过期结果，现在按 URL 校验缓存。
- **`offset + limit` 无上限校验**：debug 构建可 panic，release 回绕导致分页错乱。
- **5 处 `tab_id` 解析失败静默用 0**：用户看到的是 `tabId is required` 而非真实原因。
- **`screenshot` / `save-pdf` 不校验产物**：0 字节或损坏的文件仍返回成功、退出码 0。现在校验非空 + 魔数 + 最小长度。
- **`extension_control` 硬编码 30s**：忽略调用方 timeout，`scan --timeout 300` 在 CSP 回退通道上被截断到 30s。
- **`--monitor` 的 MutationObserver 在动作提前返回时泄漏**：错误路径不回收观察器，长跑会话里持续占用。
- **`downloads wait` 的超时 / 中断只体现在内层 `ok`**：外层仍是 `ok:true`，CLI 无影响但 HTTP 直连方会误判。
- **长跑 `exec` / `scan` 期间 daemon 自己退出**：空闲判定只数后台 job 数量，前台长命令不算在内，daemon 误判空闲后退出，命令被腰斩。现在在途命令同样计入。
- **`close` 失败会丢会话**：以前拿到扩展确认前就删会话，关闭失败时既丢了会话状态，又让 `default_session_key` 指向已删除的会话。现在确认后才清理。
- **`close` 失败无条件标 `disconnected`**：网络抖动时用户看到「标签未连接」但浏览器里明明开着。
- **会话级缓存只增不减**：`snapshots` / `page_scale` / `session_locks` 不随会话销毁，长跑 daemon 的内存单调上涨；超过 10 分钟仍断开的会话也一直留着。现在随会话一并回收，断开的会话连同 snapshot 缓存一起清理。
- **迟到 / 重复 `ack` 永久驻留**：ack 集合只增不减，长跑 daemon 缓慢泄漏。现在超出等待窗口的 ack 直接丢弃。
- **`inflight` 没有上限也没有 TTL**：某个 handler 永久挂住时（扩展失联 + 长 `--timeout`），daemon 再也不会空闲退出。现在超过 2× 空闲 TTL（不少于 30 分钟）视为卡死放行，并打日志说明已挂时长。
- **`DriverState` 全局锁拖慢结果回传**：`pending` / `acked` / `active_exec_sessions` 拆到独立的 `ExecRegistry`（新增一把锁），Ack/Result/Error 不再和会话读写抢同一把锁；超时 / 断连 / 关标签三处清理统一收口到 `register` / `take` / `take_session` / `retain_sessions`。
- **`job cancel` 对 `wait` 现在真能停**：`wait --js` 以前把剩余时间整段注入页面，取消要等整段返回才被看见（30s 的等待取消后仍然跑满 30s）。现在可取消的任务按 1s 切片，取消约 1s 内生效，并如实标为 `cancelled`；同步调用不受影响（仍是一次注入）。
- **job 淘汰与取消会破坏已有结果**：淘汰按 HashMap 无序删除，可能删掉刚完成的结果，且全 Running 时无上界；`job cancel` 在完成窗口内会覆盖终态、丢弃已产生的结果。现在淘汰有上界且不删终态，取消不覆盖已完成的记录。
- **`--browser` / `--profile` 选会话依赖 HashMap 顺序**：回退 `matches.first()`，同一条命令重启前后可能打到不同标签。现在按稳定顺序选取。
- **`profile-label` 查重与写入之间的竞态**：两个 profile 可挂同一 label；`session_locks` 还会为已消亡的会话新建条目。
- **并发串台**：删除 `execute_raw_js_on_tab` 里「临时改写全局 `default_session_key` → 执行 → 还原」的做法，改为显式携带 session_key；`get_html` 同样改为显式会话。后台 job 与前台命令并发时不再互相覆盖路由。
- **`open` 后第一条命令可能打到旧标签页**：`open` 等待新会话就绪（最多 3s），`select_tab` 优先使用待生效的默认会话。
- **扩展断开时命令不再干等**：daemon 在 WebSocket 断开时把挂在这些会话上的 pending 命令以 `extension_disconnected` 失败，不再让 CLI 等 15~60 秒超时（实测 66ms 返回）。
- **扩展发送失败后命令空等**：WebSocket 发送失败时没有回收在途请求，命令只能等到超时。现在发送失败立即收口并返回错误。
- **冷启动 token 缓存 bug**：等待 `/ping` 时不再把旧 token（或 `None`）固化进进程缓存，避免 daemon 刚启动后首条业务请求 401。
- **token 文件异常时误判 daemon 未启动**：新增免 token 的 `/ping`，401 不再触发「重复拉起 daemon + 白等 15 秒」；token 不匹配时给出明确错误。空 token 文件也不再让命令直接失败。
- **WS 端口绑定失败被吞**：绑定失败写入状态，并在 `/ping`、`/health` 暴露 `ws_bind_error`，`doctor` 不再误报「端口已监听」。
- **daemon 随调用方一起被回收（Windows）**：拉起 daemon 时补 `DETACHED_PROCESS` 与 `CREATE_BREAKAWAY_FROM_JOB`（不允许 breakaway 的环境会自动退回），避免在带 Job Object 的终端 / CI 里 CLI 一退出 daemon 就消失、每条命令都冷启动。
- **Windows 上 CLI 卡死**：CLI 持有启动锁时拉起 daemon，子进程继承锁文件句柄导致后续命令永久阻塞；现在对锁文件与 std 句柄关闭继承。
- **截图 / PDF 默认路径**：不再硬编码 `/tmp`，改用系统临时目录（Windows 不再落到当前盘符根目录）。
- **调试指令泄漏到对外文案**：`chat` / `monitor` 相关错误里混入了只该给模型看的指令，对外部用户无意义，已中性化。
- **`tabs` 的 `connected_at` 恒为 null**：现在按扩展上报的连接时间填写。
- **扩展 service worker 被回收后页面请求永久挂死**：`debugSessions` 只在内存，SW 被杀即空；但 CDP 侧的请求已暂停。新 SW 被 `Fetch.requestPaused` 唤醒时找不到 session 就直接 `return`，没有任何 continue/fulfill/fail，页面一直转圈。现在该分支按阶段强制放行；并把每标签的规则落 `chrome.storage.session`，冷启动时对已 attach 的标签重建会话并重新 `Fetch.enable`。`ext_ready` 新增 `restartSeq`，daemon 能识别扩展重启。
- **`Fetch.enable` 抛错后状态错乱**：停留在「`enabled:false` 但 `rules:N`」，原有拦截静默全失效。现在失败即回滚状态。
- **扩展 `connectWS` 不自愈**：`await loadWsPort()` 在 try/catch 之外，storage 被写坏后永久断连；`onopen` 的 async 无 try/catch，任一 API 失败则 `ext_ready` 永不发出（daemon 已 accept 却等不到就绪）。现在两处都纳入错误处理并重试。
- **keepalive 把 `CONNECTING` 当成断开**：丢弃仍在连接的 socket，之后「实际连着却发不出响应」。
- **`scheduleTabsUpdate` 用内存 `setTimeout` 去抖**：SW 被杀则该次更新永久丢失。
- **`handleCookies` 对非 http(s) 页面抛 `TypeError`**；`handleNetworkDetail` 在未 attach 时不区分「未采集」与「取不到」。
- **扩展 WS 空指针崩溃**：`[TMWD-WS] message parse error TypeError: Cannot read properties of null (reading 'send')`。命令执行中途遇到 daemon 重启或插件重载时，`onclose` 会把全局 `ws` 置空，后续 `ws.send` 抛异常且该命令永远没有回复。现在每条连接持有自己的 socket，发送统一走 `socketSend` 安全封装，`onclose` 只清空属于自己那条连接的全局引用。这条报错不会因为改文件而消失——Chrome 里跑的始终是加载时那份代码；`agent-browser-cli status` 显示 `extension.stale: true` 时需到 `chrome://extensions` 点一次「重新加载」。
- **扩展收到「有 id 但 code 无效」的消息时不回任何响应**：daemon 空等到超时，现在回同样的 error 形状。
- **`smartProcessResult` 序列化失败仍算成功**：失败时返回字符串 `'[无法序列化: ...]'` 且外层 `ok:true`，改为 `ok:false`。
- **扩展 attach 报错不可读**：页面内嵌其他扩展 iframe 时 Chrome 会拒绝 attach（crbug 40753571），错误信息现在附带可执行建议。
- **`npm run sync-version` 在 Windows 上必炸**：`new URL(...).pathname` 缺 `fileURLToPath`，路径拼成 `D:\D:\...`。此前只在 ubuntu 上跑，所以长期潜伏。
- **`npm ci` 直接失败**：lock 顶层版本停在 0.3.6、5 个平台包条目没有 `version` 字段。现在 lock 与 `package.json` 同步生成并由脚本校验。
- **win32-arm64 三处一致缺失**（bin 映射 / optionalDependencies / publish matrix）：现在明确返回可操作错误（说明本平台无预编译包、可用 `cargo build --release` 自编并设置 `AGENT_BROWSER_CLI_BIN`），README 平台表同步。

### 安全

- **WebSocket 身份加固（默认不启用）**：`<用户配置目录>/ws-trust.json` 支持 `extension_ids`（白名单）与 `token`（扩展握手时带 `?token=`）。两者都为空时保持「装扩展即用」的兼容行为，不配置不影响任何现有用法。
- **扩展侧支持 `chrome.storage.local.wsToken`**：配置后自动带 token 连接；URL 日志里的 token 脱敏为 `token=***`。
- **`ws-trust.json` 解析失败不再降级为「任意扩展可连」**：以前写坏一个尾逗号，整套白名单与 token 校验立刻失效，而 daemon 在后台看不见 stderr。现在配置损坏即拒绝所有握手（503），`/health` 的 `ws_security.trust_config_error` 给出具体解析错误，与「从没配过」可以区分。
- **空的 `ws-trust.json` 被当成「未配置」**：手工 `> ws-trust.json` 或编辑器保存中断会静默回到「任意扩展可连」。现在「存在但为空」按损坏处理（daemon 拒绝握手），`--list` 也报同一处错误；写入类命令（`<id>` / `--token` / `--clear`）仍可直接覆盖，不至于把自己锁死。
- **`ws-trust.json` 写入非原子**：Windows 分支先删后改名，中途失败或断电则文件消失，daemon 把 `NotFound` 判为「从没配过」，回到任意扩展可连。现在复用 `config::replace_file_atomically`（backup → rename → 失败回滚），读取侧也走 `read_with_backup`。
- **只改 token 踢不掉已连接的扩展**：`/reload-trust` 原先只看白名单，token-only 配置（白名单为空 + 有 token）时恒不撤销，用户以为「加了 token 就锁死了」，但那条可疑连接还活着。现在握手时把 token 指纹随连接登记，改 token 即按 `token_mismatch` 断开（实测换 token 后连接 2.5 秒内被关），回包的 `revoked[]` 新增 `reason`（`untrusted_id` / `token_mismatch` / `corrupt_config`）。
- **信任变更即时生效**：`trust-extension` 之后调用 `POST /reload-trust`，重读信任配置并对已建立连接复核，不可信连接在下一次轮询（≤500ms）内收到 Close 并被断开；此前已连上的不可信扩展会被真正踢掉，不必等它自己断开或重启 daemon。
- **`trust-extension` 的错误归因**：以前把所有 reload 失败都说成「未连接到 daemon」，daemon 在跑但 token 不匹配（401）、配置损坏（503）时会被引向错误方向。现在区分四种结果，失败时原样带出原因，并给出机器可读的 `reload_applied` / `reload_error`。
- **popup 一打开就把当前页全部 cookie 静默写进系统剪贴板**（含 httpOnly 会话 cookie）：改为显式「复制 cookies」按钮，默认排除 httpOnly / secure，复制后给出提示。
- **加固是单向的**：`trust-extension` 只约束「谁能连 daemon」，不约束「扩展连的是谁」，daemon 空闲退出后端口可能被本机其它进程抢占；双向握手需要协议升级，README 与 popup 均已如实说明，边界见 `SECURITY.md`。

已知限制见 SECURITY.md。

## v0.3.7 - 2026-08-02

### 修复

- Chrome 扩展不再全局移除页面 CSP 响应头或 meta 标签；升级时会清理旧版持久化的动态规则，避免与 Tampermonkey 等注入脚本共同导致 YouTube renderer 满载卡死。受 CSP 限制的页面执行继续使用现有 CDP fallback。

## v0.3.4 - 2026-05-19

### 修复

- 修复 daemon 非 graceful 退出后扩展侧 CDP attach 状态残留，导致后续 exec/CDP fallback 报 `Another debugger is already attached` 的问题。

### 调整

- 调整 `agent-browser-cli` skill 的默认操作流程：浏览器任务优先直接执行 `tabs` / `open` / `exec` / `scan`，由 CLI 按需自动启动 daemon。
- 明确 `daemon_not_running` / `running=false` 只是未常驻状态，不能在尚未执行目标命令前阻塞任务。
- 将 `status` / `doctor` / `logs` 收敛为目标命令失败、连接异常或用户明确排障时才使用的诊断入口。

## v0.3.3 - 2026-05-18

- `tabtree` 默认改为 compact 输出，截断长 URL 并省略 `session_key` 以降低 token 消耗；新增 `tabtree --full` 输出完整 URL 和 `session_key`。
- 优化 `tabtree` 实现，daemon 锁内只复制必要会话字段，锁外完成排序和 JSON 组装。
- Chrome 扩展 popup 增加 label 唯一性提示：推荐用 CLI 设置 label 以校验当前 daemon 内跨 Profile 唯一性。
- 平台 npm 包打包时默认对复制后的原生二进制执行 `strip`，减小发布体积；本地 `target/release` 二进制保持不变。
- 新增 `tabtree` 树形查询命令，支持按 `tab_id`、`profile_id/profile_label`、`browser_id` 过滤，并保留 browser → profile → tab 父子节点；原先临时增加的 `profiles` / `browsers` 摘要命令已移除，统一使用 `tabtree` / `lookup`。
- 新增 `lookup tab|browser|profile` 反查命令，可由 `tab_id` 反查 `browser_id` / `profile_id` / `profile_label`，或由 `browser_id` 反查所属 profile。
- 移除扩展默认注入的全局 `alert` / `confirm` / `prompt` 重写，改为 CLI 页面执行期间临时抑制弹窗并在命令结束后恢复原生函数。
- 新增 `profile-label set|clear`，并在 Chrome 扩展 popup 中支持设置 Profile Label；label 冲突时 CLI 按歧义处理，不参与内部路由主键。

## v0.3.2 - 2026-05-17

### 修复

- 修复长 URL 页面中 `snapshot` 后立即使用 `@e` 可能误报 `ref expired` 的问题。

## v0.3.1 - 2026-05-17

### 新增

- 新增 `logs`：读取 `~/.agent-browser-cli/logs/daemon.log`，支持 `--tail`。
- 新增 `doctor`：输出 JSON 诊断信息，不自动启动 daemon。
- 新增 `install-skill`：支持 `--dry-run` 和 `--yes`，用于安装/更新 Agent skill。
- 增强 `status`：新增 `healthy`、`summary`、`message`，保留旧字段。
- 新增 `snapshot`：基于 Chrome Accessibility Tree 生成 `@e` 操作引用，支持 `--offset`、`--limit`、`--details`、`--tab`。
- 新增高层操作命令：`click`、`fill`、`send-keys`、`mouse-click`，支持 `--monitor`、`--wait-js`、`--wait-timeout`。
- 新增 `screenshot`：支持视口截图、全页截图、元素截图、PNG/JPEG、默认落盘目录。
- 新增 `save-pdf`：支持纸张、横向、缩放、打印背景、默认文件名清理和 50MB 上限。
- 新增 `network` 调试命令：`start`、`list`、`detail`、`clear`、`stop`。
- 新增 `console` 调试命令：`start`、`list`、`clear`、`stop`。
- 新增多浏览器 / 多 Chrome Profile 会话隔离：扩展上报 `browser_id`、`profile_id`、`profile_label`，daemon 内部使用 `session_key = browser_id:profile_id:tab_id` 路由。
- CLI 主要浏览器命令新增 `--browser` 和 `--profile`，支持按浏览器实例和 Profile 过滤/定位 tab。
- `tabs` 输出新增 `browser_id`、`profile_id`、`profile_label`、`tab_id`、`session_key`。
- `open` 新增 `--session` 和 `--group-title`，支持 Chrome 原生 tab group。
- `open` 新增 `--window`，支持新开独立 Chrome 窗口。
- `open` 新增 `--focus`，只有显式传入时才请求聚焦窗口。
- `open --window --group-title/--session` 支持对新窗口首个 tab 创建 Chrome tab group。
- 新增 `close --tab <tabId>`，通过扩展原生 `chrome.tabs.remove` 关闭标签页。
- Chrome 扩展新增图标，显示名改为 `Agent Browser CLI Bridge`。

### 调整

- `open` 返回结构改为以新打开目标为主：`opened_tab_id`、`opened_session_key`、`window_id`、`window`、`group`。旧执行通道移入 `metadata.executor`，避免 AI 误用旧 tab。
- daemon 在默认 session 失效时会回退到当前 active session，避免重启后卡在旧 `session_key`。
- `snapshot`、`@e` 缓存、调试缓存、截图/PDF、network/console 等路径统一按 `session_key` 隔离。
- daemon stdout/stderr 日志固定写入用户目录，不再落到当前目录或 npm 包目录。
- npm wrapper 设置 `AGENT_BROWSER_CLI_PACKAGE_DIR`，方便全局安装后定位内置 skill。
- `network stop` / `console stop` 会停止监听并清理对应缓存。
- daemon 退出时清理 daemon 缓存，并通知扩展清理 network/console 调试缓存。
- skill 文档重写为 Agent 操作 SOP，明确 `scan`、`snapshot`、`exec/CDP` 的职责边界，并补充多 Profile、多浏览器、`open --window`、`--focus` 和 `opened_tab_id` 使用说明。

### 验证

- 已覆盖真实 Chrome 场景：百度搜索、页面扫描、DOM 定位、输入/点击、截图、PDF、network/console 监听、缓存清理、tab group、关闭测试 tab。
- 已覆盖多 Profile 字段上报、`tabs --profile`、`tabs --browser --profile`、`exec/scan/snapshot/click/fill/send-keys/mouse-click/screenshot/save-pdf/network/console --profile --tab`、`open` tab/window/background/focus/group/session 组合。
- 已执行 `cargo fmt --check`、`cargo check`、`cargo build`、`cargo test`、`node --check`、`npm pack --dry-run`。

## v0.2.10-extension - 2026-05-15

### 调整

- Chrome 扩展默认不抢占系统前台窗口。
- `Page.bringToFront` 这类可能聚焦浏览器的行为改为显式 `allowFocus=true` 才执行。

## v0.2.9 - 2026-05-15

### 修复

- 同步平台包版本，保证 npm 包与 Rust crate 版本一致。

## v0.2.8 - 2026-05-15

### 新增

- 支持自定义 Chrome 扩展 WebSocket 监听端口。
- 新增 `set-extension-port <port>`，写入 `~/.agent-browser-cli/config.json`。
- daemon 运行中修改端口时自动重启，使新端口立即生效。

## v0.2.7 - 2026-05-15

### 调整

- 发布版本 `0.2.7`。
- 整合 release/publish 流程，为后续多平台包发布做准备。

## v0.2.6 - 2026-05-15

### 新增

- 支持 Linux ARM64 平台包。

### 文档

- 更新 Chrome 扩展安装说明，推荐从 Release 下载 `chrome-extensions.zip`。
- 补充 Linux 与 WSL 使用前提。

## v0.2.5 - 2026-05-13

### 调整

- 准备 `0.2.5` 发布。
- 更新支持平台说明。
- 改进 Chrome 扩展右侧提示角标，并补充文档说明。
- 增加 GenericAgent 友情链接。

## v0.2.4 - 2026-05-13

### 调整

- 准备 `0.2.4` 发布。
- 改进 release notes 保留逻辑。
- 增加 Linux 构建验证。

## v0.2.3 - 2026-05-12

### 修复

- 修复 Release npm artifact 匹配逻辑，让发布产物查找更稳健。
- 更新 `Cargo.lock` 到 `0.2.3`。

## v0.2.2 - 2026-05-12

### CI

- npm token 不存在时跳过 npm publish，避免发布流程失败。
- 增加 npm 发布 GitHub Actions 工作流。
- 调整 macOS Intel runner。

## v0.2.1 - 2026-05-11

### 移除

- 移除旧 Python runtime，CLI 主链路切换到 Rust 实现。

## v0.2.0 - 2026-05-11

### 新增

- 新增 Rust npm CLI release。
- 通过 npm 包分发 Rust CLI。

### 文档

- 补充性能参考和延迟数据说明。

## v0.1.1 - 2026-05-11

### 修复

- CLI 关闭时主动关闭 bridge，避免连接残留。

### 文档

- 补充安装指引，强调替换项目路径占位符。

## v0.1.0 - 2026-05-09

### 新增

- 新增 `open` 命令。
- 支持通过 Chrome 扩展原生创建标签页。
- 建立基础浏览器控制 CLI 能力。
