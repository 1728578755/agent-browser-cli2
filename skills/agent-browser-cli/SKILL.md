---
name: agent-browser-cli
description: 使用 agent-browser-cli 进行浏览器感知与控制、页面交互、截图/PDF、Cookie/CDP 和排障。
---

# agent-browser-cli

使用 `agent-browser-cli` 控制用户真实 Chrome。底层是 Rust daemon + Chrome 扩展桥，保留登录态和 Cookie；不是 Selenium/Playwright。

## 正常流程直接执行命令

每次开始浏览器任务，不要先做健康检查，直接执行最贴近目标的命令。`tabs` / `open` / `exec` / `scan` 会按需自动启动 daemon；daemon 未常驻是正常状态，不是故障。

```bash
agent-browser-cli open https://example.com
agent-browser-cli scan --text-only
agent-browser-cli snapshot --limit 200
agent-browser-cli exec --tab <tabId> 'return document.title'
```

只有目标命令失败、输出明确提示连接异常、扩展未连接、端口不一致、无可用标签页，或用户明确要求排障时，才进入状态检查：

```bash
agent-browser-cli status
agent-browser-cli doctor
agent-browser-cli logs --tail 100
```

`status` 中 `daemon_not_running` / `running=false` 不能单独作为停止操作的理由；优先继续执行目标命令让 CLI 自动拉起 daemon。`doctor` 只检查状态，不自动启动 daemon、不改配置、不安装 skill。

补充一个容易误判的点：`status` / `doctor` 里看到 `daemon_not_running`，如果此时还没有执行 `tabs` / `open` / `exec` / `scan`，通常只是 daemon 按需常驻而未启动，不代表故障。只有目标命令已经失败，或者日志/输出明确提示端口、扩展、标签页异常时，才进入排障。

## 打开本地文件

`open` 支持 HTTP(S) URL、绝对 `file://` URL 和本机文件路径。相对路径以执行 CLI 时的当前目录为基准；支持 `~`、中文、空格和符号链接，符号链接会解析到真实文件。所有普通文件类型均可打开，目录会被拒绝。

```bash
agent-browser-cli open ./demo.html
agent-browser-cli open ~/Documents/report.pdf
agent-browser-cli open 'file:///Users/me/My%20Files/demo.html#intro'
agent-browser-cli open ./demo.html --timeout 15
```

本地文件要求 Chrome 扩展版本至少为 2.1，并在 `chrome://extensions` → Agent Browser CLI Bridge → 详情中开启“允许访问文件网址”。权限关闭时 `open` 会在创建标签前失败；`status` / `doctor` 会按 Profile 报告 `file_scheme_access`，但该可选能力不影响普通 HTTP(S) 健康状态。

输入分类规则：

- `./`、`../`、`~/`、绝对路径和显式 `file://` 表示本地文件；文件必须存在且是普通文件。
- 裸输入若对应当前目录中已存在的文件，文件优先；否则按 Web 地址处理。要强制打开网站，显式写 `https://`。
- 普通路径中的 `?` 和 `#` 是文件名字符；仅显式 `file://` URL 使用 query/fragment 语义。
- `localhost`、回环/私网 IP、单标签主机和 `.local` 的无协议地址默认使用 HTTP；其他域名默认 HTTPS；端口 80/443 分别强制 HTTP/HTTPS。
- 只支持 `http:`、`https:` 和 `file:`；其他显式 scheme 会返回 `unsupported_scheme`。
- 异平台路径会明确报错，例如 macOS/Linux 不会把 `C:\\Users\\...` 错当作网址。

本地文件 `open` 默认最多等待 10 秒，直到新标签成为可控制会话；可用 `--timeout` 调整。超时后标签会保留，错误结果包含稳定的 `error_code` 和 `opened_tab_id`。**所有 `open`（包括 `--background`）都会让新标签成为默认会话**，返回里的 `result.ready` 表示是否已可立即操作；默认前台打开，加 `--background` 才在后台创建、不切换当前标签页。

## 常用命令优先级

先区分三个入口：

```text
scan：内容感知，适合看正文、列表、页面文本。
snapshot：操作定位，适合找按钮、链接、输入框并生成 @e 引用。
exec / JSON CDP：逃生口，封装命令失效或特殊页面时回退。
```

基础感知和按需排障：

```bash
agent-browser-cli tabs
agent-browser-cli tabtree
agent-browser-cli tabtree --full
agent-browser-cli tabtree --profile work
agent-browser-cli tabtree --tab <tabId>
agent-browser-cli lookup tab <tabId>
agent-browser-cli lookup browser <browser_id>
agent-browser-cli lookup profile work
agent-browser-cli tabs --profile work
agent-browser-cli profile-label set work --profile <profile_id>
agent-browser-cli scan --tabs-only
agent-browser-cli scan --profile work --tab <tabId> --text-only
agent-browser-cli open --profile work https://example.com
agent-browser-cli open --window https://example.com
agent-browser-cli open --window --focus https://example.com
# open 返回后继续操作新页面时，优先使用 result.opened_tab_id / result.opened_session_key
agent-browser-cli open https://example.com
agent-browser-cli close --tab <tabId>
agent-browser-cli exec --tab <tabId> 'return document.title'
agent-browser-cli status
agent-browser-cli doctor
agent-browser-cli logs --tail 100
```

结构化读取与查找（优先于 scan，输出更小更准）：

```bash
agent-browser-cli read --max-chars 8000          # 标题/正文/表单/计数
agent-browser-cli exec --max-bytes 20000 '...'   # 大返回值先截断，避免塞满上下文
agent-browser-cli snapshot --js                  # CDP 被挡时用 DOM 快照（引用带 CSS 选择器）
agent-browser-cli snapshot --js --details        # JS 快照也能带 DOM 详情（按 ref 的选择器批量补一次）
agent-browser-cli snapshot --interactive-only    # 只留按钮/链接/输入框，树更小
agent-browser-cli snapshot --max-nodes 200       # 大树截断，避免占满上下文
agent-browser-cli frames                         # 列出 iframe，拿 frame_id
agent-browser-cli exec --frame <frameId> '...'   # 在指定 iframe 里执行
agent-browser-cli inject 'window.__hook = fn'    # 一次性注入（语义同 exec）
agent-browser-cli inject --persist api-hook --match '*://*.example.com/*' '...'  # 导航后自动重放
agent-browser-cli inject --list                  # 查看已持久化的注入
agent-browser-cli inject --remove api-hook       # 移除
agent-browser-cli read --include-hidden          # 连隐藏内容一起读（textContent 语义）
agent-browser-cli scan --fast                    # 超大页面只取文本（十万级元素也不卡）
agent-browser-cli scan --max-nodes 20000         # 节点上限：默认 8000，--fast 下默认 80000
agent-browser-cli net fetch <url> -H 'k: v'      # 在页面上下文发请求（带 cookie）
agent-browser-cli net hook                       # 开始记录 fetch / XHR
agent-browser-cli net log --limit 20
agent-browser-cli net mock /api/user --body '{"id":1}'   # 命中即返回伪造响应
agent-browser-cli net mock /api/user --response-status 201 --response-body '{"id":2}'  # 放行后改响应（fetch 与 XHR 都可靠）
agent-browser-cli net mock /api/user -H 'x-mock: 1' --response-body '{"id":2}'         # 改响应时一并改响应头
agent-browser-cli net mock /api/user --request-header 'x-token: test'   # 放行但改请求头（覆盖同名头，不是追加）
agent-browser-cli net hook --persist                     # 刷新/跳转后仍保持 hook 与规则（同 profile 的新标签也生效）
agent-browser-cli net mock <url> --engine cdp --response-status 201 --response-body '{"v":9}'   # 网络层拦截：能改 blob/二进制体，也能拦 img/script
agent-browser-cli net routes                             # 只读查询：页面规则 + cdp 规则（按 browser:profile 共享，不重置统计/日志）
agent-browser-cli net unroute /api/user --engine cdp      # 删除 CDP 规则（不加 --engine 只删页面规则）
# 验证码 / CF 挑战：只识别、等待、转人工（不做自动破解）
agent-browser-cli challenge status                       # result.challenge = {detected, vendor, blocking, interactive, hints}
agent-browser-cli challenge solve                         # 自动过（managed/隐形挑战）或等人工点完，返回 passed；默认超时 120s
agent-browser-cli challenge wait                          # 只等「不再被拦」，不关心 token；默认超时 30s
agent-browser-cli challenge wait --timeout 60             # 显式指定总超时（不传则按 action 取默认值）
agent-browser-cli challenge wait --no-focus --timeout 10  # 只等，不抢焦点
agent-browser-cli challenge focus --no-focus             # 只激活标签页，不动系统窗口
# 严格 CSP 页面（GitHub/Google）：read/find/snapshot/click/fill/wait 自动走无 eval 通道，结果带 via:"no-eval"
agent-browser-cli downloads --limit 10           # 下载列表
agent-browser-cli download <url> --filename a.pdf
agent-browser-cli download-wait --id 7 --timeout 120
agent-browser-cli browsers                       # 多浏览器目标
agent-browser-cli broadcast exec --script 'return location.href'   # 所有 browser/profile 并发执行
agent-browser-cli read --selector '#content'      # 只读某块
agent-browser-cli read --links                    # 附带链接列表
agent-browser-cli find 'a[href]' --limit 20       # CSS 查找
agent-browser-cli find --text '登录'               # 可见文本查找（只返回最内层）
agent-browser-cli find --xpath '//button[1]'
```

页面动作（除 click/fill 外的常用操作，均为单次往返）：

```bash
agent-browser-cli hover '#menu'
agent-browser-cli select '#city' --value sh       # 或 --label 上海
agent-browser-cli check 'input[name=agree]'
agent-browser-cli scroll --to bottom              # 或 --dy 600 / scroll '#footer'
agent-browser-cli wait --selector '.result' --timeout 10
agent-browser-cli wait --text '加载完成'
agent-browser-cli back
agent-browser-cli reload
```

批量与并发（大规模任务必须用这两个，不要 shell 循环调 CLI）：

```bash
agent-browser-cli run steps.json                  # 串行、遇错即停
agent-browser-cli batch steps.json --concurrency 8 # 并发，跨标签批量
agent-browser-cli open https://a.com https://b.com --background  # 并发开标签
```

`batch` 的每个 step 形如 `{"op":"read","args":{"switch_tab_id":"<tabId>","max_chars":2000}}`，
支持 `exec/read/find/action/wait/scan/snapshot/click/fill/send-keys/upload/screenshot/cookies`。
并发时**必须显式给 switch_tab_id**，否则都打到默认标签。

推荐操作顺序：

```text
看页面内容：read（首选）/ scan --text-only
selector 明确时：直接 click/fill/hover/select/check
selector 不明确或页面复杂时：find 拿 selector，或 snapshot 生成 @e
页面结构或内容变化后：重新 snapshot / find
封装命令失效或覆盖不到特殊页面时：回退 exec / JSON CDP / 自定义 JS
```

示例：

```bash
agent-browser-cli scan --text-only
agent-browser-cli click 'button[type=submit]'
agent-browser-cli snapshot --limit 200
agent-browser-cli snapshot --offset 200 --limit 200
agent-browser-cli snapshot --details
agent-browser-cli click '@e1'
agent-browser-cli fill '@e2' 'hello'
agent-browser-cli fill '@e2' --clear
agent-browser-cli fill '@e2' ' world' --append
agent-browser-cli send-keys --target '@e2' 'Enter'
agent-browser-cli mouse-click '@e3'
```

所有高层操作都支持 `--tab <tabId>`，多 Chrome Profile / 多浏览器实例时还支持 `--profile <profile_id-or-label>` 和 `--browser <browser_id>`。`tabs` 输出会包含 `browser_id`、`profile_id`、`profile_label`、`tab_id`、`session_key`。`tabtree` 用树形结构列出 browser → profile → tabs，支持 `--tab <tabId>`、`--profile <profile_id-or-label>`、`--browser <browser_id>` 过滤，过滤结果仍保留父子节点；默认 compact 输出会截断 URL 并省略 `session_key`，需要完整字段时用 `tabtree --full`；`lookup tab <tabId>` 可由 tab 反查 `browser_id` / `profile_id` / `profile_label`，`lookup browser <browser_id>` 可反查所属 profile。`profile-label set <label> --profile <profile_id>` 可设置当前 Chrome Profile 的 label 并校验当前 daemon 内唯一性，`profile-label clear --profile <profile_id>` 可清空；popup 设置是本地便捷入口，不保证跨 Profile 唯一；label 只允许英文、数字、`-`、`_`、`.`，当前 daemon 内匹配到多个 profile 时会报歧义，不能猜。只传 `--tab` 且存在歧义时，必须补 `--profile` 或 `--browser`。`@e` 只在当前 daemon、当前 session_key、最近一次 `snapshot` 内有效。`@e` 只接受 `@e1` 这种带 `@` 的格式。

慢页面要把等待和监控分开：

```bash
agent-browser-cli click '@e1' --wait-js 'return document.body.innerText.includes("完成")' --wait-timeout 10 --monitor
```

`--wait-js` 负责等慢加载；`--monitor` 在动作执行期间观察 DOM 变更，只回计数与少量样本（`change` = `{changed, added, removed, attributes, text, samples}`，`samples` 最多 8 条 `{type, target, detail}`），不回原始 HTML，默认关闭。`click` / `fill` / `mouse-click` / `send-keys` 的 `--monitor` 与 `exec --monitor` 输出同构，可以按同一套字段消费。

观察失败时结果里会出现 `monitor: {ok:false, reason, error}`，`reason` 是机器可读的原因：

```text
navigated   动作期间页面发生导航，观察器已销毁；此时 change 为 {changed:1, navigated:true}，没有样本
superseded  同一标签上有另一个 --monitor 命令接管了观察，本次结果作废
unsupported 页面环境装不上观察器（CSP 等），动作本身已执行
```

`--monitor` 只回答「页面变了多少次、变了哪几处」，判断业务是否完成要靠 `--wait-js`。

Chrome 右上角“正在控制你的浏览器”是 `chrome.debugger.attach` 的正常提示，不是 daemon 常驻状态。普通 CDP 命令空闲后会延迟约 30 秒自动 detach；30 秒内同一 tab 继续执行 CDP 会复用连接并重新计时。network/console 持续监听期间不会自动 detach，停止监听或清理时才释放。

弹窗处理：扩展默认不改写业务页面的 `alert` / `confirm` / `prompt`。只有 CLI 页面执行命令期间临时抑制弹窗，结束后恢复；如果怀疑页面原生弹窗行为异常，先让用户重载扩展和页面。

## 后台运行与异步任务

`open` 默认前台打开（切到新标签页，不聚焦系统窗口）；需要不打扰用户时加 `--background`，标签页会在后台创建、不切页、不抢焦点，之后的 `exec` / `click` 都直接作用于该标签页。无论前后台，新标签都会成为默认目标，不需要每次 `--tab`。

长任务用 `--async` 提交，立刻拿到 `job_id`，不要阻塞等待：

```bash
agent-browser-cli --async scan --text-only
# → {"ok":true,"job":{"id":"0f3c…","status":"running"}}

agent-browser-cli job list
agent-browser-cli job get <job_id>
agent-browser-cli job wait <job_id> --timeout 300
agent-browser-cli job cancel <job_id>
```

支持 `--async` 的操作：`exec`、`scan`、`snapshot`、`click`、`mouse-click`、`fill`、`send-keys`、`screenshot`、`save-pdf`。`job wait` 超时不会丢任务，返回 `job_wait_timeout` 后可以继续 `job get`。任务状态只会存在 daemon 内存里，daemon 重启后查不到，需要留存的产物用 `--out` 落盘。

遇到 Cloudflare「Just a moment...」这类页面：`read` 会带上 `challenge` 字段，先 `challenge wait`（多数 JS 挑战几秒自动放行）；需要人点复选框时用 `challenge focus` 把标签页带到前台，处理完再 `challenge wait`。**不要**指望自动点或打码——CLI 不提供，也不该这么做。

`challenge` 的超时与转人工：

- `--timeout` 可选，不传时服务端按 action 取默认值：`wait` 30s、`solve` 120s。显式传了就用你的值（0.5~600）。
- `--focus-after N` 是从**命令开始**算的绝对时间：超过 N 秒还没通过就把标签页带到前台交给人处理（默认 8s，`0` 表示不聚焦，`--no-focus` 完全禁止抢焦点）。它不会因为每一片等待而重置。
- `wait` / `solve` 未在超时内达成目标时返回 `ok:false` + `error_code:"challenge_timeout"`，退出码非 0；`solve` 另外带 `passed:false, reason:"timeout"`。

`wait`（`--selector` / `--text` / `--js`）都是**一次注入**完成：条件在页面里判定，daemon 不再每 100ms 注入一遍。`--js` 的谓词只编译一次，DOM 变化由 MutationObserver 触发、非 DOM 条件由定时器兜底，所以后台标签页也能用。返回里 `injections` 是注入次数（正常为 1，换页销毁上下文时会重试），`checks` / `polls` 是页面内求值次数，可用来判断页面反应速度。`exec --wait-js` 的输出形状保持 `js_return: { result, wait }`。

注意：`snapshot`、`screenshot`、`network`、`console`、`mouse-click` 需要 CDP，会在该标签页顶部出现 Chrome 的"正在调试此浏览器"提示条；纯 DOM 操作（`exec` / `click` / `fill` / `send-keys`）不会。

## 多步管道、Cookie、上传

一次进程执行多步（省掉多次进程启动，默认遇错即停）：

```bash
agent-browser-cli run steps.json
```

```json
{"steps":[
  {"op":"exec","args":{"script":"return document.title"}},
  {"op":"fill","args":{"target":"#q","value":"hello","has_value":true}},
  {"op":"click","args":{"target":"#submit","wait_js":"return !!document.querySelector('.done')"}}
]}
```

支持 `exec/scan/snapshot/click/mouse-click/fill/send-keys/upload/screenshot/save-pdf`；`--continue-on-error` 可继续执行后续步骤。

读取 Cookie（不需要再用 `exec '{"cmd":"cookies"}'` 逃生口）：

```bash
agent-browser-cli cookies
agent-browser-cli cookies --url https://example.com --tab <tabId>
```

上传本地文件（优先 CDP `DOM.setFileInputFiles`，目标是 CSS selector 或 `@e`）：

```bash
agent-browser-cli upload 'input[type=file]' ./a.pdf ./b.pdf
agent-browser-cli upload '@e3' ./photo.png
```

## 退出码与输出

统一规则：命令返回 `ok:false` → 退出码非 0，脚本可以直接用 `$?` / `&&` 判断成败，不必解析 JSON。

```text
0    成功
1    业务失败（ok:false，含 challenge wait/solve 超时、exec --monitor 页面抛错）
3    等待超时（job wait 超时、download-wait 超时）
130  任务被取消（job wait 等到 cancelled，与 shell 的 SIGINT 惯例一致）
```

- `status` 的 `ok` 跟随 `healthy`：daemon 未运行、端口不一致、扩展未连接、没有可用标签页都会得到 `ok:false` 和非 0 退出码。**只想知道「能不能用」，看退出码即可。**
- `AGENT_BROWSER_CLI_EXIT0=1` 可恢复旧的「恒 0」行为（只适合依赖旧行为的脚本）。
- 默认输出单行 compact JSON；要人读时加 `--pretty`。`tabs` 默认截断 URL，需要完整 URL 用 `tabs --full`。
- `exec --file -` 从 stdin 读脚本，适合 heredoc/管道。
- `snapshot --details` 已把每个元素的 CDP 请求合并成批量 batch（每 40 个一片），比以前快一个数量级；`snapshot --js --details` 走页面内一次注入按 ref 选择器补详情，补不上时在 `pagination.details` 里标 `details_supported:false`，不会让整条快照失败。

## 端口和扩展

固定 API 端口：

```text
127.0.0.1:18767
```

默认扩展 WebSocket 端口：

```text
127.0.0.1:18765
```

配置文件：

```text
~/.agent-browser-cli/config.json
```

修改扩展端口会影响 Chrome 扩展和 daemon 连接，执行前必须说明影响并取得用户确认：

```bash
agent-browser-cli set-extension-port <port>
```

**扩展是否真的更新了**：Chrome 只执行加载时那份 `background.js`，改文件不会自动生效。用 `status` 判断：

```bash
agent-browser-cli status   # 看 extension.stale / warning
agent-browser-cli doctor   # 看 extension_fresh 检查项
```

`extension.stale: true` 表示浏览器里跑的还是旧版本，让用户点扩展图标里的「重新加载扩展」（或 `chrome://extensions` 里重载一次）。2.3 起扩展会自己比对文件哈希并在空闲时自动重载。

### 加固 WebSocket 身份（可选）

默认行为是「装了扩展就能连」：daemon 只校验握手 `Origin` 来自某个 Chrome 扩展。同一台机器上的**任意**扩展都能冒充受控端，读到下发的页面脚本、伪造执行结果。多人共用一台机器、或多个扩展共存时，可以加一层白名单 / token：

```bash
agent-browser-cli trust-extension <extensionId>                           # 只允许该扩展 ID 连接
agent-browser-cli trust-extension --list                                  # 查看当前信任配置，不修改
agent-browser-cli trust-extension --remove <extensionId>                  # 从白名单移除
agent-browser-cli trust-extension --token <value>                         # 握手必须带 ?token=<value>
agent-browser-cli trust-extension --clear-token                           # 只清 token，保留白名单
agent-browser-cli trust-extension --clear                                 # 全部清空，回到兼容模式
```

- 配置写在 `<用户配置目录>/ws-trust.json`：`{"extension_ids": [...], "token": "..."}`。`extension_ids` 非空则只放行名单内的扩展；`token` 非空则握手必须带 `?token=`；两者同时配置时都要满足。
- daemon 每次握手都重新读文件，**改完不用重启**；已经连上的扩展要重连（`chrome://extensions` 里点一次「重新加载」）才会按新规则校验。
- 扩展侧支持 `chrome.storage.local.wsToken`（popup 里可填），配置后自动以 `?token=` 连接。
- 扩展 ID 获取：`chrome://extensions` → 打开「开发者模式」→ 卡片下方的「ID」（32 位 a-p 小写字母）。
- 这是**可选加固**，不配就是原来的「装扩展即用」。`status` 的 `ws_security` 会显示当前是 `trusted_ids` 条数 / `token_required`。



## 网络和控制台调试

`network` / `console` 需要扩展侧持续监听 CDP 事件。修改或升级扩展后，必须先让用户重载 Chrome 插件。

```bash
agent-browser-cli network start --tab <tabId>
agent-browser-cli network list --tab <tabId> --filter api
agent-browser-cli network detail <requestId> --tab <tabId>
agent-browser-cli network clear --tab <tabId>
agent-browser-cli network stop --tab <tabId>

agent-browser-cli console start --tab <tabId>
agent-browser-cli console list --tab <tabId>
agent-browser-cli console list --tab <tabId> --level error
agent-browser-cli console clear --tab <tabId>
agent-browser-cli console stop --tab <tabId>
```

`network detail` 会截断大响应体并标记 `base64Encoded`，不要把巨大 body 粘到对话里。`network clear` 清请求缓存；`network stop` 会停止监听并清请求缓存。`console clear` 清日志缓存；`console stop` 会停止监听并清日志缓存。

页面网络（`net`）的几个使用要点：

- 规则按 `browser:profile` 共享：同 profile 下任意标签执行 `net routes` / `net mock` / `net unroute` 都作用于同一套规则。
- `net hook --persist` 持久化的是**真实规则数组**，刷新后 mock 与改写都不会丢；`net mock` / `net unroute` 之后也会自动同步，不需要重新 hook。
- `net routes` 是只读查询，不会重置拦截统计与日志；想清零用 `net reset`（会连同规则一起清掉）。
- 页面引擎与 CDP 引擎的规则分开存：删 CDP 规则要 `net unroute <pattern> --engine cdp`，不加 `--engine` 只删页面规则；`net reset` 才是两边全清。
- CDP 引擎下「放行」默认不写日志（否则 200 条环形缓冲几秒就被放行记录挤满，有价值的 mock/改写记录反而丢失）；`net mock --engine cdp` 的结果里会直接返回本次是否生效，排查放行为什么没命中时先看规则是否被收窄或退化成全量匹配。


`agent-browser-cli stop` / daemon idle 退出时会额外清理 daemon 内的 snapshot/@e 缓存，并通知扩展清理 network/console 调试缓存。

## 标签分组

多任务开新标签时可以用 session 或 group-title 把标签放入 Chrome 原生标签组。需要独立窗口时用 `open --window`；默认不聚焦，需要抢焦点时显式加 `--focus`；`--window --group-title` / `--window --session` 会把新窗口里的首个 tab 加入对应 tab group。分组只是整理浏览器标签，失败不影响开 tab 主流程。

```bash
agent-browser-cli open https://example.com --session research
agent-browser-cli open https://example.com --group-title "任务A"
agent-browser-cli open --window https://example.com
agent-browser-cli open --window --focus https://example.com
agent-browser-cli open --profile work https://example.com
```

`--session` 和 `--group-title` 都会作为标签组标题；两者同时传时优先使用 `--group-title`。

## 截图和 PDF

截图/PDF 必须让 CLI 写文件，不要把 base64 大段塞进上下文。命令只返回路径、字节数和少量元信息。

```bash
agent-browser-cli screenshot --out /tmp/page.png
agent-browser-cli screenshot --full-page --out /tmp/full.png
agent-browser-cli screenshot --target '@e1' --out /tmp/button.png
agent-browser-cli screenshot --selector 'button[type=submit]' --format jpeg --quality 70 --out /tmp/button.jpg
agent-browser-cli save-pdf --out /tmp/page.pdf
agent-browser-cli save-pdf --paper a4 --landscape --scale 0.9 --out /tmp/page.pdf
```

`screenshot` 默认截当前视口；`--full-page` 截全页；`--target` 和兼容别名 `--selector` 二选一，目标既可以是 `@e` 也可以是 CSS selector。没有 `--out` 时，截图写到 `/tmp/agent-browser-cli-screenshots/`。

`save-pdf` 默认 `paper=a4`、`scale=1.0`、`print-background=true`；需要关闭背景时用 `--no-print-background`。没有 `--out` 时，PDF 写到 `/tmp/agent-browser-cli-pdfs/`，默认文件名来自清理后的页面标题。

如果封装命令失效，用 `exec` 调 CDP 后由脚本落盘，仍然避免把 base64 粘到对话里。

## exec 使用规则

执行复杂 JS 时写入临时文件：

```bash
agent-browser-cli exec --tab <tabId> --file /tmp/script.js
```

需要等待页面变化时使用 `--wait-js`，不要在脚本里固定 `setTimeout`：

```bash
agent-browser-cli exec --tab <tabId> 'document.querySelector("button").click()' --wait-js 'return document.body.innerText.includes("完成")' --wait-timeout 3
```

`exec` 中使用 `await` 必须显式 `return`，否则结果可能是 `null`。

## JSON/CDP 逃生口

跨标签页、Cookie、CDP、扩展管理、浏览器内容权限时，用 JSON 指令：

```bash
agent-browser-cli exec '{"cmd":"tabs"}'
agent-browser-cli exec '{"cmd":"cookies"}'
agent-browser-cli exec '{"cmd":"cdp","tabId":303987837,"method":"Page.captureScreenshot","params":{"format":"png"}}'
```

CDP 点击优先三事件序列：`mouseMoved -> mousePressed -> mouseReleased`。首次 attach 可能出现 Chrome infobar，先发无害 `mouseMoved(0,0)` 预热。

## 文件上传

优先使用 `upload` 命令，它走 CDP `DOM.setFileInputFiles`，最接近真实用户选择文件：

```bash
agent-browser-cli upload 'input[type=file]' ./a.pdf
```

CDP 不可用（例如 devtools 已被占用）或需要一次性构造内存文件时，才退回页面内 `DataTransfer`：

```js
const input = document.querySelector('input[type=file]');
const file = new File(['content'], 'demo.txt', { type: 'text/plain' });
const dt = new DataTransfer();
dt.items.add(file);
input.files = dt.files;
input.dispatchEvent(new Event('input', { bubbles: true }));
input.dispatchEvent(new Event('change', { bubbles: true }));
return input.files.length;
```

## 运维入口

- 目标命令失败、扩展未连接、端口不一致、无可用标签页：看 `references/operations.md`。
- daemon 未运行但尚未执行目标命令：不要排障，直接继续执行 `tabs/open/exec/scan`。
- skill 安装：先 `agent-browser-cli install-skill --dry-run` 展示计划，用户确认后再 `agent-browser-cli install-skill`。
- 可自动执行的排障命令：`status`、`doctor`、`logs --tail`、`restart`、`stop`、`tabs`。

```bash
agent-browser-cli status --pretty    # 一眼看 healthy / summary / warning
agent-browser-cli doctor             # 逐项自检（只检查，不启动 daemon）
agent-browser-cli logs --tail 100    # 纯文本日志
agent-browser-cli restart            # 重读配置并重启 daemon
agent-browser-cli stop               # 只停 daemon
```
