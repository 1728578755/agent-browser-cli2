# 最佳实践

面向两类读者：把 atb 接进自己智能体的工程师，以及直接用命令行的人。

---

## 1. 给每个智能体一个稳定身份

默认身份是 `default`。多个智能体同时跑时，虽然 `--tab` 能保证目标正确，但"裸命令"会共用同一个默认标签页。

```bash
# 方式一：写进智能体的环境（推荐）
export ATB_AGENT=reviewer

# 方式二：每条命令显式指定
atb --agent reviewer open https://example.com
```

优先级：`--agent` → `ATB_AGENT`（旧名 `AGENT_BROWSER_CLI_AGENT`）→ 终端会话标识 → `default`。
同一台机器开两个 Windows Terminal 窗口时，`WT_SESSION` 不同，会自动隔离。

**判断是否隔离成功**：两个身份各自 `open` 不同页面后，各自执行不带 `--tab` 的
`exec 'return location.href'`，应当分别返回自己打开的那个 URL。

---

## 2. 批量操作：一条命令，而不是一个循环

```bash
# 好：1 个进程、1 个 HTTP 请求、daemon 内并发
atb read --tab url:docs --max-chars 2000

# 差：N 个进程串行，每个都要建连、解析参数
for id in 111 222 333; do atb read --tab $id; done
```

实测（6 个标签页、同一台机器）：批量 138ms，串行 648ms。

选择器的选取顺序建议：

1. 能确定 id → `--tab 111,222`（最快、最明确）
2. 按业务分组 → `--tab url:order` / `--tab title:订单`
3. 调试/探索 → `tabs --ids` 看清单，再用 `--tab @1-3`
4. 清理 → `close --tab ...` 或 `cleanup`

---

## 3. 先 dry-run，再执行

会影响多个标签页的命令，先看一眼解析结果：

```bash
atb click --tab all '#load-more' --dry-run
```

输出里的 `targets` 是 daemon 实际会打的标签页。如果多了一个不该出现的（比如用户自己开的同域页面），
换成更精确的 `--tab` 再执行。

同样的思路适用于 `open`：`open url1 url2 --dry-run` 只做语法与本地文件校验，不会开标签页。

---

## 4. 收尾一定要清理

任务结束调用：

```bash
atb cleanup --dry-run   # 确认清单
atb cleanup             # 关闭本智能体 open 的标签页
```

`cleanup` 的依据是"本工具 `open` 过的会话"，不按 URL 匹配，所以不会关掉用户自己的页面。
多个智能体共用一台机器时，各自清理各自的；需要一次性收干净用 `--all-agents`。

**不要**用 `close --tab all`（不支持），也不要用 `pkill chrome` 这类粗暴手段。

---

## 5. 版本配套与升级

三个版本：CLI/daemon 的 API 协议（当前 5）、扩展的消息协议（当前 4）、扩展的文件版本（当前 5.2.9）。

```bash
atb status | jq '.compatibility'
```

- `ok:true`：三方配套；
- `ok:false` + `issues[].kind:"extension_outdated"`：扩展旧了。用仓库里的
  `assets/atb_bridge` 覆盖扩展目录，然后在 `chrome://extensions` 重新加载；
- CLI 报 `CLI 与 daemon 版本不配套`：旧 daemon 还占着 18767。先 `restart`，
  不行就按提示结束占用端口的进程。

升级顺序建议：**先换扩展文件 → 重载扩展 → 再升级 CLI（或先 `atb restart`）**。反过来也能用，
只是中间会有一段时间 `compatibility.ok:false`。协议 5 的 CLI 会直接拒绝连旧 daemon，
不会"尽力而为"地解析旧响应。

**覆盖扩展文件不等于重载扩展**：5.0.0 的改动（页面操作走 ISOLATED world、角标默认关闭）
都在扩展现行代码里，不点「重新加载」就完全没生效——`atb stealth` 的 `extension_ok` 会是 false。

---

## 6. 少用 `exec`，多用封装命令

| 场景 | 推荐 | 原因 |
| --- | --- | --- |
| 读正文 | `read` | 单次注入、按预算截断，比 `document.body.innerText` 省 token |
| 找元素 | `find` / `snapshot` | 返回可操作的 selector / `@eN` 引用 |
| 点击 | `click` / `mouse-click` | 处理滚动、遮挡、真实事件序列 |
| 等待 | `wait --selector/--text/--js` | 页面内 MutationObserver，不占进程 |
| 网络 | `net hook` / `net mock` | 页面侧包装，抗 CSP 更好 |

`exec` 适合：封装命令覆盖不到的一次性读取、调试、以及在隔离世界跑脚本。

---

## 7. 稳定性与性能

- **别用固定 `sleep`**：用 `wait`，或 `exec --wait-js`。
- **大页面**：`read --max-chars` / `scan --fast` / `snapshot --max-nodes` 限流。
- **并发上限**：`/multi` 默认 8 路。页面很重时降到 4 路更稳（目前通过 `--tab` 分组间接控制）。
- **长任务**：超过 30 秒的操作用 `--async` + `job wait`，不要阻塞调用方。
- **daemon 空闲退出**是正常行为（默认 300 秒且无扩展连接时），不需要常驻；扩展连着时它不会退出，下一条命令也会自动拉起。



## 9. 被站点判定为自动化时怎么排查

先别改浏览器参数，先看**站点到底读到了什么**。这一步只有 `atb stealth audit` 能回答：

```bash
atb stealth audit
atb stealth audit --tab 111 --timeout 20
```

输出里的 `score` / `level` 只是概览，真正要读的是 `findings[].scope`——它决定了这锅该谁背、该按什么顺序处置。

### 处置顺序

**第 0 步：确认扩展是新版**

`extension_ok:false` 说明浏览器里跑的还是 5.0.0 之前的扩展。此时审计可能显示"干净"，但页面操作仍在往 MAIN world 写全局——**先按返回的 `fixes` 重载扩展，再重新审计**，否则后面全是错的。

**第 1 步：`scope:"self"` —— 本工具自己的，先清掉**

| finding id | 含义 | 处置 |
| --- | --- | --- |
| `own_globals` | 页面 window 上有本工具注入的全局函数 | `atb stealth scrub` |
| `own_dom_badge` | 页面 DOM 里有扩展角标节点 | popup 里关掉「显示状态角标」，或 `scrub` 临时移除 |
| `own_net_hook` | `fetch` / XHR 被网络钩子包装过 | `atb network stop` 或 `scrub`；只抓包用 `net mock --engine cdp` |

```bash
atb stealth scrub
# → restored / removed / remaining
```

注意 `scrub` 会连带停掉 `net hook`。正在抓包的页面别顺手 scrub，会断流。

**第 2 步：`scope:"browser"` —— 启动方式的问题，页面内修不了**

| finding id | 典型原因 | 处置 |
| --- | --- | --- |
| `navigator_webdriver` | 用 `--enable-automation` 或调试端口拉起 Chrome | 用日常的 Chrome 手动启动，装扩展连 daemon；不要用自动化框架的启动器 |
| `ua_headless` / `no_plugins` / `zero_outer_size` | headless 模式 | 换有头模式重开浏览器 |
| `software_webgl` | 传了 `--disable-gpu` / `--use-gl=swiftshader` 或虚拟机无 GPU | 去掉这些参数，用真实 GPU |
| `no_timezone` | headless 环境未设时区 | 设置系统时区 |
| `notification_permission_mismatch` | headless 下常见 | 有头模式重开 |

这一类**不要**试图用页面 JS 去"补"——改 `navigator.webdriver` 之类只是给这一个页面贴创可贴，换页面就露，而且等于伪造。

**第 3 步：`scope:"unknown"` —— 别人的痕迹**

- `automation_framework`（`__playwright` / `__puppeteer` / `__selenium` / `cdc_` 等）：说明这个浏览器 profile 被别的自动化框架接管过，去掉对应框架的注入 / 启动参数，或用干净的 profile。
- `patched_network`（`fetch`/XHR 非原生，但本工具没开 hook）：多数是油猴脚本或别的扩展改的，逐个停用定位。
- `chrome_runtime_exposed`：别的扩展声明了 `externally_connectable` 或注入了 web_accessible resource。


---

## 10. 排障顺序

```text
命令报错
  ├─ 看 error_code
  │   ├─ invalid_* / unsupported_*  → 命令写错，按 hint 改
  │   ├─ target_not_found / no_target_tabs → tabs --ids 复核目标
  │   ├─ extension_outdated         → 重载扩展（见第 5 节）
  │   └─ timeout / csp_blocked      → 换命令或加超时
  ├─ 站点判定为自动化
  │   ├─ stealth audit → 看 findings[].scope
  │   ├─ self         → stealth scrub / network stop
  │   └─ browser      → 换有头模式、真实 GPU（见第 9 节）
  └─ 连接类问题
      ├─ status       看 summary / compatibility / next_steps
      ├─ doctor       逐项自检（不启动 daemon）
      └─ logs --tail  看 daemon 日志
```
