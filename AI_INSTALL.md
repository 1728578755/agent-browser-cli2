# AI 安装说明

把下面这段话发给 AI，让 AI 在你的本机环境里完成安装、配置 skill 和验证。

```text
请帮我安装 agent-browser-cli：https://github.com/1728578755/agent-browser-cli

要求：
1. 优先使用 npm 安装：npm install -g @sleepinsummer/agent-browser-cli。
2. 先询问我希望把 Chrome 扩展解压到哪个本地目录。
3. 从 GitHub 最新 Release 下载 `chrome-extensions.zip`，解压到我指定的目录，并指导我在 Chrome 中加载解压后的 `tmwd_cdp_bridge` 扩展目录。
4. 如果之前已经加载过扩展，必须在 chrome://extensions 里重新加载该扩展，确保最新 `config.js`、`content.js` 和 `background.js` 生效。
5. skill 从 npm 包内置的 `skills/agent-browser-cli` 复制安装，不需要从 GitHub 下载 SKILL.md；先执行 `agent-browser-cli install-skill --dry-run` 展示安装计划，向我确认后再执行 `agent-browser-cli install-skill`。
6. 执行 agent-browser-cli status、doctor、tabs、open 验证可用。
7. 如果 npm 平台包暂不支持当前系统，再回退到源码构建：cargo build --release。
```

## 1. 安装 CLI

优先使用 npm 全局安装：

```bash
npm install -g @sleepinsummer/agent-browser-cli
agent-browser-cli --help
```

当前 npm 包按平台安装原生二进制：

```text
@sleepinsummer/agent-browser-cli
@sleepinsummer/agent-browser-cli-darwin-arm64
@sleepinsummer/agent-browser-cli-darwin-x64
@sleepinsummer/agent-browser-cli-linux-x64
@sleepinsummer/agent-browser-cli-linux-arm64
@sleepinsummer/agent-browser-cli-win32-x64
```

Windows ARM64 没有预编译包，需要自行 `cargo build --release`，再用 `AGENT_BROWSER_CLI_BIN` 指向生成的二进制。

Linux 使用前请确认本机 Chrome / Chromium 支持安装扩展。

如果在 WSL 中使用，当前版本依赖 WSL 访问宿主机 `localhost` 上的 Chrome 桥接服务。建议环境：

```text
WSL 2.0.0+
Windows 11 22H2+
.wslconfig 中启用 networkingMode=mirrored
```

如果当前平台包暂未发布或安装失败，使用源码构建：

```bash
git clone https://github.com/1728578755/agent-browser-cli.git
cd agent-browser-cli
cargo build --release
./target/release/agent-browser-cli --help
```

## 2. 加载 Chrome 扩展

先询问用户希望把扩展解压到哪个本地目录，例如：

```text
请告诉我 Chrome 扩展希望解压到哪个目录，例如 ~/agent-browser-cli-extension。
```

然后下载最新 Release 中的 `chrome-extensions.zip` 并解压到该目录。zip 解压后内部目录仍是 `tmwd_cdp_bridge`：

```bash
EXT_PARENT="$HOME/agent-browser-cli-extension"
mkdir -p "$EXT_PARENT"
ZIP_URL="$(curl -fsSL https://api.github.com/repos/1728578755/agent-browser-cli/releases/latest | grep -o '"browser_download_url": "[^"]*chrome-extensions.zip"' | head -n 1 | cut -d '"' -f 4)"
curl -fL "$ZIP_URL" -o "$EXT_PARENT/chrome-extensions.zip"
unzip -o "$EXT_PARENT/chrome-extensions.zip" -d "$EXT_PARENT"
```

解压后扩展目录应为：

```text
$EXT_PARENT/tmwd_cdp_bridge
```

在 Chrome 打开：

```text
chrome://extensions
```

开启“开发者模式”，加载已解压扩展目录：

```text
$EXT_PARENT/tmwd_cdp_bridge
```

如果之前已经安装过旧版 GenericAgent 的 `tmwd_cdp_bridge` 扩展，可以继续使用同协议旧扩展；但建议加载最新 Release 解压出来的 `tmwd_cdp_bridge` 并点击“重新加载”。

当前扩展配置应包含：

```js
const TID = '__agent_browser_cli_bridge_26c9f1';
```

Chrome 至少需要打开一个正常网页标签页，不要只停留在 `about:blank` 或 `chrome://` 页面。

## 3. 安装 skill

`agent-browser-cli install-skill` 使用 CLI 包内置的 skill 目录作为来源。npm 全局安装时，来源位于 npm 包内部的 `skills/agent-browser-cli`，不会联网，也不会从 GitHub 拉取 `SKILL.md`。源码构建时，来源为仓库内的同名目录。

默认实体安装目录：

```text
~/.agents/skills/agent-browser-cli
```

Codex、Claude、Kimi CLI、Cursor、Gemini 等目录只创建指向主安装目录的软链接，不复制多份实体文件：

```text
~/.codex/skills/agent-browser-cli -> ~/.agents/skills/agent-browser-cli
~/.claude/skills/agent-browser-cli -> ~/.agents/skills/agent-browser-cli
~/.config/agents/skills/agent-browser-cli -> ~/.agents/skills/agent-browser-cli
~/.cursor/skills/agent-browser-cli -> ~/.agents/skills/agent-browser-cli
~/.gemini/skills/agent-browser-cli -> ~/.agents/skills/agent-browser-cli
```

安装前必须先展示真实计划，并让用户确认。dry-run 输出中的“复制内置 skill 目录”就是本次安装的实际来源：

```bash
agent-browser-cli install-skill --dry-run
```

确认后再执行：

```bash
agent-browser-cli install-skill
```

`--yes` 只适合脚本化安装，不建议默认使用：

```bash
agent-browser-cli install-skill --yes
```

安装命令不会覆盖 Codex/Claude/Kimi CLI/Cursor/Gemini 目录下已存在的非软链接实体路径；即使使用 `--yes` 也会跳过并提示用户手动处理。

## 4. 验证

```bash
agent-browser-cli status
agent-browser-cli doctor
agent-browser-cli tabs
agent-browser-cli open https://www.baidu.com
```

成功时，`status` 应返回 `healthy: true` 和 `summary: "ready"`；`tabs` 会返回 `ok: true`，并包含当前 Chrome 标签页数量。
`open` 应能原生新开标签页，不应使用 `exec --monitor` 或 `window.open` 代替。

如果常驻服务需要重载最新代码：

```bash
agent-browser-cli restart
```

## 5. 使用入口

拿到标签页 ID 后，可以执行：

```bash
agent-browser-cli scan --tab <tabId> --text-only
agent-browser-cli exec --tab <tabId> 'return document.title'
```

完整命令和浏览器操作 SOP 见安装后的：

```text
~/.agents/skills/agent-browser-cli/SKILL.md
```

源码仓库中的对应文件为：

```text
skills/agent-browser-cli/SKILL.md
```
