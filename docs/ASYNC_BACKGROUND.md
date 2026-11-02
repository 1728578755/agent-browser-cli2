# 后台运行与异步任务

> 目标：让智能体在**不打断用户当前浏览**的前提下操作页面，并且不必阻塞等待长任务。
> 本文描述后台运行与异步任务的行为、用法与边界。实现集中在 `src/server.rs`（任务队列、等待语义）、`src/cli.rs`（`--async` / `job`）、`assets/tmwd_cdp_bridge/*`（后台保活、降低页面开销）。

---

## 1. 三项能力

| 能力 | 用户感受 | 实现方式 |
|---|---|---|
| 后台打开与操作 | 标签页在后台创建，不切换、不抢焦点，用户继续做自己的事 | `open --background` 设 `active:false`；默认是前台打开；受控标签页设置 `autoDiscardable:false`，避免被"内存节省"丢弃 |
| 异步任务 | 提交后立刻拿到 `job_id`，智能体可以先去干别的，或直接把控制权交还用户 | daemon 内 `JobStore` + `tokio::spawn`；`--async` 只负责提交，`job wait` 负责取结果 |
| 后台等待 | 隐藏标签页里的 `wait` / `--wait-js` 不会因为定时器节流而超时 | 一次注入 + 页面内 `MutationObserver`：谓词只编译一次，DOM 变化立即求值，非 DOM 条件由页面定时器兜底；daemon 只在注入失败（如换页）时补注入 |

---

## 2. 行为变更（重要）

### 2.1 `open` 默认前台，`--background` 才后台

```bash
# 默认：前台打开，切到新标签页（不聚焦系统窗口）
agent-browser-cli open https://example.com

# 后台打开：不切标签页、不抢焦点，用户继续做自己的事
agent-browser-cli open https://example.com --background

# 需要连窗口一起聚焦时
agent-browser-cli open https://example.com --focus
```

- 无论前台还是后台，**新标签页都会成为后续命令的默认目标**，所以 `open` 之后可以直接 `exec` / `click`，不需要每次 `--tab`。
- `open` 会最多等 3 秒，直到新标签页成为可操作会话；返回里 `result.ready` 表示是否可以立即操作。
- `--background` 与 `--focus` 互斥（clap 校验）。

### 2.2 受控标签页禁止自动丢弃

扩展在创建标签页后调用 `chrome.tabs.update(tabId, { autoDiscardable: false })`。
这样即使用户打开了很多标签页，Chrome 的"内存节省"也不会把我们正在操作的页面回收掉，避免"命令执行到一半标签页没了"。

### 2.3 长任务的上限与失败语义

- `exec --timeout` 默认 60 秒，可放大到 600 秒；超时返回 `ok:false` + `exec_timeout`，不会以成功的形状返回。
- `scan --timeout` 同样生效，页面内容抓取超时返回 `scan_timeout`。
- 异步任务没有额外总时限：各操作按自己的 `timeout` 约束，`job wait` 只影响"等多久取结果"，不影响任务本身。

### 2.4 页面侧开销降低（不打扰用户）

- 右下角状态角标：只在页面**可见**时轮询。
- 内容脚本补注入：按扩展版本只做一次。
- 标签页列表同步：250ms 去抖，一次导航触发的多条 `onUpdated` 合并处理。

---

## 3. 异步任务用法

### 3.1 提交

```bash
agent-browser-cli --async exec 'return document.title'
agent-browser-cli --async scan --text-only --tab 123456
agent-browser-cli --async snapshot --details --limit 200
agent-browser-cli --async screenshot --full-page --out /tmp/full.png
```

返回：

```json
{
  "ok": true,
  "job": { "id": "0f3c…", "op": "exec", "status": "running", "created_at": 1793512483.7 },
  "hint": "agent-browser-cli job wait 0f3c…"
}
```

支持异步的操作：`exec`、`scan`、`snapshot`、`click`、`mouse-click`、`fill`、`send-keys`、`screenshot`、`save-pdf`。
`open` / `close` / `tabs` 等路由类命令保持同步（本身很快，且需要立刻拿到 tab 标识）。

### 3.2 取结果

```bash
agent-browser-cli job list                     # 最近任务
agent-browser-cli job get <job_id>             # 查一次
agent-browser-cli job wait <job_id> --timeout 300
agent-browser-cli job cancel <job_id>
```

`job wait` 会阻塞到任务结束；超时不会丢弃任务，返回 `error_code: "job_wait_timeout"` 并附带当前状态，可以稍后再查。

任务完成后的结构：

```json
{
  "ok": true,
  "job": {
    "id": "0f3c…",
    "op": "exec",
    "status": "succeeded",          // running | succeeded | failed | cancelled
    "elapsed_ms": 208,
    "result": { "status": "success", "js_return": "Example Domain", "tab_id": "…" },
    "error": null,
    "session_key": "browser-…:profile-…:123456"
  }
}
```

说明：

- 任务结果与同步接口完全一致，直接复用同一套执行函数，不存在"两套逻辑"。
- 取消是**协作式**的：等待类任务按 1 秒切片注入，取消在片间生效（约 1 秒内）；已经发出的页面脚本会执行完当前步骤。
- daemon 在**有任务运行时不会因空闲退出**（空闲自杀的 5 分钟计时会让路），任务结束才恢复计时。
- 任务历史最多保留 100 条，超出后清理已完成的最旧记录。
- 结果只在 daemon 内存里；daemon 重启后 `job get` 会返回 `job_not_found`。需要长期留存的产物请落到文件（`screenshot --out` / `save-pdf --out`）。

### 3.3 `health` 新增字段

```json
{ "jobs": { "running": 1, "total": 7 } }
```

`status` 输出中也会带上该字段，方便判断"daemon 是不是正忙"。

### 3.4 多步管道：`run`

一次进程执行多步，省掉 N−1 次进程启动与 HTTP 往返：

```bash
agent-browser-cli run steps.json
```

```json
{
  "steps": [
    { "op": "exec",   "args": { "script": "return document.title" } },
    { "op": "fill",   "args": { "target": "#q", "value": "hello", "has_value": true } },
    { "op": "click",  "args": { "target": "#submit", "wait_js": "return !!document.querySelector('.done')" } },
    { "op": "screenshot", "args": { "out": "shot.png" } }
  ]
}
```

- 支持 `exec / scan / snapshot / click / mouse-click / fill / send-keys / upload / screenshot / save-pdf`。
- 默认遇错即停（`stopped_at` 标记位置），`--continue-on-error` 可继续。
- 返回逐步结果：`{ok, count, total, stopped_at, steps:[{index, op, ok, elapsed_ms, result|error}]}`。

### 3.5 失败可编程与输出体积

- `ok:false` → 退出码 `1`；`job wait` 超时 → 退出码 `3`；成功 → `0`。设置 `AGENT_BROWSER_CLI_EXIT0=1` 可恢复旧的恒 0 行为。
- 默认输出 **compact JSON**（单行）；需要人读时加 `--pretty`。
- `tabs` 默认把 URL 截断到 80 字符，`tabs --full` 输出完整 URL；内部数据模型始终保留完整 URL。

---

## 4. 后台等待：一次注入，页面内观察

Chrome 对**隐藏标签页**的定时器有两级节流：

1. 普通后台标签页：`setTimeout` / `setInterval` 最快约 1 秒一次；
2. 长时间后台（"intensive throttling"）：可降到 1 分钟一次。

在页面里用 `setTimeout` 循环轮询，等在后台标签页就会退化成秒级甚至分钟级一次——页面早已就绪，命令却迟迟返回不了。

现在的做法是**只注入一次**，等待在页面内完成：

```text
daemon: 注入一次；谓词在页面里编译一次
页面:   MutationObserver 监听 DOM 变化 → 变化即求值
        非 DOM 条件（时间、全局变量、网络回调）由定时器兜底
        命中 → 立即返回；到 timeoutMs 仍未命中 → matched:false
```

- `wait --selector` / `wait --text`：`MutationObserver` 直接监听文档，命中即返回（`mode:"observer"`）。
- `wait --js`：谓词只编译一次，之后在页面内反复求值；DOM 变化由观察器立即触发，定时器只兜底非 DOM 条件（`mode:"js-observer"`）。
- `exec --wait-js` 复用同一条路径；返回形状仍是 `js_return: { result, wait }`。
- 后台标签页里 `MutationObserver` 不受定时器节流影响，这是它能替代轮询的关键。
- daemon 侧只在**注入本身失败**时按剩余时间重试（典型场景是等待期间发生导航、旧执行上下文被销毁）；正常路径只有一次往返。
- 可取消的等待按 ≤1 秒切片注入，让 `job cancel` 与 Ctrl-C 在片间生效；同步调用仍保持整段注入。

输出字段：

| 字段 | 含义 |
|---|---|
| `mode` | `observer`（selector / text）或 `js-observer`（js 谓词） |
| `polls` / `checks` | 页面内的求值次数，可用来判断页面反应快慢 |
| `injections` | daemon 实际注入次数，正常为 1；换页重试会增大 |

`--interval` 是页面内两次求值的最小间隔（默认 50ms）；对求值开销敏感的页面可以调大。

---

## 5. 能力边界（需要知道的 Chrome 限制）

| 限制 | 影响 | 缓解 |
|---|---|---|
| `chrome.debugger` attach 会显示"正在调试此浏览器"提示条 | 只要用到 CDP（snapshot / 截图 / 网络 / 控制台 / mouse-click）就会出现 | 普通 DOM 操作（exec / click / fill / send-keys）走 `scripting`，不 attach |
| 后台标签页不渲染 | `IntersectionObserver` 懒加载、图片延迟加载、`requestAnimationFrame` 可能不触发 | 需要完整内容时先 `--focus` 打开，或显式滚动（`scrollIntoView`）；后续可加 `Emulation.setFocusEmulationEnabled` 选项 |
| 后台标签页可能被冻结 | 长任务中途脚本执行被暂停 | 已用 `autoDiscardable:false` 防止丢弃；CDP 路径额外调用 `Page.setWebLifecycleState('active')` |
| 同一标签页只能被一个 debugger 占用 | 用户打开 DevTools 时 snapshot/截图会失败 | 错误会返回 `Another debugger is already attached`；建议此时改用 `exec` |
| 页面原生弹窗 | `alert/confirm` 会阻塞页面脚本 | 扩展在 CLI 执行期间临时抑制并 toast 提示，执行完恢复 |
| daemon / 插件重启 | 执行中的命令会失去结果 | 扩展侧每条连接持自己的 socket，发送失败不再抛空指针；daemon 断开时立即让挂起命令以 `extension_disconnected` 失败（实测 66ms），不再等 15~60s 超时 |

---

## 6. 实测基线（Windows + 真实 Chrome，仅本机参考）

| 场景 | 实测 |
|---|---|
| CLI 进程启动 + 提交异步任务（`--async exec`，不含页面执行） | 260 ~ 285 ms |
| 隐藏标签页执行 `return 1`（daemon 侧 `elapsed_ms`） | 205 ~ 215 ms |
| 隐藏标签页 `--wait-js`（条件约 2 秒后成立） | 单次注入，命中即返回；不再受定时器节流影响 |
| `scan --text-only`（example.com，含 CLI 启动） | ≈ 480 ms |
| 可见/隐藏标签页 `exec` 端到端 | 380 ~ 420 ms |

解读：进程启动占 60% 左右，页面执行约 200 ms 主要来自 MV3 Service Worker 唤醒 + `chrome.scripting` 调度；隐藏标签页会被 Chrome 降优先级，但**不再因为定时器节流而超时**。

---

## 7. 协议（daemon HTTP）

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/ping` | **无需 token**，返回 `{ok,running,pid,protocol}`；CLI 用它判断 daemon 存活 |
| POST | `/job/submit` | `{op, args}`；`args` 与对应同步接口的请求体一致 |
| GET | `/job/get/:id` | 单任务状态与结果 |
| GET | `/job/list?limit=20` | 最近任务（running 计数在顶层） |
| POST | `/job/cancel/:id` | 协作式取消 |

`/ping` 独立于鉴权是刻意设计：token 文件缺失或残留旧 daemon 时，不会再被误判为"未启动"从而重复拉起 daemon、白等 15 秒。

---

## 8. 自测

不依赖真实 Chrome 的端到端测试（假扩展）：

```bash
cargo build
node tests/mock_extension.mjs --self-test
```

覆盖：daemon 冷启动、后台打开、默认会话路由、`--async` + `job wait`、页面内观察器等待、`job list`、`health.jobs`、关闭标签页。

需要在真实浏览器里确认时，跑一遍最小序列即可：`open --background` → `exec 'return location.href'`（应命中新标签页）→ `--async exec` + `job wait`。

---

## 9. 后续可做

1. `open --wait-load`：等待 `readyState === 'complete'` + 网络空闲后再返回（当前只保证"会话可操作"）。
2. `job` 结果落盘（`--out`），避免 daemon 重启丢结果。
3. 后台模式的焦点模拟（`Emulation.setFocusEmulationEnabled`）与懒加载触发策略。

以上都还没有实现，欢迎在 issue 里讨论方案或直接提 PR。
