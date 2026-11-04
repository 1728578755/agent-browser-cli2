# 隐私说明

适用于 `atb` 命令行工具（npm 包 `@g1728578755/atb`）与配套 Chrome 扩展 **ATB**（扩展文件版本 5.2.9）。

安全边界与威胁模型见 [SECURITY.md](./SECURITY.md)；外部来源与依赖许可见 [SOURCES.md](./SOURCES.md)。

## 一句话结论

CLI、daemon、扩展之间的通信都在本机 `127.0.0.1`。工具不含遥测、不上传数据、不加载远程代码，也不需要账号。

## 数据流

```text
Chrome 页面
   ↑↓  内容脚本 / chrome.* API
Chrome 扩展 ATB
   ↑↓  WebSocket  ws://127.0.0.1:18765
本机 daemon（Rust 进程）
   ↑↓  HTTP  http://127.0.0.1:18767（带 api-token）
atb CLI（每次命令一个进程）
```

- daemon 监听 `18765`（扩展）与 `18767`（CLI），两者都只绑回环地址。
- daemon 由 CLI 按需拉起；默认空闲 300 秒且没有扩展连接时退出（`ATB_IDLE_TTL` 可调），扩展连着时不退。
- 扩展是 WebSocket 客户端，只连本机；连不上时不做任何重试以外的动作。
- 配置目录默认 `~/.atb`，可用 `ATB_HOME` 改到别处。

## 本地数据清单

| 路径 | 内容 | 用途 | 删除 |
| --- | --- | --- | --- |
| `~/.atb/config.json` | `{"extension_port":18765}` | 扩展连接端口 | 删除后回默认 18765 |
| `~/.atb/api-token` | 随机 token（权限 0600） | CLI 调 daemon 的鉴权 | 删除后新 daemon 重新生成 |
| `~/.atb/ws-trust.json` | 扩展白名单与握手 token | `trust-extension` 的可选加固 | 删除后回到兼容模式 |
| `~/.atb/net-hooks.json` | 按 `browser:profile` 存的网络规则 | daemon 重启后恢复 `net` 规则 | 删除即清空规则 |
| `~/.atb/agent-tabs.json` | 智能体名到标签页的映射 | 多智能体互不串台 | 删除即清空分配记录 |
| `~/.atb/logs/daemon.log` | daemon 运行日志 | 排查连接与命令失败 | 删除后下次运行重建 |
| 浏览器扩展存储 | 扩展与 profile 标识、扩展端口、握手 token、角标开关、最后连接错误、自检状态、持久化注入脚本；`storage.session` 放请求去重与调试会话恢复等短期状态 | 多浏览器识别、弹窗诊断、刷新后重放注入 | 卸载扩展即删除 |

扩展存储在 Chrome 的扩展数据目录内，不在 `~/.atb`。`storage.session` 的数据随浏览器退出清空。

## 会接触的浏览器数据

| 数据 | 来源 | 使用条件 |
| --- | --- | --- |
| Cookie（含 `httpOnly`） | `chrome.cookies` | `atb cookies` 读取或清除 |
| 页面 DOM 与文本 | 内容脚本、`chrome.scripting` | `read` / `scan` / `snapshot` / `find` / `click` 等页面命令 |
| 网络请求与响应 | `chrome.debugger` 或页面 hook | `net hook` / `net log` / `network` 系列 |
| 控制台输出 | 页面与 `chrome.debugger` | `console start` 期间 |
| 下载记录与文件 | `chrome.downloads` | `downloads` / `download` / `download-wait` |
| 标签页、标签组、窗口 | `chrome.tabs`、`chrome.tabGroups` | `tabs` / `tabtree` / `open` / `close` / `cleanup` |
| 扩展自身管理 | `chrome.management` | 接收 daemon 的重载指令；扩展另有 list / disable / enable 处理器，当前 CLI 未暴露对应命令 |
| 站点内容设置 | `chrome.contentSettings` | 扩展保留该权限与处理器，当前 CLI 未暴露读写入口 |

页面数据只在 CLI（或经 CLI 转发的 Agent）主动下发命令时读取；扩展自身的管理与重载可能由 daemon 自动触发。扩展不会自行采集或外发页面内容，页面正文也不落盘；本地留存的是连接配置、扩展/Profile 标识、诊断状态，以及你显式要求持久化的注入脚本。

扩展声明的完整权限列表以 `assets/atb_bridge/manifest.json` 为准。

## 不收集、不上传

- 无统计、无埋点、无崩溃上报、无广告 SDK。
- 无远程配置、无远程代码：扩展与页面脚本都打包在发行物内，不从网络加载或执行。
- 无账号体系，不需要登录，不采集设备指纹。
- 不向任何服务器发送 Cookie、页面正文、URL 列表或命令记录。

两个边界：

- 你操作的网页仍按站点自身逻辑联网，这部分流量与 atb 无关。
- `atb net fetch` 会按你给的 URL 发起请求，`atb open` 会导航到目标地址。这是命令语义，不是遥测。

## 日志可能包含什么

daemon 日志以排查为目的，可能记录：URL、错误码、错误上下文、扩展版本与协议版本、时间戳、端口与进程信息。

代码未对日志做统一脱敏，因此不要假定日志里一定没有敏感内容。日志可能间接带出页面地址等上下文；分享日志前请自行检查并按需删改。

## 你的控制权

- `atb stop`：停止 daemon，端口关闭，扩展随即断线。
- `atb cookies --clear`：清除当前标签页 URL 下的 Cookie；加 `--domain` 指定域。会退出相关网站的登录状态。
- 断开扩展：在扩展弹窗里断开，或直接禁用/卸载扩展。
- 删除 `~/.atb` 整个目录：清掉配置、token、信任配置、规则、分配记录与日志。
- 卸载扩展：删除扩展本地存储，包括标识与持久化注入脚本。

## 第三方

没有。工具不集成第三方分析、推送、崩溃收集或广告服务。二进制依赖的许可见 [SOURCES.md](./SOURCES.md)。

## 变更与联系方式

本文件随功能变化更新，版本变更记录见 [CHANGELOG.md](./CHANGELOG.md)。仓库地址：<https://github.com/1728578755/atb-cli>。

隐私相关问题请开 issue：<https://github.com/1728578755/atb-cli/issues>。安全问题请按 [SECURITY.md](./SECURITY.md) 的私密渠道报告，不要公开贴细节。

## English summary

`atb` and its companion Chrome extension **ATB** talk only to `127.0.0.1` for tool traffic (WebSocket 18765 for the extension, HTTP 18767 for the CLI); the pages you drive still talk to their own sites.

There is no telemetry, no analytics, no ads, no remote configuration and no remote code; everything ships inside the package.

Local state lives in `~/.atb` (`config.json`, `api-token`, `ws-trust.json`, `net-hooks.json`, `agent-tabs.json`, `logs/daemon.log`) plus the extension's own browser storage.

Browser data (cookies including `httpOnly`, page DOM, network traffic, console output, downloads, tabs, extension list, content settings) is touched only when you run a command.

Daemon logs are written for troubleshooting and may contain URLs and error context; review them before sharing.

Remove everything with `atb stop`, delete `~/.atb`, and uninstall the extension.
