# 安全说明

本文件说明 `atb` 的安全边界、已接受的设计取舍，以及你可以做的加固。隐私与数据流见 [PRIVACY.md](./PRIVACY.md)；来源与依赖许可见 [SOURCES.md](./SOURCES.md)。

## 支持版本

只有最新 minor 接受安全修复。当前线是 `0.7.x`，最新版 `0.7.1`。

- 报告前先升级到最新 `0.7.x`，旧补丁版的问题可能在当前版已修复。
- `0.6.x` 及更早不再接受安全修复。
- 扩展版本需与 CLI 配套（当前扩展 `5.2.9`，消息协议 `4`）。

## 报告漏洞

优先用 GitHub Security Advisories 私密报告：在仓库 Security 页面选择「Report a vulnerability」。

- 仓库：<https://github.com/1728578755/atb-cli>
- 若私密报告不可用，开一个 issue，只写「需要私下联系」，不要贴利用细节、PoC 或受影响地址。
- 请附：版本（`atb --version`）、扩展版本、平台、复现步骤、期望与实际结果。
- 首次回复尽力在 3 个工作日内。修复时间取决于影响面与复杂度，本文件不承诺修复 SLA。

## 威胁模型

| 主体 | 信任假设 | 说明 |
| --- | --- | --- |
| 运行 atb 的本机用户 | 可信 | 你能读 `~/.atb`、能改配置，本工具不防你 |
| 同机其他进程与其他系统用户 | 不完全可信 | 能读文件、能占端口、能发本地 HTTP 请求，取决于系统权限 |
| 被操作的页面内容 | 不可信 | 页面脚本可探测、干扰、伪造你看到的内容 |
| 扩展与 daemon 之间的连接 | 本机回环 WebSocket | 无 TLS，默认只校验「对端是某个 Chrome 扩展」 |

前提：攻击者若已能在本机以你的身份执行代码，atb 不是防线。它能放大这种访问（读登录态、驱动浏览器），但不能阻止它。

这套模型不覆盖：浏览器自身漏洞、被第三方修改过的扩展包、你主动交给 Agent 的凭据。

攻击面：

- `127.0.0.1:18765`：扩展接入的 WebSocket，默认「装了扩展就能连」。
- `127.0.0.1:18767`：CLI 调 daemon 的 HTTP API，需要 `api-token`。
- `~/.atb/api-token`：文件权限 0600；Windows 上另用 `icacls` 收紧。
- `~/.atb/ws-trust.json`：白名单与握手 token，可选。
- `chrome.debugger`：attach 后能读写该标签页的网络与控制台。
- `<all_urls>` 注入：内容脚本在所有帧运行。
- 页面脚本执行：`exec` / `inject` 在页面上下文执行任意 JS。
- 下载落盘：`download` 把文件写进浏览器下载目录，文件名来自站点。

## 已知设计取舍

| 取舍 | 现状 | 后果 |
| --- | --- | --- |
| `trust-extension` 单向 | 只约束「谁能连 daemon」，扩展不校验对端 | 空窗期里冒充 daemon 的本机进程仍可能被扩展接受 |
| daemon 空闲退出 | 默认 300 秒无活动且无扩展连接时退出（`ATB_IDLE_TTL`）；扩展断开有 60 秒宽限 | 退出到下次启动之间，18765 可能被同机进程抢占 |
| `debugger` 权限 | `snapshot` 默认、纯 CSS 的 `click`/`fill`、`mouse-click`、`send-keys`、`upload`、`network`/`console`、整页/元素截图、`save-pdf`、`net mock --engine cdp`、挑战求解/点击会 attach | 浏览器顶部出现调试横幅，页面可感知 |
| `<all_urls>` 注入 | 内容脚本在所有站点与所有帧运行 | 页面可探测到本工具的信号面，用 `stealth audit` 量化 |
| Cookie 可读 | 能读含 `httpOnly` 的 Cookie | 拿到 CLI 或扩展控制权即可导出登录态 |
| 挑战自动交互 | `challenge` 会点击可交互组件并等待放行；失败时智能体用 `widget` + `click --at` 自行兜底，最后才转人工 | 站点不放行时超时失败；需要视觉/短信验证的仍要人 |

以上取舍是公开的、有意的。不要把「配置了 token」理解成安全边界，它只覆盖一个方向。

## 加固清单

日常：

- 不要把 `atb stop` 当安全边界：它会释放 18765，扩展仍在重连；要收口就同时禁用扩展或关闭浏览器，或保持 daemon 运行占住端口。
- 保持 CLI 与扩展同版本；`atb status` 的 `compatibility` 不配套时先修版本。
- `atb doctor` 定位端口、token、daemon 状态；`--fix` 自动修复，加 `--force` 才会杀进程。

处理敏感账号时：

- `atb trust-extension <extensionId>` 限定扩展白名单，`--token <value>` 要求握手带 token；两者可同时生效。
- 设 `ATB_NO_DEBUGGER=1`，彻底禁止 attach；需要 CDP 的命令会返回替代方案。
- 用专用的 Chrome Profile，避免日常登录态与自动化任务混在一起。
- 不要在不可信页面执行来源不明的脚本：`exec` / `inject` 拿到的代码会在页面里跑。
- 检查 `~/.atb` 及其文件的权限，确认只有本人可读。

多人共用一台机器：

- CLI API 端口固定为 `18767`，不能同时跑两个 daemon。独立系统账号和 `ATB_HOME` 只能隔离配置与文件权限，解决不了端口冲突；需要并行使用请分机器/分容器，或约定同一时间只有一个用户运行 atb。
- 不要共用同一个浏览器 Profile 与同一个扩展实例。
- 需要时给 daemon 端口做系统级访问控制，而不是只依赖 token。

## 安全相关命令

| 命令 | 作用 | 何时用 |
| --- | --- | --- |
| `atb status` | 健康状态、版本与协议配套 | 每次升级后 |
| `atb doctor` | 诊断端口、token、daemon 版本；`--fix` 修复 | 连不上或行为异常 |
| `atb capabilities` | 扩展版本、daemon 版本、特性门禁 | 怀疑版本不配套 |
| `atb trust-extension <id>` | 扩展白名单；`--token`、`--list`、`--clear` | 多人共用或加固 |
| `atb stealth audit` | 审计页面可读到的痕迹信号，给出评分与发现项 | 怀疑被站点识别 |
| `atb stop` | 停止 daemon 并释放 18765 | 需要彻底停用时，同时禁用扩展或关闭浏览器 |

## 不属于漏洞的现象

- 需要本机代码执行才能利用的本地提权或横向移动。
- 用户在 `exec` / `inject` 中主动执行任意 JavaScript 造成的后果。
- 被操作网站自身的安全问题。
- 挑战页未通过、验证码未被破解导致的超时。
- 浏览器顶部出现调试横幅（attach 的既定行为，可用 `ATB_NO_DEBUGGER=1` 规避）。
- 同机进程在扩展断开、daemon 空闲退出后抢占端口（已知取舍，见上表）。
- 共用系统账号时多个 atb 实例互相影响配置与端口。

## English summary

Only the latest minor (`0.7.x`) receives security fixes; upgrade before reporting.

Report privately through GitHub Security Advisories. If unavailable, open an issue that only says a private channel is needed, with no exploit details. We aim to reply within 3 business days and do not promise a fix SLA.

The local user is trusted; other local processes and page content are not. Traffic between the CLI, daemon and extension stays on `127.0.0.1`, using WebSocket 18765 (extension) and HTTP 18767 (CLI, `api-token`).

Known trade-offs: one-way `trust-extension`, a port window after the extension disconnects and the daemon idles out, no daemon authentication by the extension, `chrome.debugger` attach, `<all_urls>` injection, readable cookies, and challenge handling that interacts with the widget, lets the agent click by coordinates as a fallback, and hands over to a human only when needed — without bypassing access control.

Do not treat `atb stop` as a security boundary: it frees 18765 while the extension keeps reconnecting. Disable the extension or close the browser to close the window, or keep the daemon running to hold the port. Keep versions matched, and use `trust-extension` plus `ATB_NO_DEBUGGER=1` for sensitive work.
