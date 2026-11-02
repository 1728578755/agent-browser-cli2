<div align="center">

# agent-browser-cli

面向 Agent 的浏览器感知与控制 CLI，把真实 Chrome 会话变成可复用的标签页扫描、页面 JS、Cookie、CDP 和截图能力。

浏览器感知 · 页面控制 · Chrome 登录态复用 · CDP · 条件等待 · Agent Skill 集成

<p>
  <a href="https://github.com/1728578755/agent-browser-cli"><img src="https://img.shields.io/badge/CLI-agentbrowsercli-2ea44f" alt="CLI agentbrowsercli"></a>
  <a href="https://github.com/1728578755/agent-browser-cli/blob/main/LICENSE"><img src="https://img.shields.io/badge/License-MIT-green" alt="License MIT"></a>
  <a href="https://github.com/1728578755/agent-browser-cli"><img src="https://img.shields.io/badge/sys-win%2Fmac%2Flinux-0078D6?labelColor=0078D6&color=C0C0C0" alt="sys win/mac/linux"></a>
  <a href="https://github.com/1728578755/agent-browser-cli/releases"><img src="https://img.shields.io/badge/release-v0.3.8-orange" alt="release v0.3.8"></a>
  <a href="https://github.com/1728578755/agent-browser-cli/pulls"><img src="https://img.shields.io/badge/PRs-welcome-brightgreen" alt="PRs welcome"></a>
</p>

[AI 一句话安装](#ai-一句话安装) · [手动安装](#手动安装) · [Chrome 扩展](#chrome-扩展) · [更新](#更新) · [更新日志](CHANGELOG.md) · [卸载](#卸载) · [友情链接](#友情链接)

中文 | [English](README_EN.md)

</div>

`agent-browser-cli` 是一个面向 Agent 的浏览器感知与控制工具。它通过 Chrome 扩展连接用户真实浏览器，保留登录态和 Cookie，提供标签页扫描、页面 JS 执行、Cookie 读取、CDP 控制、截图、文件上传、下拉框点击等能力。

本项目不是 Selenium / Playwright。它更适合在已有浏览器会话中辅助 Agent 精确读取页面和执行操作。

## 项目信息

- 当前版本：`0.3.8`
- 支持平台：macOS（arm64 / x64）、Linux（x64 / arm64）、Windows（x64，含 WSL）
- 未提供预编译包的平台：Windows ARM64（`win32-arm64`）需自行 `cargo build --release`，再用 `AGENT_BROWSER_CLI_BIN` 指向生成的二进制
- 浏览器：Chrome；Edge 亦可使用，需各装一份扩展
- Linux 支持前提：本机 Chrome / Chromium 需要支持安装扩展
- WSL 支持前提：需使用 `WSL 2.0.0+`，并建议在 Windows `11 22H2+` 下启用 `networkingMode=mirrored`，以便 WSL 连接宿主机 `localhost` 上的 Chrome 桥接服务

## 致谢

本项目基于 [sleepinginsummer/agent-browser-cli](https://github.com/sleepinginsummer/agent-browser-cli) 的 0.3.7 代码继续开发；该上游项目又派生自 [GenericAgent](https://github.com/lsdefine/GenericAgent) 的 Web 工具链，包括 `TMWebDriver`、`simphtml` 和 `tmwd_cdp_bridge` 扩展相关思路与实现。

本项目与上述两个上游项目均无隶属或背书关系。感谢它们提供的浏览器桥接、页面简化、CDP 控制和实践 SOP；0.3.7 之上的二次开发内容见 [CHANGELOG.md](./CHANGELOG.md) 的 v0.3.8 段。

完整的代码派生链、二次开发范围、规范依据与依赖许可证见 [SOURCES.md](./SOURCES.md)；两个上游的 MIT 版权声明与许可全文见 [NOTICE](./NOTICE)。

## AI 一句话安装

```text
请阅读 https://github.com/1728578755/agent-browser-cli/blob/main/AI_INSTALL.md，按说明安装 CLI、加载 Chrome 扩展，并添加 `skills/agent-browser-cli/SKILL.md`。
```

## 相对 GenericAgent 的改进（0.3.7 基线）

以下为 0.3.7 基线相对 GenericAgent 的改动；本仓库在 0.3.7 之上的增量见 CHANGELOG。

- 从 GenericAgent 中拆出浏览器控制能力，以 CLI 形式提供给 Codex、Claude Code、OpenCode 等 Agent 工具使用。GenericAgent 浏览器插件不需要重新安装，可以共用同一个插件。
- 避免每次命令都重新初始化浏览器连接。
- 新增启动锁，避免多个 CLI 并发启动时重复绑定底层端口。
- 增加 skill：`skills/agent-browser-cli/SKILL.md`，供 AI 参考使用。
- 若干优化，缩短命令执行时间。
- 优化 CDP debugger attach/detach 流程，减少页面闪烁和异常退出后的残留 attach。
- CLI 端使用 Rust 实现。

## 他能做的事情

1. 自动化测试
   可以复用真实浏览器环境做页面流程验证、表单提交、按钮点击、跳转检查、登录态页面测试。
2. 前端页面 Debug
   可以读取 DOM、执行 JS、查看页面状态、截图确认效果，辅助定位前端交互、渲染和数据问题，对接后端接口。
3. 页面样式调试
   可以在真实页面里执行 JS 修改 DOM / CSS，临时验证样式、布局和交互效果，但更偏辅助调试，不是完整设计工具。
4. 网页数据采集
   可以读取页面内容、表格、列表、Cookie 和接口相关状态，适合处理需要登录态的页面数据提取。
5. 浏览器操作脚本化
   可以把打开页面、切换标签页、执行 JS、截图、上传文件等操作串成脚本，做重复性网页任务。
6. Agent 辅助操作网页后台
   适合让 AI Agent 操作管理后台、配置页面、低代码平台、表单系统等已有网页工具。
7. 页面结构分析
   可以简化 HTML、识别主要内容区和列表结构，帮助 Agent 更快理解复杂页面。
8. 安全研究和逆向辅助
   可以在真实浏览器会话里观察页面行为、执行调试脚本、读取前端状态，辅助分析前端逻辑和接口调用。

## 他的能力

1. 扫描当前 Chrome 标签页，获取页面标题、URL 和标签页 ID。
2. 切换到指定标签页，复用已有页面和登录态。
3. 打开新标签页，支持直接访问目标 URL。
4. 在页面中执行 JavaScript，读取 DOM、表单、状态和页面数据。
5. 读取当前页面 Cookie，方便处理登录态相关任务。
6. 调用 Chrome CDP 能力，执行更底层的页面控制。
7. 截取页面截图，用于视觉检查和页面确认。
8. 上传本地文件到网页文件选择框。
9. 操作下拉框、按钮、表单等常见页面元素。
10. 后台运行：`open --background` 可在后台创建标签页、不抢焦点；异步任务用 `--async` + `job` 系列命令，长任务不阻塞调用方。

## 后台与异步

`open` 默认前台打开（切到新标签页）；加 `--background` 则不切页、不抢焦点，新标签同样会成为后续命令的默认目标。

```bash
agent-browser-cli open https://example.com              # 前台打开
agent-browser-cli open https://example.com --background # 后台打开
agent-browser-cli exec 'return location.href'            # 直接作用于新标签页
agent-browser-cli --async scan --text-only              # 提交后台任务，立刻返回 job_id
agent-browser-cli job wait <jobId> --timeout 300        # 需要结果时再取
agent-browser-cli run steps.json                        # 一次进程执行多步
```

细节与限制见 [docs/ASYNC_BACKGROUND.md](./docs/ASYNC_BACKGROUND.md)；
已实现 / 下一步 / 边界与性能数据见 [docs/FEATURE_STATUS.md](./docs/FEATURE_STATUS.md)。

扩展有更新时，需要到 `chrome://extensions` 重新加载一次扩展目录；此后（2.3 起）扩展会自己盯着 `background.js` 的内容哈希，磁盘再改动会在空闲时自动重载，不用反复手点。
不确定浏览器里跑的是不是最新扩展时，运行 `agent-browser-cli status`：`extension.stale: true` 或出现 `warning` 就说明还没重新加载。

## 目录结构

```text
.
├── Cargo.toml                    # Rust 工程配置
├── src/                          # Rust CLI / 常驻服务 / bridge
├── assets/tmwd_cdp_bridge/       # Chrome MV3 扩展
├── assets/page_*.js              # 页面脚本主副本
├── assets/simphtml_opt.js        # 页面简化脚本
├── scripts/                      # 构建/同步辅助脚本（sync-extension-assets.mjs 等）
├── npm/                          # npm 启动脚本
├── skills/agent-browser-cli/     # Agent Skill
├── tests/                        # Node 侧测试、假扩展与手工验收脚本
├── docs/                         # 行为说明与能力现状
└── .github/workflows/            # CI 与发布流水线
```

`assets/page_*.js` 会由 `npm run sync:extension` 逐字节复制到 `assets/tmwd_cdp_bridge/ops/`：前者供 daemon 打包走 eval 通道，后者在严格 CSP 页面下由扩展注入。**改页面脚本只改 `assets/` 下那份**，然后跑同步脚本；`npm run check:extension` 只比对不写盘，CI 用它做一致性守卫，`prepack` 打包前也会自动同步一次。

## 手动安装

### npm 安装

```bash
npm install -g @sleepinsummer/agent-browser-cli
agent-browser-cli tabs
```

> **注意**：`@sleepinsummer` 是上游项目的 npm 作用域。本仓库尚未发布独立 npm 包，这条命令安装的是上游 0.3.7；需要本仓库的 0.3.8 请使用下面的源码构建。

### 本地源码构建

```bash
cargo build --release
./target/release/agent-browser-cli tabs
```


## Chrome 扩展

1. 推荐从 [最新 Release](https://github.com/1728578755/agent-browser-cli/releases/latest) 下载 `chrome-extensions.zip`，下载后解压，Chrome 打开 `chrome://extensions`，开启“开发者模式”，点击“加载已解压的扩展程序”，选择解压后的 `tmwd_cdp_bridge` 目录。

2. 本地源码构建时，也可以直接加载扩展目录：

```text
assets/tmwd_cdp_bridge
```

3. Chrome 需要至少打开一个正常网页标签页，不要只停留在 `about:blank` 或 `chrome://` 页面。
4. 扩展连接后会在页面右侧显示 Chrome 插件提示角标。角标支持拖动位置，鼠标悬浮时展开；10 秒无命令后自动隐藏，也可以点击 `本次隐藏` 手动隐藏，本次服务连接周期内不再显示，约 300 秒服务断开并下次重连后恢复。
5. 默认只校验握手来自某个 Chrome 扩展，即「装了扩展就能连」。需要加固时用 `agent-browser-cli trust-extension <extensionId>` 限定扩展白名单、`--token <value>` 要求握手带 token；两者都配置时同时生效，`--clear` 回到兼容模式。详见 [SKILL.md](./skills/agent-browser-cli/SKILL.md#加固-websocket-身份可选)。

###  自定义Chrome插件的ws监听端口

- `18765`：默认插件 WebSocket 端口，Chrome 扩展连接使用，可通过 `agent-browser-cli set-extension-port <port>` 修改。
- `18767`：CLI HTTP API 端口，供 CLI 复用会话，不能作为插件端口使用。

CLI 修改插件端口：

```bash
agent-browser-cli set-extension-port 18766
```

该命令会写入配置文件；如果 daemon 正在运行，会自动重启 daemon，让新端口立即生效。

也可以手动修改配置文件。配置文件位于 `~/.agent-browser-cli/config.json`，不存在时会自动生成：

```json
{
  "extension_port": 18765
}
```

手动修改示例：

```json
{
  "extension_port": 18766
}
```

手动改配置后需要执行 `agent-browser-cli restart`，daemon 才会按新端口重新监听。

Chrome 插件 popup 中也可以修改插件端口并立即重连。插件端口必须和 CLI 配置中的 `extension_port` 一致。

### Profile Label

多 Chrome Profile / 多浏览器实例下，`profile_id` 和 `browser_id` 较长。可以给每个 Chrome Profile 设置短 label，之后用 `--profile <label>` 操作。

```bash
agent-browser-cli lookup tab <tabId>
agent-browser-cli lookup browser <browser_id>
agent-browser-cli profile-label set work --profile <profile_id>
agent-browser-cli tabs --profile work
```

也可以在对应 Chrome Profile 的扩展 popup 中设置 Profile Label。label 只作为别名，内部路由仍使用 `browser_id:profile_id:tab_id`；如果当前 daemon 内 label 匹配到多个 profile，CLI 会报歧义。推荐用 CLI 设置 label，因为 CLI 会校验当前 daemon 内跨 Profile 唯一性；popup 是本地便捷入口，不保证跨 Profile 唯一。`tabtree` 默认截断 URL 并省略 `session_key` 以减少 token，需完整字段时加 `--full`。




### 弹窗抑制

扩展不再默认重写页面的 `alert` / `confirm` / `prompt`。只有 CLI 执行页面脚本命令期间会临时抑制原生弹窗，命令结束后恢复，避免长期污染业务页面全局函数。

## 快速自检

```bash
agent-browser-cli tabs
agent-browser-cli open https://www.baidu.com
```

成功时 `tabs` 会返回（`open` 返回 `opened_tab_id` / `opened_session_key` / `ready`）：

```json
{
  "ok": true,
  "result": {
    "status": "success",
    "metadata": {
      "tabs_count": 1
    }
  }
}
```

## 常用命令

README 只保留快速入口；完整命令和浏览器操作 SOP 见 [skills/agent-browser-cli/SKILL.md](./skills/agent-browser-cli/SKILL.md)。

```bash
agent-browser-cli tabs
agent-browser-cli scan
agent-browser-cli exec 'document.title'
agent-browser-cli snapshot
agent-browser-cli click @e1
agent-browser-cli fill @e2 'text'
agent-browser-cli send-keys Enter --target @e2
agent-browser-cli mouse-click @e1
agent-browser-cli screenshot --full-page
agent-browser-cli save-pdf
agent-browser-cli network start
agent-browser-cli network list
agent-browser-cli console start
agent-browser-cli console list
agent-browser-cli cookies
agent-browser-cli upload 'input[type=file]' ./a.pdf
agent-browser-cli run steps.json
agent-browser-cli batch steps.json --concurrency 8   # 并发批量（跨标签）
agent-browser-cli read --max-chars 8000 --links      # 结构化读页面
agent-browser-cli find 'a[href]' --limit 20           # 找元素
agent-browser-cli find --text '登录'                   # 按可见文本找
agent-browser-cli hover '#menu'
agent-browser-cli select '#city' --value sh
agent-browser-cli check 'input[name=agree]'
agent-browser-cli scroll --to bottom
agent-browser-cli wait --selector '.result' --timeout 10
agent-browser-cli back
agent-browser-cli reload
agent-browser-cli frames                              # 列出 iframe
agent-browser-cli exec --frame <frameId> '...'        # 在 iframe 内执行
agent-browser-cli inject 'window.__x = 1'             # 一次性注入
agent-browser-cli inject --persist hook --match '*://*.example.com/*' '...'
agent-browser-cli inject --list
agent-browser-cli inject --remove hook

# 读取：隐藏内容 / 大页面 / iframe
agent-browser-cli read --include-hidden                  # 连 display:none 的内容一起返回
agent-browser-cli read --max-nodes 20000 --budget-ms 800 # 超大页面限流
agent-browser-cli scan --fast                            # 只取紧凑文本，十万级页面也快
agent-browser-cli read --frame <frameId>                 # 读 iframe 内的内容

# 页面网络：发请求 / 记录 / mock
agent-browser-cli net fetch https://api.example.com/me -H 'Accept: application/json'
agent-browser-cli net hook --capture-bodies
agent-browser-cli net hook --persist                      # 刷新后仍生效
agent-browser-cli net mock /api/user --request-header 'x-token: t' --response-status 201
agent-browser-cli net log --limit 20
agent-browser-cli net mock /api/user --status 200 --body '{"id":1,"name":"mock"}'
agent-browser-cli net routes
agent-browser-cli net unroute /api/user                      # 删页面规则
agent-browser-cli net unroute /api/user --engine cdp          # 删 CDP 规则
agent-browser-cli net reset

# 验证码 / CF 挑战：只识别、等待、转人工（不做自动破解）
agent-browser-cli challenge status
agent-browser-cli challenge solve                             # 默认最多等 120s
agent-browser-cli challenge wait                              # 默认最多等 30s
agent-browser-cli challenge focus

# 下载监听
agent-browser-cli downloads --limit 10
agent-browser-cli download https://example.com/report.pdf --filename report.pdf
agent-browser-cli download-wait --id 7 --timeout 120

# 多浏览器编排（Chrome / Edge 各装一份扩展）
agent-browser-cli browsers
agent-browser-cli broadcast exec --script 'return location.href'
agent-browser-cli broadcast read --args '{"max_chars":2000}' --browser chrome --browser edge
agent-browser-cli open https://a.com https://b.com https://c.com --background  # 并发开多个标签
agent-browser-cli tabs --full
agent-browser-cli open https://example.com --window --focus
agent-browser-cli open https://example.com --group-title demo
agent-browser-cli close --tab <tabId>
agent-browser-cli --async exec 'return document.title'
agent-browser-cli job list
agent-browser-cli job wait <jobId>
agent-browser-cli tabtree
agent-browser-cli tabtree --full
agent-browser-cli tabtree --profile <profile_label>
agent-browser-cli tabtree --tab <tabId>
agent-browser-cli lookup tab <tabId>
agent-browser-cli lookup browser <browser_id>
agent-browser-cli profile-label set work --profile <profile_id>
agent-browser-cli profile-label clear --profile <profile_id>
agent-browser-cli status
agent-browser-cli doctor
agent-browser-cli logs --tail 100
agent-browser-cli install-skill --dry-run

# WebSocket 身份加固（可选，默认不启用）
agent-browser-cli trust-extension <extensionId>   # 只允许名单内扩展连接
agent-browser-cli trust-extension --token <value> # 握手必须带 ?token=<value>
agent-browser-cli trust-extension --list
```

> **这项加固是单向的，别把它当安全边界。** `trust-extension` 校验的是「谁可以连 daemon」；反方向没有校验——**扩展不会确认它连上的是不是真的 daemon**。
>
> daemon 空闲 5 分钟会自动退出，18765 端口随之空出来。这段时间里本机其它进程可以抢占该端口，扩展按自己的重试节奏连上去之后，仍会执行对方下发的命令（包括读取任意站点 cookie、调用任意 CDP method）。也就是说：**配置 token 只能防「别的扩展连你的 daemon」，防不住「你的扩展连上别的进程」**。
>
> 实际影响与建议：这是本地提权/横向移动的放大器，需要攻击者已经能在本机执行代码。敏感操作请在有你在场时进行；不使用时 `agent-browser-cli stop` 关掉 daemon，能显著缩短空窗期。

## 退出码

命令返回 `ok:false` 时退出码非 0，脚本和智能体可以直接用 `$?` / `&&` 判断成败，不必解析 JSON。

```text
0    成功
1    业务失败（ok:false）；含 challenge wait/solve 超时、exec --monitor 里页面脚本抛错
3    等待超时：job wait 超时、download-wait 超时
130  任务被取消：job wait 等到 cancelled
```

`status` 的 `ok` 跟随 `healthy` —— daemon 未运行、端口不一致、扩展未连接、没有可用页面时都是 `ok:false` + 非 0 退出码。需要旧的「恒 0」行为时设置环境变量 `AGENT_BROWSER_CLI_EXIT0`（值任意，含 `0`）；不依赖旧行为就不要设。

daemon 默认空闲 5 分钟退出，可用 `AGENT_BROWSER_CLI_IDLE_TTL`（秒）覆盖；缩短空窗期可降低本机端口被抢占的风险。

## 更新

完整版本记录见 [CHANGELOG.md](./CHANGELOG.md)。

ai一句话更新
```text
请阅读 https://github.com/1728578755/agent-browser-cli/blob/main/AI_INSTALL.md，按说明更新 CLI、重新下载插件 zip 到用户指定位置，用户手动加载 Chrome 扩展，并更新相关 SKILL.md。
```

如果 Chrome 扩展有更新，在 `chrome://extensions` 中重新下载 zip 覆盖，然后重新加载 `assets/tmwd_cdp_bridge` 扩展。

### 从更早版本升级时注意

- **`change.top_change` 已移除**：`--monitor` 现在只返回 `change = {changed, added, removed, attributes, text, samples}`。解析过 `top_change` 的脚本要改读 `changed` / `samples`；观察失败时还会带 `monitor.reason`（`navigated` / `superseded` / `unsupported`）。
- **`status` 的退出码变了**：`ok` 跟随 `healthy`，daemon 不健康时不再退出 0。CI / 脚本里用 `status && ...` 做前置检查的要改成看 `summary`，或临时设 `AGENT_BROWSER_CLI_EXIT0=1`。
- **`--monitor` / 等待类命令的失败不再伪装成功**：`exec --monitor` 里页面脚本抛错、`job wait` 等到 `failed`/`cancelled`、`download-wait` 超时、`challenge wait/solve` 超时都会给出非 0 退出码。
- **`trust-extension` 是可选的**：不配置时仍是「装扩展即用」。多人共用一台机器、或多个扩展共存时建议配白名单 / token。注意它只约束「谁能连 daemon」，不约束「扩展连的是谁」——详见上面的加固说明。
- **扩展资源要一起更新**：`assets/page_*.js` 与 `assets/tmwd_cdp_bridge/ops/` 是同一份脚本的两个副本，从源码构建时先 `npm run sync:extension`，否则严格 CSP 页面会跑旧代码。


## 卸载

先停止常驻服务：

```bash
agent-browser-cli stop
```

然后按需清理：

```bash
npm uninstall -g @sleepinsummer/agent-browser-cli
rm -rf ~/.agent-browser-cli
rm -rf ~/.agents/skills/agent-browser-cli
```

`~/.agent-browser-cli/` 存放 `config.json`、`api-token`、`ws-trust.json`、`net-hooks.json`、`daemon.lock` 与日志；删除前请确认其中没有还要保留的配置或凭据。

最后在 Chrome 扩展管理页中移除 `Agent Browser CLI Bridge` 扩展，或删除已加载的 `assets/tmwd_cdp_bridge` 扩展配置。

## 隐私与数据

本项目只在本机工作：CLI 与常驻 daemon 通过 `127.0.0.1` 通信，扩展只连接本机端口，不上传页面内容、Cookie 或截图。

需要了解的数据边界：

- **Cookie 与页面内容**：`cookies`、`read`、`exec` 等命令会把读取结果返回给调用方，通常进入 Agent 的对话上下文，并可能被发送给对应的模型服务商。敏感站点请自行评估。
- **网络拦截**：`net hook --capture-bodies` 会记录请求/响应体，`net mock` 会改写响应；内容仅在本机流转，可用 `net reset` 清除。
- **本地文件**：配置、token、持久化网络规则与日志位于 `~/.agent-browser-cli/`；日志可能包含 URL 与错误信息。
- **扩展权限**：详见 [PRIVACY.md](./PRIVACY.md)，包括 `cookies`、`debugger`、`downloads` 等权限的用途。

## 合规使用

本工具用于自动化测试、前端调试、数据采集与授权范围内的安全研究。使用者应遵守目标网站的服务条款、robots 协议与所在地法律，不得用于未授权的访问、绕过访问控制或侵犯他人数据。项目不提供验证码自动破解能力，`challenge` 系列只做识别、等待与转人工。

## 参与贡献

欢迎 Issue 与 PR。提交前请阅读 [CONTRIBUTING.md](./CONTRIBUTING.md)；安全问题请按 [SECURITY.md](./SECURITY.md) 私下报告，不要公开提交可利用细节。

## 友情链接

- [LINUX DO - 新的理想型社区](https://linux.do/)
- [GenericAgent--复旦团队研发|仅仅~3K行代码 Self-Evolving Agent](https://github.com/lsdefine/GenericAgent/tree/main)

## 许可证

MIT License. See [LICENSE](./LICENSE).
