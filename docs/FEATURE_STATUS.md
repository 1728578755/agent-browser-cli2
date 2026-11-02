# 能力现状：已实现 / Roadmap / 边界

适用版本：扩展 3.4，CLI 0.3.8

这份文档回答三个问题：现在能做什么、Roadmap 有哪些方向、什么做不到。

---

## 1. 已实现

### 会话与标签

| 能力 | 命令 | 说明 |
|---|---|---|
| 列出标签 | `tabs [--full] [--browser] [--profile]` | 默认截断 URL，`--full` 给完整 URL |
| 标签树 | `tabtree` | 按 browser/profile 分组，省略 session_key 省 token |
| 定位单个标签 | `lookup tab/browser/profile` | 精确查询 |
| 打开 | `open <url...> [--background] [--focus] [--group-title]` | **支持一次给多个 URL 并发打开**，返回 `elapsed_ms` |
| 关闭 | `close --tab <id>` | |
| 默认路由 | 任意命令省略 `--tab` | 命中最近一次 `open` 的标签；并发任务用显式 `--tab` |

### 页面读取

| 能力 | 命令 | 返回 |
|---|---|---|
| 结构化读取 | `read [--selector] [--max-chars] [--links] [--no-forms] [--include-hidden] [--budget-ms N] [--max-nodes N] [--frame N]` | 标题/URL/正文/元信息/标题层级/各类计数/表单结构；单遍遍历 + 时间预算，超大页面不卡 |
| 隐藏文本 | `read --include-hidden` | 走 `textContent` 语义，把 `display:none` / `hidden` / `aria-hidden` / CSS 隐藏的内容也返回；默认仍只返回可见文本（`text_mode` 标明 `visible` / `visible-approx` / `all`） |
| 元素查找 | `find <selector>` / `find --text <文本>` / `find --xpath <expr>` | 命中数 + 每个元素的可操作信息（tag/text/value/href/rect/disabled/checked/selector） |
| 页面扫描 | `scan [--tabs-only] [--text-only] [--fast] [--max-nodes N]` | 精简 HTML、纯文本或快速文本；超大页面（>6 万元素）自动改走文本并注明 |
| 页面网络 | `net fetch <url> [--method] [-H k:v] [--body] [--response-type]` | 在页面上下文发请求（带 cookie / 同源策略 / 页面 CSP 视角），返回 status/headers/body/json |
| 网络 hook | `net hook [--capture-bodies] [--persist] [--match <pattern>]` / `net log` / `net reset` | 包装 `fetch` 与 `XMLHttpRequest`，记录 URL/方法/状态/耗时；`--persist` 把「库 + 规则」注册成 document_start 注入，**刷新/跳转后自动重装** |
| 网络 mock | `net mock <片段\|正则> [--status N] [--body JSON] [--method GET] [--abort]` / `net routes` / `net unroute` | 命中规则的 `fetch`/XHR 直接返回伪造响应（或网络错误），不真的发出去；规则按 **profile 共享**，同 profile 的新标签自动继承；`net unroute --engine page\|cdp` 可单独删除某一类规则，CDP 放行默认不写 `net log` |
| 网络层拦截 | `net mock <pattern> --engine cdp [--response-status N] [--response-body B] [-H 'k: v']` | 走扩展 `chrome.debugger` 的 `Fetch` 域，能改 **blob / 二进制体**、也能拦 `<img>` / `<script>` / JSONP；不换 body 时用 `continueResponse` 原地放行；CDP 被占用时明确报错且不落库。干净 profile 真机 15 条断言通过（`tests/live_cdp_net.mjs`） |
| 挑战页/验证码 | `challenge status` / `challenge wait` / `challenge solve` / `challenge focus` | 识别 CF/Cloudflare Turnstile/reCAPTCHA/hCaptcha/PerimeterX/Akamai/DataDome/AWS WAF 与通用验证码，滑块/点选类（阿里云 noCaptcha、极验、腾讯防水墙、顶象、数美）单独标 `kind:"slider"`；`wait` 等自动放行（页面内 MutationObserver，4 秒一片重注入），超时前可自动把标签页带到前台交给人工。**不做自动破解**（不打码平台、不自动点复选框、不模拟滑块/点选）；`solve` 是「自动放行 / 等 token / 转人工」的接力：CF managed 与隐形 Turnstile 会自己通过并产出 token，需要人点时聚焦标签页、人完成后立即返回 `passed:true`。`read` 命中挑战特征时会附带 `challenge` 字段 |
| 请求改写 | `net mock <pattern> --request-header 'x-a: 1' --request-body JSON` | 放行真实请求，但按规则改请求头 / 请求体（GET/HEAD 不写 body）；日志标记 `request_rewritten` |
| 响应改写 | `net mock <pattern> --response-status 201 --response-body JSON [-H 'x-a: 1']` | 放行真实请求后替换状态码 / 响应体 / 响应头；**fetch 与 XHR 都可靠**（XHR 走访问器接管，与页面监听器注册顺序无关），支持 `text` / `json` / `arraybuffer` / `document` / `blob`（blob 需规则显式给 body） |
| 结果限流 | `exec --max-bytes N` | 返回值超限时给截断预览（`truncated:true`），避免一条命令塞满上下文 |
| 脚本注入 | `inject <code> [--file] [--world main\|isolated] [--frame N] [--all-frames]` | 一次性注入，语义同 `exec` |
| 持久注入 | `inject --persist <name> [--match <pattern>] <code>` + `--list` / `--remove` | 存代码；注册 `persist_boot.js` 在 **document_start** 生效，注册失败时退回导航后重放 |
| iframe 列表 | `frames [--tab]` | 返回 frame_id / parent_frame_id / url，配合 `--frame` 使用 |
| 快照 | `snapshot [--js] [--interactive-only] [--max-nodes N] [--details]` | 可访问性树 + `@e` 引用；CDP 不可用时**自动回退 DOM 快照**（`mode:"js"`）；`--interactive-only` 只留可操作元素，`--max-nodes` 截断大树 |
| 引用重定位 | 任意 `@e` 操作 | 页面导航 / SPA 换页后，`@e` 会按 role+name 在新页面**自动重定位**并返回 `ref_relocated:true` + `ref_selector`；确实找不到才报 `ref_expired` |
| Cookie | `cookies [--url]` | |
| 网络/控制台 | `network *` / `console *` | 需要 CDP |

### 页面操作

| 能力 | 命令 | 实现路径 |
|---|---|---|
| 点击 | `click <target> [--frame N]` | CDP `DOM.resolveNode` + 真实事件；`@e` 来自 JS 快照或指定 `--frame` 时走页面内点击 |
| 鼠标点击 | `mouse-click <target>` | CDP `Input.dispatchMouseEvent` |
| 填写 | `fill <target> <value> [--clear] [--append] [--frame N]` | CDP；`@e` 来自 JS 快照或指定 `--frame` 时走页面内 setValue |
| 键盘 | `send-keys <keys> [--target] [--frame N]` | CDP（frame 只影响 focus 目标） |
| 悬停 | `hover <target>` | 页面内单往返（pointer/mouse 六事件） |
| 下拉选择 | `select <target> --value V` / `--label L` | 页面内单往返，触发 `input`+`change` |
| 勾选 | `check <target>` | 页面内单往返 |
| 滚动 | `scroll [<target>] [--dy N] [--to top\|bottom]` | 页面内单往返 |
| 等待 | `wait [--selector S] [--text T] [--js C] [--visible] [--frame N]` | 全部**一次注入**完成：`--selector`/`--text` 走页面内 MutationObserver（`mode:"observer"`）；`--js` 谓词在页面内编译一次、DOM 变化由观察器触发（`mode:"js-observer"`，`injections` 通常为 1） |
| 前进/刷新 | `back` / `reload` | 先返回再触发导航，避免白等超时 |
| 截图 / PDF | `screenshot` / `save-pdf` | 需要 CDP |
| 上传 | `upload <target> <files...>` | 需要 CDP |

所有 `target` 支持 CSS 选择器，`hover/select/check/scroll` 还支持 `text=可见文本` 与 `--index`。

### 批量、异步与并发

| 能力 | 命令 | 语义 |
|---|---|---|
| 串行管道 | `run <steps.json>` | 一次进程多步，默认遇错即停（适合有依赖的流程） |
| 并发批量 | `batch <steps.json> --concurrency N` | 一次请求内最多 N 个并发（适合跨标签批量读取/操作） |
| 异步任务 | `--async <cmd>` + `job list/get/wait/cancel` | 立即返回 job_id，daemon 内继续执行 |
| 单次分发 | `open a b c` | 客户端并发打开多个标签 |
| 浏览器清单 | `browsers` | 列出所有 (browser_id, profile) 与代表标签，多浏览器编排前先看这里 |
| 跨浏览器广播 | `broadcast <op> [--script S\|--args JSON] [--browser B] [--profile P] [--concurrency N]` | 一次在**所有**匹配的 browser/profile 上并发执行同一操作，逐目标返回结果与耗时 |
| 下载监听 | `downloads [--limit] [--state] [--id]` / `download <url> [--filename] [--dir]` / `download-wait [--id] [--timeout]` | 扩展 `chrome.downloads`，list/start/wait；wait 在 daemon 侧轮询 |

同一标签的命令在 daemon 内**串行排队**（每会话一把异步锁），不同标签各自并发；这样同一标签 50+ 并发不会互相踩踏，也不会拖慢其它标签。

`batch` 的可用 op：`exec / read / find / action / wait / scan / snapshot / click / fill / mouse-click / send-keys / upload / screenshot / save-pdf / cookies`。

### 无 eval 通道（严格 CSP 页面）

`chrome.scripting.executeScript` 注入函数本身不受页面 CSP 限制，但函数内部 `eval` 会。于是给固定操作准备了一条不 eval 的路径：扩展注入 `ops/page_*.js`（与 daemon 侧同源），再调用其全局函数。

> `ops/` 与 `assets/` 两处副本由脚本维护：`npm run sync:extension` 生成/更新副本，`npm run check:extension` 在 CI 里守卫一致性。改过 `assets/page_*.js` 后必须跑一次 sync，否则扩展实际注入的还是旧副本。

| 能力 | 走法 | 说明 |
|---|---|---|
| `read` / `find` / `snapshot --js` / `action`（hover/select/check/scroll/click/fill） / `wait` | 先 eval 快路径；命中 CSP 错误时自动切 `pageOp` | 返回里带 `via:"no-eval"`（snapshot 仍是 `mode:"js"`） |
| 任意 `exec` / 页面变量读取 | 不可用 | 需要 eval，CSP 页面无解；错误会明确说原因 |

实测 GitHub 首页（CSP 严格 + debugger 被其他扩展 iframe 挡住）：`read` 返回 269 字正文、`find` 命中 19 个链接、`snapshot --js` 给出 5 个 `@e`、`hover`/`scroll`/`wait` 全部可用。

### 扩展自更新与诊断

- `status`：`extension.stale` / `warning` 直说浏览器里跑的是不是最新扩展。
- `doctor`：`extension_fresh` 等检查项。
- daemon 发现扩展版本落后会主动发 `management.reload`（同版本只发一次）。
- 扩展每 60 秒比对 `background.js` 哈希，磁盘有改动且空闲时自我重载。
- popup 有「运行版本 + 重新加载扩展」按钮。
- 诊断命令：`injectProbe`（三种注入路径）、`selfCheck`（哈希基线/定时器状态）。

---

## 2. 性能数据

环境：Windows + Chrome（140+），本地 daemon，真实浏览器，页面为 `example.com`。
测量工具：`node tests/concurrency_bench.mjs --tabs N --rounds M`。

| 场景 | 实测（多次运行区间） |
|---|---|
| `open` 12 个标签（并发） | 0.37 ~ 1.35 s（首次加载 vs 命中缓存） |
| 单标签一次 `exec`（含 CLI 冷启动） | 89 ~ 254 ms（冷启动 daemon 占大头） |
| 60 步（12 标签 × 5 轮，read 4KB + exec 15ms 忙等）并行 `batch(concurrency=12)` | **82 ms**，失败 0 |
| 同样 60 步串行 `run` | 542 ms（各步合计） |
| 加速比 | **6.6x** |
| 60 步纯 `exec 'return 1'`（轻负载）并行 / 串行 | 6 ms / 8 ms —— 轻负载已到噪声底，加速比无意义 |
| 每步平均（并行，warm） | 2 ~ 4 ms |
| 同一标签 12 条并发命令 | 38 ~ 48 ms，失败 0 |
| `read` 单次（warm daemon） | 7 ~ 115 ms，取决于正文大小 |
| `exec --monitor` 额外开销 | 约 1 ms（只记录变更，不做整页序列化） |
| `wait --selector`（800ms 后出现） | **856 ms 命中，2 次检查** |
| `wait --selector`（已存在） | 20 ms 命中，1 次检查 |
| `wait --selector` 超时（2s） | 2585 ms 返回 `matched:false`，1 次注入 |
| `wait --js`（条件已成立） | **5 ms 命中，1 次注入，1 次求值** |
| `wait --js`（DOM 800ms 后出现） | **673 ms 命中，1 次注入** |
| `wait --js`（定时器条件 800ms） | 722 ms 命中，1 次注入，15 次求值 |
| `wait --js` 超时（0.8s） | 868 ms 返回 `matched:false`，1 次注入 |
| `wait --js` 跨导航 | 2 次注入后命中（旧页面销毁后按剩余时间自动重试） |
| `exec --wait-js`（条件已成立） | 0.4 ms 页面内命中，1 次注入 |

超大页面（`tests/gen_huge.mjs`，26 万元素 / 6 MB HTML，同一标签连续测量）：

| 场景 | 早期实现 | 当前实现 |
|---|---|---|
| `read`（默认，可见文本） | `innerText` 单次约 200 ms，再叠加 5~6 次全量 `querySelectorAll` | **270 ~ 300 ms**（单遍遍历，页面内 65 ~ 140 ms） |
| `scan --text-only` | 4.6 ~ 4.9 s（`optHTML` 逐节点 `getComputedStyle`） | **270 ~ 310 ms** |
| `scan`（HTML） | 首次超过 230 s 仍未返回（整页布局 + 序列化） | **265 ~ 345 ms**（自动改走文本，内容头部注明原因） |
| `scan --fast` | — | **285 ~ 350 ms** |
| `exec 'return 1'` | — | 190 ~ 220 ms（warm） |

机制：`read`/`scan` 不再做多次全量查询；`optHTML` 有节点预算与布局跳过；页面元素数 > 6 万时 `scan` 直接返回文本而不是序列化整棵 DOM。元素数按会话缓存 2 分钟，避免重复探测。

几点说明：

- 串行瓶颈不在页面执行，而在「每条命令一次 CLI 冷启动 + 一次 HTTP」。所以**批量场景必须用 `batch`/`run`**，不要用 shell 循环调 CLI。
- 并发收益取决于单步是否有实质工作：`read`/`find`/页面脚本这类每步几毫秒以上的任务能到 6x；`exec 'return 1'` 这种 1~2 ms 的步骤会被固定开销淹没，此时并行与串行差别在噪声范围内。基准脚本会先预热每个标签，并对并行取两轮较小值。
- 扩展侧对同一标签的命令是并发的（`onmessage` 不串行 await），12 条并发仍 47 ms。
- 隐藏标签页的 `setTimeout` 会被 Chrome 节流，所以等待都放进页面里由 MutationObserver 驱动；`wait --js` 的非 DOM 条件再由 `setInterval` 兜底（后台会被节流到 1s，DOM 类条件不受影响）。

---

## 3. Roadmap

以下方向都还没有实现，按「对使用者的价值」排列，不承诺顺序。欢迎在 issue 里讨论方案，或直接提 PR；标注 `good first issue` 的项适合第一次参与。

| 方向 | 现状 | 目标 |
|---|---|---|
| 加载完成语义 | `open` 只保证「会话可操作」 | `open --wait-load` 等到 `readyState === 'complete'` 且网络空闲 |
| 快照增量与分页 | 折叠已支持（`--interactive-only` / `--max-nodes`），大页面一次返回全部引用仍偏大 | 跨次调用的增量游标与分页，大页面可分片读取 |
| 下载进度 | 靠轮询 `chrome.downloads.search` | 通过 `onChanged` 推送进度事件，`download-wait` 可报百分比 |
| 单标签背压 | 同一标签的并发请求排队，但没有上限 | 可配置的并发上限与排队反馈，50+ 并发时给出明确背压 |
| 动作通道统一 | CSS 选择器的 `click`/`fill` 默认走 CDP，`@e` 与 `--frame` 走页面内 | 全部走页面内动作，减少对 debugger 的依赖，DevTools 打开时也能操作 |

---

## 4. 边界与限制

**环境限制（不是代码问题）**

1. Chrome 在页面内嵌**其他扩展** iframe 时拒绝 `chrome.debugger.attach`（crbug 40753571）。受影响：`screenshot`、`save-pdf`、`network`、`console`、`mouse-click`、`upload`。
   - `snapshot` 已自动回退 DOM 版本（`mode:"js"`），`@e` 仍可用于 `click/fill`。
2. 页面 CSP 禁止 `eval` 时（GitHub、Google 等），任意 `exec` 不可用；但 `read`/`find`/`snapshot --js`/`action`/`wait` 会**自动切到无 eval 通道**（扩展注入 `ops/page_*.js`），返回里带 `via:"no-eval"`。
3. 依赖顺序：
   - 允许 eval → 全部能力可用；
   - 禁止 eval 但允许注入（大多数 CSP 站）→ `exec` 不可用，DOM 读/查/快照/动作/等待可用；
   - 禁止 eval 且禁止注入（极少）→ 只有 `tabs`/`cookies`/`open`/`close`。
4. 持久注入优先由 `persist_boot.js` 在 document_start 执行（实测页面内 173ms 处已生效）；若浏览器不支持动态注册，会退回「导航提交后重放」，此时比 document_start 晚几毫秒。
5. 指定 `--frame` / `--all-frames` 时不再回退 CDP（CDP 只跑主框架），失败会直接报 `No frame with id N`，不会悄悄在主框架执行。

**设计边界**

- 一个浏览器 profile 一条 WebSocket：命令在扩展内并发处理，但共用一条连接，极端并发下会排队。
- `@e` 引用会跨导航自动重定位（role+name 匹配，最多扫 600 个可操作元素）；如果新页面上同名元素有多个，可能命中第一个而非原来那个；带 `--tab` 的引用不能跨标签复用。
- 无 eval 通道只覆盖固定操作；`exec` 任意 JS、读取页面变量在 CSP 页面仍然不可用。
- 超大页面（>6 万元素）的 `scan` 返回的是文本而不是 HTML（内容头部有 `<!-- scan: ... -->` 说明）；需要 HTML 时请显式 `--max-nodes` 或分块读取。
- `read` 默认只返回可见文本：元素数 ≤ 3 万时用 `innerText`（精确排除 CSS 隐藏）；更大的页面用 TreeWalker 近似（`text_mode:"visible-approx"`，能看到 `[hidden]`/`aria-hidden`/内联样式隐藏，但**看不到 class 级 CSS 隐藏**）。要完整文本用 `--include-hidden`。
- `find --text` 只返回「最内层」匹配元素（避免整段容器命中），因此不会返回包裹层。
- 批量操作失败不重试；`batch` 会跑完所有步骤并逐条报告。
- `wait --js` 的谓词在页面内反复求值：DOM 频繁变更的页面上约 60~80 次/秒（受事件循环批次限制），谓词要保持轻量（`querySelector` / 读全局变量），不要在里面做全量遍历或重计算；精确控制频率可以用 `--interval`。

**多浏览器能力边界**

- `browsers` / `broadcast` 按扩展上报的 `browser_id + profile_id` 分组，Chrome 与 Edge 各装一份扩展即可同时出现在列表里；每个 profile 取「默认标签」作为该目标。
- `broadcast` 支持 `exec / read / find / scan / net / downloads / click / fill / ...`（与 `batch` 的 op 集合一致）；每个目标上仍是串行执行。
- 扩展未安装/未连接的浏览器不会出现在 `browsers` 里；`broadcast --browser edge` 在没装 Edge 扩展时会返回 `no_target`。

**网络能力边界**

- `net fetch` 跑在**页面**上下文（MAIN world），因此受页面 CSP 的 `connect-src` 与同源策略约束；跨域请求是否成功取决于目标站点 CORS，和页面里自己 `fetch` 完全一致。
- `net hook` 默认只在当前页面生效；加 `--persist` 后会注册 document_start 注入，刷新/跳转后自动重装（`net reset` 会移除）。规则变更会自动同步到持久化注入。
- 响应改写对 `fetch` 与 XHR 都可靠：fetch 返回新的 `Response`；XHR 接管 `responseText` / `response` / `status` / `statusText` / `getAllResponseHeaders` / `getResponseHeader`，页面监听器无论注册多早都读到改写值。`responseType=blob` 在页面引擎下只有「规则显式给了 body」才能构造新 Blob（原始体无法同步读出，日志记 `blob-without-body`）；要改原始二进制体请用 `net mock --engine cdp`。
- `--engine cdp` 依赖 `chrome.debugger`：页面内嵌其它扩展 iframe 时 Chrome 拒绝 attach（crbug 40753571），此时命令会明确失败且规则不落库，退回 `--engine page` 即可；CDP 模式下的拦截记录暂不进 `net log`（日志仍来自页面 hook）。
- 验证码只做识别与等待：**不自动破解**。CF 的 JS 挑战通常几秒自动放行，`challenge wait` 足够；需要人点时用 `challenge focus` 把标签页带到前台，处理完再 `challenge wait`。打码平台/自动点复选框/滑块模拟都不提供。
- `challenge status` 会先等页面从 `about:blank`/`loading` 落到真实文档（最多 2.5s），避免 `open` 后立刻查询误判成「没有验证码」。
- 网络层能改状态码/头/字节，但**改 `<img>` 的状态码不会让图片失败**：Chrome 的图片解码只看字节与 MIME，404 + 合法图片字节仍会渲染。要让图片失败用 `--abort` 或替换字节（真机已验证字节替换生效）。
- `net mock` 对 `fetch` 与 XHR 都生效；`--abort` 会让请求以 `TypeError: Failed to fetch` 失败，用于测试错误分支。
- 下载监听走 `chrome.downloads`，只能看到**本浏览器**的下载；`download-wait` 默认等最近一条，指定 `--id` 更稳。
