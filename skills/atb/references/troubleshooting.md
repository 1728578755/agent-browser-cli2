# 排障

先执行目标命令，不要用本文做开机自检。daemon 由命令自动拉起；只有命令失败，且输出指向连接、端口、扩展或标签页时，才按下表定位。

| 现象 | 先看 | 节 |
| --- | --- | --- |
| 连接被拒绝、`daemon_not_running` | `atb tabs` 是否成功 | 1 |
| `extension_not_connected`（status 摘要）/ `extension_disconnected`（命令错误码） | Chrome 是否打开、扩展是否启用 | 2 |
| `port_mismatch` | 扩展 popup 端口与配置端口 | 3 |
| `CLI 与 daemon 版本不配套` / `extension_outdated` | `status` 的 `compatibility` | 4 |
| `no_target_tabs` / `no_active_tabs`（status 摘要） | 是否有普通网页 | 5 |
| `csp_blocked` / `page_protected` | 页面 CSP 与来源 | 6 |
| `element_not_found` / `invalid_selector` | 选择器与 iframe | 7 |
| `challenge_timeout` | `challenge status` 的 reason | 8 |
| 下载停在 `interrupted` | 磁盘与浏览器下载设置 | 9 |
| 两个智能体互相看到对方页面 | 身份与 `--tab` | 10 |
| 其它 | 日志 | 11 |

## 1. daemon 没起来

**症状**：`status` 的 `summary:"daemon_not_running"`（`capabilities` 等命令会作为 `error_code` 返回），或连接 `127.0.0.1:18767` 被拒绝。**判断**：daemon 默认空闲 300 秒且无扩展连接时退出；扩展连着时不退，断开后有 60 秒重连宽限。`running:false` 不代表安装损坏，下一条目标命令会自动拉起；`doctor` 不会启动 daemon。

**处理**：先 `atb tabs` 触发拉起；仍失败用 `atb restart`；daemon 占着 18767 且 token 不匹配时用 `atb restart --force`。

## 2. 扩展未连接

**症状**：`summary:"extension_not_connected"`，或 `connection.extension_connected:false`。**判断**：按顺序排除浏览器没开、扩展没加载/没启用、popup 端口与 `~/.atb/config.json` 不一致、覆盖过文件但没点「重新加载」。

**处理**：`atb status` 看 `connection`；冷启动慢时用 `atb open <url> --warm` 先唤醒 service worker。重载入口在 `chrome://extensions` 的扩展卡片上。

## 3. 端口不一致

**症状**：`summary:"port_mismatch"`，扩展连不上但 daemon 正常。**判断**：扩展 WebSocket 端口默认 `18765`，可用 `set-extension-port` 改；`18767` 是 CLI HTTP API，固定且不能作扩展端口。

**处理**：先 `atb doctor` 与 `atb restart`；确认要改端口再 `atb set-extension-port <port>`（写 `~/.atb/config.json` 并重启 daemon），然后在扩展 popup 填同一端口并重载。

## 4. 协议或版本不配套

**症状**：`CLI 与 daemon 版本不配套`、`extension_outdated`、`upgrade_recommended:true`、`stealth` 返回 `extension_ok:false`。**判断**：三方须配套：API 协议 `5`、扩展消息协议 `4`、扩展文件版本 `5.2.9`。不配套时命令明确报错，不会静默跑错。

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `CLI 与 daemon 版本不配套` | 旧 daemon 仍占 18767 | `atb restart`；不行用 `--force` |
| `extension_outdated` | 扩展文件是旧的 | 覆盖 `assets/atb_bridge` 后重载 |
| `upgrade_recommended:true` | 扩展低于期望，core 命令可用 | 重载扩展直到 `compatibility.ok` |
| `extension_ok:false` | 扩展低于 5.0.0 或没重载 | 按返回的 `fixes` 重载后再跑 |

覆盖扩展文件不等于生效；不点「重新加载」，页面脚本仍是旧版。

## 5. 没有可用标签页

**症状**：`summary:"no_active_tabs"`、`active_tabs:0`，或命令返回 `no_target_tabs`（旧版本/兼容路径可能仍是 `no_tabs`）。**判断**：当前只有 `about:blank`、`chrome://` 页面或扩展页，这些不能作为操作目标。

**处理**：`atb open https://example.com --background`，再用 `atb tabs --ids` 确认。`open` 也失败时回到第 2 节查扩展，不要反复重开浏览器。

## 6. CSP 或受限页面

**症状**：`error_code:"csp_blocked"`（hint 提到 CSP / inline script），或 `error_code:"page_protected"`。

**判断**：前者是页面 CSP 禁止注入内联脚本；后者是页面内嵌了其它扩展的 iframe，Chrome 拒绝 attach。
两者都看 `error_code` 与返回里的 `via` / `synthetic` 区分。

- `csp_blocked`：页面禁止内联脚本。改用 `read`、`snapshot --js`、`find`、`click`、`fill`、`wait`（都走扩展注入的 `ops/page_*.js`），返回带 `via:"no-eval"`；要在隔离环境跑脚本用 `exec --world isolated`。
- `page_protected`（`chrome.debugger.attach` 被拒，页面内嵌了别的扩展的 iframe）：**不是死路，不要急着重试**
  | 命令 | 回落通道 | 结果标记 |
  | --- | --- | --- |
  | `screenshot` | `captureVisibleTab` | `via:"captureVisibleTab"` |
  | `screenshot --full-page` | 逐屏滚动 + 拼图 | `via:"captureFullPageStitched"`、`stitched`、`slices`、`truncated` |
  | `save-pdf` | 整页长图 → 单页图片型 PDF | `via:"stitched-image-pdf"`、`text_selectable:false` |
  | `mouse-click` | 页面内合成 pointer/mouse 事件 | `synthetic:true`、`trusted:false` |
  | `snapshot` | DOM 版 | `mode:"js"` |

  仍需要调试器的只剩 `network`、`console`、`upload`、`send-keys`、`net mock --engine cdp`、元素/后台标签页截图。
- 错误里已经点名挡路的扩展（例如「检测到页面内嵌了扩展「XX助手」的 iframe」）：需要真实输入事件或可检索 PDF 时，让用户临时禁用该扩展后重试。
- 不要把「合成事件成功」当成真实点击：部分站点只认 `isTrusted` 事件，看到 `synthetic:true` 要自行确认业务是否真的响应。
- 不要改页面 CSP 头或注入 `<meta>` 绕过，那是改站点行为。

## 7. 选择器打不到

**症状**：`invalid_selector`、`unsupported_selector_syntax`、`element_not_found`，或点击无报错但没效果。

| 错误码 | 含义 | 处理 |
| --- | --- | --- |
| `invalid_selector` | 括号、引号没配平 | 按 `hint` 的「第 N 行」定位，`example` 可照抄 |
| `unsupported_selector_syntax` | 写了 `text=` / `css=` / `xpath=` | 文本用 `find --text`，XPath 用 `--xpath`，CSS 直接写 |
| `element_not_found` | 没命中或不可见 | `find` 确认；在 iframe 里就加 `--frame` |
| 点击无效 | 目标被浮层盖住 | 看 `warnings`，`action_target_covered` 时用 `mouse-click` |

```bash
atb find 'button.submit' --limit 20
atb frames
atb wait --selector '.result' --visible --timeout 10
```

## 8. 挑战页超时

**症状**：`challenge_timeout`、`challenge_blocked`，或 `challenge.auto_solve.reason` 给出结论。

**判断**：`open --challenge auto`、`challenge solve`、`challenge auto` 会主动点击组件并等待放行；`challenge wait` 才是纯等待。失败按 reason 处置，兜底由智能体自己点，最后才 `focus` 转人工。不破解访问控制，也不保证通过。

| reason | 含义 | 处理 |
| --- | --- | --- |
| `no-widget-frame` | 组件没渲染 | 等更久，或 `--reload-after 10` 重载一次 |
| `click-no-effect` | 点了没反应 | `widget` 重新取坐标后再点；仍无效再 `focus` |
| `verifying_not_completed` | 停在「正在验证」 | 等待或 reload 后重试；卡住再 `focus` |
| `widget-removed-no-nav` | 组件消失但没跳转 | `challenge status` 复查，必要时 `reload` |

```bash
atb challenge status
atb challenge wait --timeout 30 --focus-after 8
atb challenge widget
atb challenge click --at 320,410
atb challenge focus
```

仍不通过就按「`widget` 取坐标 → `click --at` 自己点 → `status` 复查」兜底；只有图片点选、短信/邮箱验证这类需要人判读时再 `focus` 交给人，并明确说明需要人工完成验证；不要循环狂点。

## 9. 下载不完成

**症状**：`downloads` 显示 `interrupted`，或 `download-wait` 超时（退出码 `3`，error 文本以 `download_wait_timeout:` 开头）。**判断**：下载由浏览器执行，atb 只发起和查询。常见原因是磁盘满、默认下载目录无写权限、站点触发「保留/放弃」确认、直链需要登录 Cookie。

**处理**：

```bash
atb downloads --state interrupted --limit 10
atb download <url> --filename report.pdf --dir atb-downloads
atb download-wait --id <downloadId> --timeout 300
```

`--dir` 只能是 Chrome 默认下载目录下的子目录，不能写绝对路径。

## 10. 多智能体串扰

**症状**：裸命令打到别人的页面；`cleanup` 关掉了不属于自己的标签；两个智能体读到同一 URL。**判断**：两个调用方都落到 `default` 身份，或都显式指向同一个 `--tab`。

**处理**：

```bash
ATB_AGENT=reviewer atb open https://a.example.com --background
ATB_AGENT=reviewer atb exec 'return location.href'
ATB_AGENT=reviewer atb tabs --mine
ATB_AGENT=reviewer atb cleanup --dry-run
```

- 身份优先级：`--agent` → `ATB_AGENT` → 终端会话标识 → `default`。
- 给每个智能体固定 `ATB_AGENT`；关键操作显式写 `--tab`。
- `cleanup --all-agents` 会清掉所有人的页面，只在确认后使用。

## 11. 日志、重置与重装

日志固定在 `~/.atb/logs/daemon.log`：

```bash
atb logs              # 最近 100 行纯文本
atb logs --tail 300
```

`logs` 不支持 `--follow`，也不返回 JSON。重点找 attach/detach 失败、端口监听、扩展握手与版本拒绝。

Chrome 顶部出现「正在控制你的浏览器」通常正常：执行 CDP 命令时 attach，命令结束或 daemon 退出时 detach。只有命令同时失败或日志报错时才继续排查。

重置按影响从小到大：

```bash
atb restart                 # 重新读配置并拉起 daemon
atb restart --force         # 强杀占用 18767 的进程
atb trust-extension --list
atb trust-extension --clear # 清空白名单与 token，回到兼容状态
```

`trust-extension` 是单向的：只约束「谁能连 daemon」，不校验「扩展连的是不是真 daemon」。清空后任何本地扩展都能连，确认环境可信再执行。

重装 skill 先看计划，不要直接覆盖：

```bash
atb install-skill --dry-run
atb install-skill
```

实体目录是 `~/.agents/skills/atb`：**每次安装都会整体更新**（plan 里的 `update_main`），有自定义修改先备份。

五个 Agent 目录（`~/.codex/skills`、`~/.claude/skills`、`~/.config/agents/skills`、`~/.cursor/skills`、`~/.gemini/skills`）只建软链接；同名位置若已是**实体目录**，会跳过且 `--yes` 也不覆盖。这条「不覆盖」只针对这五个目录，不包括主目录。

`~/.workbuddy/skills/atb` 不在写入范围内，升级后手动同步（先备份），装完用 `atb schema --json` 验证：

```bash
cp -r skills/atb/. ~/.workbuddy/skills/atb/
atb schema --json
```
