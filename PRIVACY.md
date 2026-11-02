# 隐私政策 / Privacy Policy

**项目：** agent-browser-cli（CLI `0.3.8`）+ Chrome 扩展 `Agent Browser CLI Bridge`（Manifest V3，版本 `3.4`）
**适用范围：** 本仓库发布的 CLI 二进制、随包扩展 `assets/tmwd_cdp_bridge` 以及本地 daemon。
**最后更新：** 2026-11-02

---

## 0. 一句话结论

**本项目只在你自己的电脑上工作：扩展只连本机 `127.0.0.1`，CLI 与 daemon 只在回环地址上通信，两者都不会把任何数据上传到远程服务器，也不含统计、埋点、广告或崩溃上报 SDK。**

唯一的例外不在本项目的代码里，而在你如何使用它：命令的输出会交给调用它的 Agent，Agent 通常会把这些内容放进模型上下文并发给模型服务商。这一点在第 2 节单独说明，是使用本工具时**最大的隐私暴露面**。

---

## 1. 组件与端口

| 组件 | 形态 | 监听 / 连接 | 说明 |
| --- | --- | --- | --- |
| Chrome 扩展 | MV3，`Agent Browser CLI Bridge` v3.4 | 主动连接 `ws://127.0.0.1:18765` | 默认端口，可用 `agent-browser-cli set-extension-port <port>` 修改 |
| daemon | 本机后台进程，随 CLI 命令自动拉起 | 监听 `127.0.0.1:18767`（HTTP API）与 `127.0.0.1:18765`（WebSocket） | 只绑定回环地址，不对局域网暴露 |
| CLI | 前台命令 | 请求 `http://127.0.0.1:18767` | 携带 `x-agent-browser-token` 请求头 |

daemon 空闲 5 分钟自动退出（默认 300 秒，可用环境变量 `AGENT_BROWSER_CLI_IDLE_TTL=<秒>` 覆盖）。退出后 18765 / 18767 两个端口都会被释放。

---

## 2. 数据在哪里流转

### 2.1 三条数据通路

```
┌──────────────┐   ① WebSocket 127.0.0.1:18765   ┌──────────┐
│ Chrome 扩展  │ ◄────────────────────────────► │  daemon  │
└──────────────┘   命令下行 / 结果上行           └────┬─────┘
       ▲                                              │ ② HTTP 127.0.0.1:18767
       │ 浏览器 API / CDP                            │    (x-agent-browser-token)
       ▼                                              ▼
   你的浏览器标签页                                ┌──────────┐
                                                  │   CLI    │
                                                  └────┬─────┘
                                                       │ ③ stdout / JSON
                                                       ▼
                                                  ┌──────────┐
                                                  │  Agent   │  ← 可能转发给模型服务商
                                                  └──────────┘
```

**① 扩展 ↔ daemon（本机 WebSocket，默认 18765）**

- 方向一：daemon 把命令下发给扩展（点击、读取 DOM、截图、读取 cookie、抓包、下载等）。
- 方向二：扩展把执行结果回传给 daemon（页面文本、HTML、Cookie 值、截图 base64、网络请求/响应体、控制台日志、下载记录）。
- 全程 `ws://` 明文，但只走回环接口；数据不离开本机网卡。

**② CLI ↔ daemon（本机 HTTP，127.0.0.1:18767）**

- CLI 用 `x-agent-browser-token` 头鉴权，token 从 `~/.agent-browser-cli/api-token` 读取。
- 请求体是命令参数（脚本、选择器、URL、步骤文件等），响应体是上一条通路拿到的结果。
- 除了 `/ping`（仅返回存活与端口状态，不含业务数据），所有接口都要求 token。

**③ Agent 调用方（CLI 之外，本项目无法控制）**

CLI 把结果打到 stdout（JSON）。**调用它的 Agent 通常会把 stdout 直接放进自己的上下文，再发给模型服务商。** 这意味着：

- `cookies` 导出的 Cookie 值、`read` / `snapshot` 读到的页面正文、`screenshot` 的截图（本地文件路径或返回的图片数据）、`net hook --capture-bodies` 抓到的请求/响应体、`console` 日志，**都可能离开你的电脑**；
- 是否离开、离开多少，取决于你用的 Agent、模型与它们的隐私条款，**不由本项目的代码决定**；
- 在已登录的敏感站点（邮箱、网盘、后台、银行、企业 SaaS）上使用前，请先确认你能接受这些内容进入模型上下文；
- 需要严格控制时，优先用结果裁剪类参数（如 `read --max-chars`、`snapshot --limit`）、避免 `net hook --capture-bodies`、避免在敏感 Profile 上跑自动化。

> **免责声明：** 本项目保证自身不发起远程上传，但**不对 Agent 或模型服务商如何处理命令输出负责**。请按你所在组织的数据分级要求决定可以把哪些页面交给自动化流程。

### 2.2 明确不做的事

- 不含遥测、埋点、A/B 实验、崩溃上报、广告 SDK。
- 不访问任何远程 API（源码中不存在指向外部域名的 HTTP 调用）。
- 扩展内没有远程代码加载：所有 JS 都打包在扩展目录里，不从网络拉取或 `eval` 远程脚本。
- 不修改搜索引擎、不劫持新标签页、不注入第三方脚本、不出售数据。

---

## 3. 扩展权限逐项说明

下表与 `assets/tmwd_cdp_bridge/manifest.json`（`manifest_version: 3`，`version: 3.4`）**逐项一一对应**，顺序与文件一致。

| 权限 | 实际用途 | 能接触到什么 |
| --- | --- | --- |
| `cookies` | 读取当前标签页 URL 对应的 Cookie（含 CHIPS 分区 Cookie），用于复用你已登录的会话 | Cookie 名、值、域、过期时间、`httpOnly` / `secure` 标记 |
| `tabs` | 列出、新建、关闭、激活、移动标签页；读取标题与 URL | 标签页标题、URL、窗口与分组归属 |
| `tabGroups` | 创建、查询、移动标签组；按组批量操作标签页 | 分组标题、颜色、包含的标签页 |
| `activeTab` | 用户点击扩展图标时临时获得当前标签页访问权，作为最小权限路径的兜底（本项目同时声明了 `<all_urls>`，因此它主要用于弹窗内操作） | 当前标签页 |
| `debugger` | 通过 `chrome.debugger` attach 到标签页，启用 Network / Fetch / Page / Runtime 等域。**可读取并改写请求与响应体**（`net mock` 即由此实现），也可截图、执行脚本、读控制台 | 页面全部网络流量（含请求头、Cookie 头、响应体）、DOM、JS 运行时、截图 |
| `scripting` | 向指定标签页或子框架注入脚本，执行点击、填表、读取 DOM | 页面 DOM 与 JS 环境 |
| `alarms` | 定时任务：Service Worker 保活自检、延迟重载扩展（扩展代码在磁盘更新后自动生效） | 无业务数据 |
| `storage` | 保存扩展侧配置：WS 端口、WS token、`browserId` / `profileId` / `profileLabel`、持久化注入脚本、最后连接错误、构建哈希 | 见第 4.2 节 |
| `declarativeNetRequest` | **仅用于删除旧版本遗留的一条动态规则**（`updateDynamicRules({ removeRuleIds: [...] })`）。当前版本不注入任何拦截/改写规则；请求改写走 `debugger` 的 Fetch 域 | 无（只做规则清理） |
| `management` | 列出已安装扩展及其启用状态；按 CLI 指令启用/禁用其它扩展；配合 `alarms` 重载本扩展自身 | 其它扩展的 ID、名称、版本、启用状态 |
| `contentSettings` | 修改站点级内容设置（如 `automaticDownloads`），让 `download` 流程不被浏览器拦住 | 指定 pattern 下的站点权限设置 |
| `webNavigation` | 列出标签页内全部框架及 URL；监听导航提交事件以维护会话与注入时机 | 框架树、每个框架的 URL |
| `downloads` | 发起下载、查询/列出下载记录（文件名、URL、状态、时间）、取消下载；**`downloads erase` 可删除下载记录，省略 id/state 时会清空全部下载历史**（必须显式传 `confirmAll: true` 才会执行） | 下载历史与本地文件路径 |
| `host_permissions: <all_urls>` | 上述能力可作用于任意 `http` / `https` 页面；`file://` 页面还需在扩展详情页手动开启「允许访问文件网址」（弹窗里显示为「文件网址访问」） | 任意站点 |

**额外声明（不在 `permissions` 数组内，但同属清单能力）：** `content_scripts` 以 `matches: ["<all_urls>"]`、`all_frames: true`、`run_at: document_idle` 注入 `config.js` 与 `content.js`。该脚本在顶层页面右下角绘制一个可拖动的连接状态徽标，不读取页面内容、不采集数据；`all_frames` 是为了让子框架知道扩展存在。

`<all_urls>` 是本产品的必要成本：工具的存在意义就是操作你指定的任意网页。扩展不会自行遍历你没有让它访问的页面。

---

## 4. 本地存储

### 4.1 CLI / daemon 侧：`~/.agent-browser-cli/`

| 文件 | 内容 | 是否敏感 | 如何删除 |
| --- | --- | --- | --- |
| `config.json` | `{"extension_port": 18765}`，插件 WebSocket 端口 | 否 | 直接删除；下次启动按默认值重建 |
| `api-token` | 每个 daemon 启动时生成的随机 token（UUIDv4 去连字符，32 位十六进制），CLI 用它调用 18767 | **是**，本机 API 凭据。Unix 上以 `0600` 写入 | 删除后需 `agent-browser-cli restart`，否则当前 CLI 会 401 |
| `ws-trust.json` | 扩展白名单 `extension_ids` 与可选的 WebSocket 握手 `token` | **是**，含握手 token 明文；Unix 上以 `0600` 写入 | 删除即回到「任何扩展可连」的兼容模式；更推荐 `agent-browser-cli trust-extension --clear` |
| `net-hooks.json` | 按 `<browser:profile>` 保存的 `net` 规则（URL 匹配、mock 响应体等） | **可能敏感**，可能含 URL 与伪造的响应内容 | `agent-browser-cli net reset`，或删除文件后重启 daemon |
| `daemon.lock` | 零字节锁文件，用于防止并发拉起多个 daemon | 否 | daemon 停止后可直接删除 |
| `logs/daemon.log` | 追加写日志，见 4.3 节 | **可能敏感**（含 URL、错误文本、偶发的原始报文片段） | `agent-browser-cli stop` 后删除；daemon 下次启动会重建 |

### 4.2 扩展侧：`chrome.storage.local`

| 键 | 内容 | 是否敏感 |
| --- | --- | --- |
| `wsPort` | 扩展连接 daemon 的端口 | 否 |
| `wsToken` | WebSocket 握手 token，**明文存储** | **是** |
| `browserId` / `profileId` / `profileLabel` | 浏览器与 Profile 标识、用户起的别名 | 低（可用于区分浏览器实例） |
| `persistInjections` | `inject --persist` 保存的脚本与其 URL 匹配规则 | **可能敏感**，脚本可能含凭据或业务逻辑 |
| `lastConnectError` | 最后一次连接失败的原因（供弹窗诊断） | 低 |
| `appliedBuildHash` 等 | 扩展自检用的构建哈希与时间戳 | 否 |

卸载扩展即清除以上全部内容。单独清除某项可用 `inject --remove <name>`（持久注入）或在弹窗里清空 token。

### 4.3 日志内容

`logs/daemon.log` 记录的是**运行诊断**，不是业务数据：启动与监听地址、插件端口绑定失败原因、`ws-trust.json` 损坏时拒绝握手、扩展版本过低触发重载、`net-hooks.json` 读写失败、以及「WebSocket 消息解析失败」时的**原始报文片段**。最后一项在扩展发来畸形消息时可能包含命令参数。日志**没有自动轮转**，长期使用会持续增长。

### 4.4 完整卸载

```bash
agent-browser-cli stop
rm -rf ~/.agent-browser-cli
```

然后在 `chrome://extensions` 中移除 `Agent Browser CLI Bridge`（或删除已加载的 `assets/tmwd_cdp_bridge` 目录）。这两步做完，本机不再残留本项目写入的任何配置、凭据或日志。

---

## 5. JavaScript 执行

- **扩展不自带远程代码。** 随包的 `background.js`、`content.js`、`popup.js` 就是全部逻辑，不从 CDN 或任何网络地址加载脚本。
- **但扩展会执行本机 CLI 在运行期下发的任意 JS。** 这是产品设计的一部分，不是缺陷：
  - `agent-browser-cli exec "<js>"`：在指定标签页执行一段脚本并返回结果；
  - `agent-browser-cli inject "<js>"`：注入脚本，可选 `--world main|isolated`、`--frame`、`--all-frames`；
  - `agent-browser-cli inject --persist <name>`：把脚本命名持久化，**之后每次页面导航都会自动重放**，直到 `inject --remove <name>`。
- 持久注入的脚本体存放在 `chrome.storage.local` 的 `persistInjections` 中，可长期存在。
- 结论：能连上 daemon 的一方，等价于能在你的浏览器里执行任意 JS。**请把「谁能连上 daemon / 扩展连上了谁」当作最高优先级的安全边界**，详见 [SECURITY.md](./SECURITY.md)。

---

## 6. Cookie 导出

扩展弹窗（`popup.js`）里的 Cookie 区域：

1. 打开弹窗时**只读取并展示**当前标签页的 Cookie，**不会自动写剪贴板**（旧版本曾自动复制，已移除）。
2. 「复制 cookies」按钮是**显式操作**，必须由你点击才会调用 `navigator.clipboard.writeText`。
3. 默认**排除** `httpOnly` 与 `secure` 的条目；要复制它们必须勾选「包含 httpOnly/secure」复选框。被跳过的条数会在弹窗里显示（例如「已复制 8 条，跳过 3 条 httpOnly/secure」）。
4. 即便只复制普通 Cookie，拼出的 `a=1; b=2` 字符串也会进入系统剪贴板 —— 本机任何进程都能读取剪贴板。用完请清空或覆盖剪贴板。

注意：弹窗展示的是**完整 Cookie 值**（带 `[H]` / `[S]` / `[P]` 标记），因此不要在不信任的录屏、共享屏幕或远程协助场景下打开弹窗。

---

## 7. 保留策略与关闭方式

- **不持久化页面数据。** 页面内容、截图、网络抓包、控制台日志只存在于 daemon 内存与当次响应中；daemon 退出时 `cleanup_on_shutdown` 会清空快照表与在途命令登记，并通知扩展清理调试会话（`debugClearAll`），随后进程结束，内存中的会话全部消失。
- **持久化的只有配置与凭据**：`config.json`、`api-token`、`ws-trust.json`、`net-hooks.json`、`daemon.log`，以及扩展侧的 `chrome.storage.local`。
- **网络抓包**：`net hook --capture-bodies` 的记录留在 daemon 内存，`net reset` 或 daemon 退出即清空；`net-hooks.json` 只保存规则，不保存抓包历史。
- **日志**：追加写、无自动轮转、无上传；随时可删。
- **关闭**：
  - 临时停止：`agent-browser-cli stop`（同时释放 18765 / 18767）；
  - 不使用时让 daemon 空闲退出（默认 300 秒），或把 `AGENT_BROWSER_CLI_IDLE_TTL` 调短；
  - 彻底断开浏览器侧：在 `chrome://extensions` 停用或移除扩展。
- **关闭后残留**：扩展的 `chrome.storage.local` 与 `~/.agent-browser-cli/` 仍在磁盘上，按 4.4 节清理。

---

## 8. 联系与变更

- 隐私问题、数据边界疑问：提交到 <https://github.com/sleepinginsummer/agent-browser-cli/issues>。
- 安全漏洞：**不要开公开 issue**，请按 [SECURITY.md](./SECURITY.md) 走私密渠道。
- 本政策随功能变化更新，最近一次修订：**2026-11-02**（对应 CLI `0.3.8` / 扩展 `3.4`）。

---

# Privacy Policy (English)

**Project:** agent-browser-cli (CLI `0.3.8`) with the Chrome extension `Agent Browser CLI Bridge` (Manifest V3, version `3.4`).
**Last updated:** 2026-11-02.

### Summary

This project runs entirely on your own machine. The extension talks only to a daemon listening on `127.0.0.1` (WebSocket, default port 18765), and the CLI talks only to the same daemon's HTTP API on `127.0.0.1:18767`. Neither the extension nor the daemon contacts a remote server: there is no telemetry, analytics, advertising, crash reporting, or remote code loading. The only data that may leave your computer is what **your agent** chooses to forward to a model provider after reading the CLI's stdout.

### Data flows

1. **Extension ↔ daemon (`ws://127.0.0.1:18765`).** Commands travel down (click, read DOM, screenshot, read cookies, intercept network, download) and results travel up (page text, HTML, cookie values, base64 screenshots, request/response bodies, console logs, download records). Plaintext WebSocket, loopback only.
2. **CLI ↔ daemon (`http://127.0.0.1:18767`).** Authenticated with the `x-agent-browser-token` header, whose value is read from `~/.agent-browser-cli/api-token`. Every route except `/ping` requires the token.
3. **Agent caller (outside this project's control).** CLI results are printed to stdout as JSON. Agents usually place that output directly into their context and send it to a model provider. Cookies, page content, screenshots, captured request/response bodies, and console output can therefore leave your machine. Whether and how much they leave is decided by your agent and model provider, not by this project's code. Avoid `net hook --capture-bodies` and be cautious with authenticated or sensitive sites. **This is the largest privacy exposure surface when using the tool.**

The project itself makes no outbound connections; the Rust sources contain no calls to external endpoints.

### Permissions (identical to `manifest.json`)

`cookies` (read cookies for the active URL, including CHIPS partitioned cookies); `tabs` (list/create/close/activate tabs, read titles and URLs); `tabGroups` (create and manage tab groups); `activeTab` (temporary access to the active tab for popup actions); `debugger` (attach Chrome DevTools Protocol, **read and rewrite request/response bodies**, take screenshots, evaluate scripts, read console); `scripting` (inject scripts into tabs and frames); `alarms` (service-worker keepalive, deferred self-reload); `storage` (persist port, WS token, browser/profile identity, persistent injections, last connection error); `declarativeNetRequest` (**only** removes one legacy dynamic rule; this version injects no rules — request rewriting uses the debugger's Fetch domain); `management` (list installed extensions, enable/disable them on CLI request, reload this extension); `contentSettings` (adjust site-level settings such as automatic downloads); `webNavigation` (enumerate frames and track navigation); `downloads` (start downloads, list download history, cancel, and **erase download records — erasing everything requires an explicit `confirmAll: true`**); `host_permissions: <all_urls>` (all of the above on any site; `file://` additionally requires the user to enable "Allow access to file URLs").

The content script is also injected into `<all_urls>` in all frames; it only renders a draggable connection-status badge in the top frame and collects no data.

### Local storage

`~/.agent-browser-cli/` holds `config.json` (port, not sensitive), `api-token` (sensitive; random per daemon start, mode `0600` on Unix), `ws-trust.json` (sensitive; extension allow-list and optional handshake token), `net-hooks.json` (may be sensitive; net rules and mock bodies), `daemon.lock` (empty lock file), and `logs/daemon.log` (diagnostics; may contain URLs and stray message fragments; no rotation). Extension-side `chrome.storage.local` holds the port, the WS token **in plaintext**, browser/profile identifiers, persistent injection scripts, the last connection error, and build hashes.

Full removal:

```bash
agent-browser-cli stop
rm -rf ~/.agent-browser-cli
```

Then remove the extension in `chrome://extensions`.

### JavaScript execution

No remote code ships or is fetched. However, the extension executes arbitrary JavaScript supplied at runtime by the local CLI — `exec`, `inject`, and `inject --persist` (which replays on every navigation until removed). This is by design: whoever can talk to the daemon can run JS in your browser. Treat "who may connect to the daemon" and "which daemon the extension connected to" as the primary security boundary; see [SECURITY.md](./SECURITY.md).

### Cookie export

The popup reads and displays cookies but **never writes to the clipboard automatically**. The "复制 cookies" (copy cookies) button is an explicit action; by default it **excludes `httpOnly` and `secure` entries**, which require ticking the "包含 httpOnly/secure" checkbox. The popup shows full cookie values, so avoid opening it while screen-sharing.

### Retention and shutdown

Page content, screenshots, captures, and console logs live only in daemon memory and are cleared on shutdown; only configuration and credentials persist. Logs are append-only with no rotation and are never uploaded. Stop the daemon with `agent-browser-cli stop`; it also exits after 300 seconds of inactivity (override with `AGENT_BROWSER_CLI_IDLE_TTL`).

### Contact

Privacy questions: <https://github.com/sleepinginsummer/agent-browser-cli/issues>. Security reports: use the private channel described in [SECURITY.md](./SECURITY.md), not a public issue.
