# 来源与依据

本文件记录 `agent-browser-cli` 的代码派生来源、规范依据与第三方依赖，便于使用者核对出处、许可证与改造范围。

- 项目版本：0.3.8
- 更新日期：2026-11-02

## 1. 代码派生来源

### 1.1 GenericAgent

| 项 | 内容 |
|---|---|
| 上游项目 | GenericAgent |
| 仓库 | <https://github.com/lsdefine/GenericAgent> |
| 许可证 | MIT License，Copyright (c) 2025 lsdefine |
| 上游定位 | 自演化 Agent，通过 TMWebDriver 注入真实浏览器，保留登录态 |

本项目从 GenericAgent 的 Web 工具链中提取并改造了以下部分：

| 本项目路径 | 上游对应 | 派生内容 |
|---|---|---|
| `assets/tmwd_cdp_bridge/` | `tmwd_cdp_bridge/` | Chrome MV3 扩展骨架、WebSocket 桥接、CDP attach/detach、标签页与会话标识 |
| `assets/simphtml_opt.js` | `simphtml` | 页面简化与去噪算法（固定浮层处理、可见性判定、文本抽取） |
| `assets/tmwd_cdp_bridge/background.js` | `TMWebDriver.py` / `tmwd_cdp_bridge` | 本地 WebSocket 客户端与命令分发思路 |

改造范围包括但不限于：

- 由 Python 运行时改为 Rust daemon + CLI，新增本机 HTTP API、启动锁、会话路由与 job 队列；
- 页面脚本重写为 `assets/page_*.js`，新增 `read` / `find` / `action` / `wait` / `net` / `challenge` 等能力；
- 新增严格 CSP 下的无 eval 注入通道、多浏览器 / 多 Profile 隔离、异步任务与批量执行；
- 新增 `tests/`、`docs/`、`skills/` 与构建/发布脚本。

> 上游版权声明与 MIT 许可全文见仓库根目录 [NOTICE](./NOTICE)。分发本项目的二进制或源码时，请同时保留 `LICENSE` 与 `NOTICE`。

### 1.2 非派生部分

除上表列出的文件外，`src/`、`scripts/`、`tests/`、`docs/`、`skills/` 与 `assets/page_*.js` 为本项目整理或重写实现。它们同样以 MIT 许可证发布，但不属于 GenericAgent 的原始代码。

## 2. 规范与平台依据

实现所依据的公开规范与平台文档：

| 依据 | 用途 | 链接 |
|---|---|---|
| Chrome DevTools Protocol（CDP） | `Page` / `Runtime` / `Network` / `Fetch` / `DOM` / `Input` / `Target` 等域；截图、网络拦截、控制台、文件上传 | <https://chromedevtools.github.io/devtools-protocol/> |
| Chrome Extensions Manifest V3 | 扩展清单、Service Worker、`chrome.scripting` / `debugger` / `alarms` / `storage` / `downloads` | <https://developer.chrome.com/docs/extensions/reference/manifest> |
| WebSocket（RFC 6455） | 扩展与本机 daemon 之间的桥接传输 | <https://www.rfc-editor.org/rfc/rfc6455> |
| HTML Living Standard | 页面解析与序列化（`html5ever` / `markup5ever_rcdom` 实现） | <https://html.spec.whatwg.org/> |
| URL Standard | URL 解析、规范化与本地文件路径判定 | <https://url.spec.whatwg.org/> |
| HTTP Semantics（RFC 9110） | 本机 HTTP API 与页面内 `fetch` 语义 | <https://www.rfc-editor.org/rfc/rfc9110> |

扩展最低版本 `minimum_chrome_version: 120` 的推导依据记录在 `assets/tmwd_cdp_bridge/background.js` 文件头：`scripting.executeScript` 的 `world: "MAIN"`、`storage.session`、`alarms` 的 30 秒最小粒度等能力下限。

## 3. 第三方依赖

### 3.1 Rust 直接依赖

版本以 `Cargo.toml` 为准，实际锁定版本见 `Cargo.lock`。

| crate | 版本约束 | 许可证 | 用途 |
|---|---|---|---|
| `anyhow` | 1.0 | MIT OR Apache-2.0 | 错误上下文 |
| `axum` | 0.7 | MIT | 本机 HTTP / WebSocket 服务 |
| `clap` | 4.5 | MIT OR Apache-2.0 | CLI 参数解析 |
| `dirs` | 6.0 | MIT OR Apache-2.0 | 用户配置目录 |
| `dunce` | 1.0 | CC0-1.0 OR MIT-0 OR Apache-2.0 | Windows 路径规范化 |
| `fs2` | 0.4 | MIT OR Apache-2.0 | 文件锁 |
| `html5ever` | 0.27 | MIT OR Apache-2.0 | HTML 解析 |
| `libc` | 0.2 | MIT OR Apache-2.0 | Unix 权限与进程控制 |
| `markup5ever_rcdom` | 0.3 | MIT OR Apache-2.0 | DOM 树 |
| `path-clean` | 1.0 | MIT OR Apache-2.0 | 路径清理 |
| `reqwest` | 0.12 | MIT OR Apache-2.0 | 本机 HTTP 客户端 |
| `serde` | 1.0 | MIT OR Apache-2.0 | 序列化 |
| `serde_json` | 1.0 | MIT OR Apache-2.0 | JSON |
| `tokio` | 1.48 | MIT | 异步运行时 |
| `url` | 2.5 | MIT OR Apache-2.0 | URL 解析 |
| `uuid` | 1.18 | Apache-2.0 OR MIT | 会话 / token 生成 |

传递依赖的版本与许可证可由 `cargo metadata` 或 `cargo deny` 复核。

### 3.2 Node 侧

- 运行时无第三方 npm 依赖；`package.json` 只有本项目的 5 个平台二进制子包（`optionalDependencies`）。
- CI 使用 GitHub Actions 官方 action 与 `dtolnay/rust-toolchain`、`Swatinem/rust-cache`。

## 4. 名称与商标

- `GenericAgent`、`TMWebDriver`、`simphtml`、`tmwd_cdp_bridge` 的名称与相关权利归上游项目及其作者所有。
- `Chrome`、`Google Chrome` 是 Google LLC 的商标。
- 本项目与上述项目/公司相互独立；提及名称仅用于说明来源与兼容性，不构成背书。

## 5. 许可合规

- 本项目以 MIT License 发布，全文见 [LICENSE](./LICENSE)。
- GenericAgent 的 MIT 版权声明与许可全文收录于 [NOTICE](./NOTICE)。
- 如果你分发修改后的版本，请保留 `LICENSE`、`NOTICE` 与本文件，并在派生文件头保留来源说明。

## 6. 变更记录

| 日期 | 变更 |
|---|---|
| 2026-11-02 | 首次整理并公开来源、规范依据与依赖清单 |
