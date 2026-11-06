# 更新日志归档

本文件保留 0.3.7 及更早版本、以及主线重构前的逐条记录，仅作历史参考。
当前维护的更新日志见 [../CHANGELOG.md](../CHANGELOG.md)。

归档内容未做事实订正；其中出现的旧命令名 `agent-browser-cli` / `atb2`、旧端口或旧路径，
以当前代码与 `CHANGELOG.md` 为准。

---

所有重要变更都会记录在这里。日期使用北京时间自然日。

## [Unreleased] - 2026-11-04

> open/tabs/close 体验改进（P2-1 / P2-2 / P2-5 / P2-6 / P2-8）。全部向后兼容：不删除、
> 不改名任何旧字段，旧脚本无需改动。

### Added

- **`open --wait-for <networkidle|selector:S|text:T>`（P2-2）**：打开后继续等页面条件
  满足才算成功。networkidle 在页面内等「readyState=complete 且 1 秒无新请求/DOM 变更」；
  selector/text 复用 daemon 现成的 JS 条件轮询。超时跟随 `--timeout`，等不到的目标
  `ok:false` + `error_code:"wait_for_timeout"`，不影响其它目标。带 `--wait-for` 时
  `ready:true` 语义升级为「条件已满足」。
- **`open --warm`（P2-6）**：打开前先 ping 一次扩展（拉 tabs 列表）唤醒 service worker；
  ping 失败重试一次后照常打开。
- **`tabs --fields` / `--limit` / `--mine`（P2-5）**：字段白名单过滤（未知字段报
  `unsupported_flag` 退出码 2）、条数截断、只看本 agent open 过的标签页（daemon 侧按
  `agent_opened` 记录过滤）。
- **`close --tab group:<标题> --yes`（P2-8）**：按组关闭的显式确认形态。不带 `--yes`
  时报错并告知将关闭的标签数；`all` / `url:` / `title:` 仍然拒绝。关闭结果逐条列
  tab_id + title。

### Changed

- **open 返回统一（P2-1）**：单 URL 与多 URL 返回都额外带顶层 `targets`：
  `[{url, tab_id, ok, error?, ready, elapsed_ms}]`，并统一带 `count`。旧的
  `result.opened_tab_id`（单 URL）与 `tabs[]`（多 URL）字段原样保留；推荐新代码
  只解析 `targets`。多 URL 每个目标新增 `elapsed_ms`。

## [Unreleased] - 2026-11-03 / 11-04

> 本节记录一次完整代码排查（106 条）与首轮报告（10 条）的修复与结构整理，CLI 版本未变更（0.7.1）。
> **2026-11-04 补充**：后续审阅遗留与实测反馈（restart 自愈、错误码细分、四个新命令
> schema/capabilities/doctor --fix/scrape）也记入本节；**扩展文件版本为 5.2.3**（11-03 记载的
> 5.2.2 已被 11-04 部署覆盖）。

### Added

- **`atb schema --json`**：输出全部命令的机器可读 schema（参数名/类型/默认值/可选值 +
  主要 `error_code`），实现为 `src/cli/cmd_schema.rs` 的静态表（对照参数定义逐命令抄写），
  Agent 一次拿到用法，不必逐条 `--help` / 试错。
- **`atb capabilities`**：一次探测 daemon 版本、扩展版本与能力门禁
  （`features.core/open_session/group/window/current/challenge`，阈值在 CLI 侧写死并随输出给出，
  口径与 daemon 各自的版本校验一致）；daemon 未运行时报结构化 `daemon_not_running`。
- **`atb doctor --fix [--force]`**：在诊断之外自动修复——旧 daemon 占 18767
  （`netstat -ano` / `lsof` 找 PID，`--force` 才真杀，不带时只打印命令）、
  API token 失配（删除 `~/.atb/api-token` 并提示重试）、扩展过旧
  （打印带实际扩展目录路径的重载指引）。每条修复输出 `{fix, executed, result}`。
- **`atb scrape`**：批量抓取（CLI 侧编排，复用 daemon 的 open/exec/close，不改 daemon）：
  `--urls` 文件/逗号列表（去重）→ 分批后台开页（`--batch`）→ 并发执行 JS
  （`--concurrency`，`--js-file` 缺省取 `{title,url,text(前9000字)}`）→ 结果逐行写
  `--out`（JSONL：`{url, tab_id, ok, data, error, elapsed_ms}`）→ 批完关页 →
  失败退避重试（3s/5s/7s，最多 `--retry` 轮）→ 摘要输出。

### Changed

- **结果未知语义（V8-03）**：CLI 对 daemon 的 POST「请求已发出但响应未回」（超时 /
  写后读失败 / 连接中断）不再误报 `unexpected_error`，改为结构化
  `{"error_code":"outcome_unknown","outcome":"unknown"}`，退出码 **4**（新增，
  退出码表同步更新）；连接都没建立的失败仍按原样报错。新增全局
  `--idempotency-key <key>`：随 `x-atb-idempotency-key` 请求头发给 daemon，
  daemon 对 60 秒内同 key 的 POST 直接回放首次响应（响应体带
  `idempotent_replay:true`），动作类命令可安全重发。
- **迟到结果暂存（A1-09）**：扩展在请求超时后才回包的 exec 结果/错误不再静默丢弃，
  暂存 5 分钟（超限淘汰最旧）；新增端点 `GET /exec-result/:exec_id`
  （命中即取走，返回 `found` / `result` / `error` / `ttl_seconds`）；
  exec 超时错误信息附带 exec_id 与查询指引。

- **源码模块化**：`src/server.rs`（14 692 行）拆为 `src/server/` 下 29 个领域模块 +
  测试模块（`net` 再拆为 `settings/log/page/cdp/hook`）；`src/cli.rs`（4 635 行）拆为 `src/cli/` 下 15 个模块，并把原本 750 行的
  `run()` 拆成按命令族的 22 个处理函数。子模块共享同一命名空间（`mod.rs` 统一
  `pub(crate) use`），调用关系与行为不变。
- **退出码契约归一**：`main` 改为返回 `ExitCode`，任何失败都输出结构化 JSON；
  参数类错误统一退出码 2（新增 `usage_failure` / `usage_err!` 与 `job_not_found` 等错误码）。
  `ATB_EXIT0` 只在值为 `1`/`true` 时生效。
- **`--async` 语义统一**：不支持的命令直接报 `unsupported_flag`（退出码 2），
  不再静默按同步执行；`inject` / `frames` 补上真正的后台提交。
- **`--dry-run` 覆盖面与形状**：`close` / `run` / `batch` / `trust-extension` /
  `set-extension-port` 现在只输出计划；计划统一包进 `result`，与真实执行同一套
  `jq .result` 解析；`--out` 占位符只在真多目标时校验。

- **`atb restart` 自愈（P0 实测反馈）**：daemon 在跑但 API token 不匹配时，旧实现只提示
  「结束占用 18767 的进程后重试」然后退出，而 restart 本身就是干这件事的，形成自锁。
  现在默认输出结构化 `daemon_token_mismatch` 错误（退出码 2，附将执行的命令）；
  新增 `atb restart --force`：自动定位 LISTENING 在 18767 的 PID（Windows `netstat -ano` /
  macOS·Linux `lsof`）强杀后照常重新拉起 daemon（含防误杀自身进程的过滤）。
- **job 硬上限淘汰语义（D-05）**：任务表达到硬上限强制淘汰 running 任务时，daemon 侧
  现在留存根（最多 32 条，含 `evicted_at`）；`job get/wait` 命中存根返回
  `error_code:"job_evicted"` 与提示，不再把「daemon 侧丢弃」伪装成 `job_not_found`
  （那会被当成 id 写错）。
- **注释纠偏（V8-12）**：`assets/atb_bridge/background.js` 中「ext_ready 的 restartSeq
  daemon 侧从未解析」的说法不实（ws.rs 一直在解析并置 `cdpRebuildRequired`），已按
  实际契约改写：daemon 旁路解析、扩展侧仅上报。
- **清理过期 `allow(dead_code)`（V8-13）**：`config::load()`（cmd_status 已接入）与
  `preflight::clamp_timeout_seconds`（server/cli 的 `secs()` 已接入）的过期 allow 与
  注释已删除；其余 `allow(dead_code)` 逐处核验后保留（确为测试专用或未启用）。
- **配置兜底路径不再静默吞错**：Windows「备份 + 改名」退路中删除 `.bak` 失败时，
  现在输出带路径的警告（保留不因清理失败而整体报错的语义）；NotFound 视为正常。

### Fixed

- **click 自动降级真实鼠标（P2-3）**：CDP 路径 `click` 失败且错误属于 CDP 层
  （协议 / 批回包缺结果，如 `Runtime.callFunctionOn response missing`）时，自动
  降级走 `mouse-click` 路径（`Input.dispatchMouseEvent`），响应带
  `used_fallback:true` + `fallback_reason`；降级也失败时报原始错误 + 降级错误。
  页面内点击路径（`@e` 带 selector / `--frame`）不降级；`blocked` / `covered_by`
  遮挡告警不受影响。
- **受限站点报错人话化（P2-4）**：CDP 错误出口（`cdp_call` / `batch_cdp_call`）
  命中 Chrome 原文 `Cannot access a chrome-extension:// URL of different extension`
  时，翻译为结构化错误：`error_code:"page_protected"`、
  error:「该页面禁止调试器注入（页面内嵌了其他扩展的 iframe 或站点 CSP/扩展隔离）」、
  hint:「改用 read / scan / snapshot（走无 eval 通道），或 mouse-click 直接坐标点击」，
  并保留 `raw_error`。
- **早期审阅遗留（C-02 / C-04 / C-05 / C-06 / X-15）**：
  - C-02：`collect_dom_details` 只在 `--details` 时执行；旧实现不带 `--details`
    也无条件发起最多 25 批 CDP 往返，结果写进从未被读取的缓存字段。
  - C-04：CDP 快照的可操作角色集并入 JS 快照的 `file`（并集策略，AX 树特有角色
    保留）；两处角色集差异已在注释中写明边界。
  - C-05：`dom_mouse_click` 保持两次往返的原因（扩展 batch 协议 `$N` 占位只覆盖
    objectId/nodeId，坐标必须页面侧量出）已写入注释，逻辑未改。
  - C-06：`snapshot --js` 的 `next_offset` 加边界保护——页面回包未显式带
    `offset`/`has_more`/`next_offset`（旧版脚本）时不再给出 `next_offset`，
    避免调用方跟进后必然报「不支持 offset 分页」。
  - X-15：`stalledMs` 已由 challenge 侧接通（核验通过）；`skipGeneric` 接通
    daemon 侧（`SnapshotRequest.skip_generic` → 页面脚本），CLI flag 待 cmd 侧补充；
    `confirmMs` / `credentials` / `parseJson` / `innerTextLimit` 所需改动在
    challenge.rs / net.rs / read.rs，不在本轮修改文件范围，维持现状。
- **退出码兜底**：`job_evicted` 归入参数类错误白名单（退出码 2）；错误文本的中文
  子串兜底（「未连接」等）只在 JSON 没有结构化 `error_code` 时才生效，daemon 补上
  typed error_code 后退出码不再漂移。
- **假成功（P0）**：`upload` / `mouse-click` 由 `batch_cdp_call` 逐条校验
  `results[*].ok`、`data.skipped` 与顶层 `failed`；`wait` / `broadcast` / `run` /
  `batch` / `multi` / `job` 的内层 `ok:false` 会上浮为退出码非 0；
  挑战「已通过」在探测失败时不再成立。
- **多智能体隔离**：`tokio::spawn` 前捕获并 `scope` 调用方身份，异步任务不再落到
  `default` 的标签；`preferred_default_session_key` 记录归属者。
- **超时参数护栏**：所有 `Duration::from_secs_f64` 走 `secs()` / `clamp_timeout_seconds`，
  `NaN` / `Inf` / `1e300` 不再 panic。
- **会话与状态机**：`prune_sessions` 真正生效；WS 断开能清掉 refresh 登记的会话；
  `agent-tabs.json` 原子写 + 损坏隔离；`net reset` 先停后删、规则改动广播到同 profile
  的所有标签、配置读改写加锁。
- **扩展侧**：SW 重启自愈（规则/采集状态恢复）、按 tab 串行 attach、
  `persistDebugInterceptState` 读-改-写、socket close 守卫、发送队列、
  `cookies clear --url` 的域匹配方向。
- **页面脚本**：`snapshot --js` 支持 `offset` 分页（`@e` 全页连续编号，可翻到 1000 条之后）；
  `net fetch` 不再把 HTTP 404 当命令失败（`http_ok` 单独承载 HTTP 成败）；
  规则自带 `response.body` 时不再缓冲原始响应体（`text/event-stream` 不会永久挂死）；
  `wait --text --visible` 生效（Rust 侧按文本节点判可见）、
  `setValue` 事后校验、`elementFromPoint` 为 null 时给出 warning、
  非法 selector 返回 `{ok:false}` 而不是抛到通道层。
- **分发链路**：`npm/bin/atb.js` 兜底报错不再 `ReferenceError`，信号退出码按
  `128+n`，Alpine(musl) 有明确提示；Release 产物按 `<target>-atb` 命名，不再互相覆盖。
- **目标解析错误语义细分（v8 V8-05 / 实测 P1-1、P1-2）**：daemon 的目标类错误统一带
  `error_code` / `retryable` / `retry_after_ms` / `hint`，CLI 按码映射退出码，
  不再依赖「未连接」等中文子串（文案变化不再影响脚本判断）；旧自然语言文本全部保留。
  原 `no_tabs` / `no_targets` 一个码背四种病因，细分为 `tab_closed`（单标签关闭）、
  `session_gone`（整个浏览器实例下线，error 带名下失效标签数，跨 browser / 扩展重载
  后整批 tab_id 失效不再被误报成「id 写错」）、`session_mismatch`（标签属于另一
  browser，hint 指向 `--browser`）、`no_target_tabs`（桥正常但没标签，先 open）、
  `extension_disconnected`（桥断开，retryable，`retry_after_ms: 3000`）、
  `target_not_found`（id 写错）、`target_ambiguous`（原 target_unavailable / 裸文本歧义
  错误归一）；`extension_outdated` 维持不变。`tabs` / `lookup` 结果与错误
  `candidates` 元素补 `browser_id` + `session_key`；`tabs` 空列表补
  `warning_code`（`extension_disconnected` / `no_target_tabs`）。
- **`net reset` 不再把休眠标签算成失败**：页面级清理的失败拆成 `failed`（页面明确报错）
  与 `unconfirmed`（超时/断连）。此前 100+ 标签的浏览器里几乎必然 `ok:false`，
  现在返回 `ok:true` 并在 `hint` 里说明有多少标签未确认。
- **配置写入在 Windows 上是真原子**：改用 `MoveFileExW(MOVEFILE_REPLACE_EXISTING |
  MOVEFILE_WRITE_THROUGH)`，读方不会再撞见「文件不存在」的真空期；仅在目标被占用时
  退回「备份 + 改名」并记录原因。新增依赖 `windows-sys`（原本已在依赖图内，无新增下载）。

## [扩展 5.2.x] - 2026-11-03

> 本节记录 **Chrome 扩展文件**（`assets/atb_bridge/manifest.json`）的版本，当前为 **5.2.3**。
> CLI 版本（0.7.x）与扩展版本独立演进；扩展↔daemon 的消息协议未变（仍为 **4**）。

### Changed

- **5.2.0 与 CLI 0.7.0 同步发布**：扩展显示名统一为 ATB，新增 `browser_label` 上报
  （`品牌 · 系统 · 自定义名 · #短码`），`onclose` 后按 0.3→0.6→1.2→2.5→5s 退避重连
  （alarms 只做长期兜底），调试器 attach 后 1.2s 内 detach。
- **5.2.1 与 CLI 0.7.1 同步发布**：扩展侧的维护性修复（挑战探测、页面脚本与网络拦截相关），
  不新增/删除消息字段；`ops/page_*.js` 与 `assets/page_*.js` 继续保持逐字节一致。
- **5.2.3（未发布，跟随下一版 CLI）**：一轮系统性修复，协议仍为 **4**。
  - `background.js`：`detachAllDebugSessions` 不再对 `ws-close` / `keepalive-lost` 清空
    持久化拦截快照 —— 旧实现会在每次 daemon 重启时把 `net route --persist` 的规则永久抹掉，
    导致重启后拦截不自愈（EXT-27）。
  - `page_challenge.js`：`challenge wait` 不再把「挂着未通过验证码组件但页面能浏览」
    判成已放行（要求 `vendor && interactive && !token_ready` 时继续等）；
    `stalledMs` 改由 daemon 下发后，静默挂死的快速失败链路恢复生效（B-05 / B-26）。
- **5.2.2**：服务端生命周期与页面脚本的缺陷修复，协议仍为 **4**。
  - `background.js`：SW 重启后自愈（规则/采集状态恢复并重新 enable Fetch/Network/Runtime）、
    按 tab 串行化 attach、状态快照读-改-写、`onclose` 守卫、发送队列、`cookies clear --url` 域匹配。
  - `page_snapshot.js`：支持 `offset` 分页，`@e` 全页连续编号，回传
    `offset/limit/returned/has_more/next_offset`。
  - `page_net.js`：`net fetch` 的 `ok` 不再取 `response.ok`（HTTP 4xx/5xx 走 `http_ok`）；
    规则自带 `response.body` 时不再缓冲原始响应体（SSE 不再挂死）。
  - `page_wait.js`：`--text --visible` 的可见性判定。
  - `page_action.js`：`setValue` 事后校验、`elementFromPoint` 为 null 时产出 warning、
    非法 selector 返回结构化错误。

### Fixed

- **挑战探测不写页面全局**：探测只读 DOM / 走扩展 ISOLATED world，避免 `window.__abc*`
  成为站点可读的自动化指纹（这是 5.0.0 起的口径，5.2.x 期间继续沿用）。
- **页面操作与角标默认隐蔽**：页面脚本默认 ISOLATED world；角标默认关闭且挂在
  closed Shadow DOM（宿主 id `atb-status-host`）。

### 说明

- 仓库内没有 5.1.x 的独立发布记录：版本从 5.0.3 直接到 5.2.0，5.2.1 为同轮收尾。
- 本节只覆盖扩展文件；命令行为变化见下方 0.7.x 各节。

## [Unreleased]

> 主题：**重写挑战求解器**——元素驱动、点一次就不再碰，并提供智能体自己点的兜底。

### Added

- **`atb launch`：让「已开始调试此浏览器」横幅彻底不出现**。横幅是 Chrome 自己的 UI，
  只要 `chrome.debugger.attach` 就会挂出来，运行时没有任何 API 能去掉；唯一开关是启动
  参数 `--silent-debugger-extension-api`（Chrome 156 的二进制里仍然存在，实测同一坐标
  attach：带参数无横幅、不带参数有横幅）。命令行参数只在进程启动时读，所以做成了
  「按正确参数重启」：
  - `atb launch --dry-run` 只输出计划（`args` / `would_close`），不碰任何进程；
  - 不带 `--restart` 时检测到同 profile 的实例就返回 `browser_already_running`，
    **绝不关窗口**（因为再启动一次只会转发 URL、参数静默失效）；
  - `--restart` 先发不带 `/F` 的 `taskkill`（等同点窗口的关闭按钮）并轮询确认退出，
    默认带 `--restore-last-session`。实测 **Chrome 在 Windows 上会忽略这个信号**
    （报「已发送终止信号」但进程照跑），所以超时后只报错，要加 `--force` 才强杀；
    强杀后 Chrome 会弹「要恢复页面吗」，结果里用 `restore_hint` 提醒；
  - 浏览器由 **daemon** 拉起而不是由一次性命令拉起：很多运行环境会把命令放进
    带「关闭即杀」的作业对象，命令一结束整棵进程树被回收，浏览器会跟着消失。
    顺带给子进程加了 `CREATE_BREAKAWAY_FROM_JOB`；
  - 已在用这条参数运行的实例会被识别出来（读进程命令行），返回
    `already_running:true` / `launched:false`，不会多此一举地重启；
  - 结果里带 `silent_debug_banner` / `args` / `closed_pids` / `restore_hint`，
    `--arg` 可透传额外启动参数。

### Fixed

- **挑战页不再被注入任何脚本（本轮最关键的一条）**。识别逻辑原先走 `eval` 进了页面的
  MAIN world，脚本里的 `window.__abcChlSeenAt` 就成了站方可读的自动化指纹：
  点击照样被接收（页面会显示「正在验证您是否是真人」），但 Turnstile 永远不给 token。
  现在 `challenge` 的页面侧调用一律走扩展的 ISOLATED world，脚本里也不再写任何
  `window.*`；识别判据改成纯 DOM（隐藏的 `cf-turnstile-response` 字段 + 无可见
  iframe/stage），不依赖 `window.turnstile` / `_cf_chl_opt`。
  实测同一站点连跑 7 次通过 6 次（6.6–12.9s），修复前是 0 次。

### Changed

- **`challenge_auto_solve` 整体重写**。旧实现用固定偏移 `(30, h/2)` + 固定节奏
  （等 1.2s、隔 5s 再点）猜落点，实测命中率极低。新实现只认四条可观测规则：
  组件出现就点；组件消失/换新/正文变「正在验证」= 点击被接收，立刻停手；
  3s 内毫无变化才算被吞、允许重点；token 出现或离开挑战页即通过。
- **落点改成勾选框方块中心**（局部 `(21,33)`，即宽度的 7%），不再压在 label 文案上。
- **鼠标轨迹从组件外滑入**，触发 mouseenter —— 起点在组件内部时页面同样会显示
  「正在验证」，但 Turnstile 服务端始终不给结果。
- **页面 status 查询降频到 400ms**，主节拍改用 CDP 组件查询：旧实现每 120ms 注入一次
  脚本，会把挑战页主线程占满，反而让组件迟迟不渲染（表现为整段预算内都看不到组件）。

### Added

- `atb challenge widget`：只返回组件的 `rect` / `url` / `click{x,y}`（含 local 坐标），
  给调用方自己点用。
- `atb challenge click --at X,Y`：按坐标发一次可信鼠标事件，点完直接回 status。
  自动求解失败时，智能体可以用 `widget` → `click --at` → `status` 三步自己完成，
  不需要人工介入。
- 求解结论新增 `evidence`（`widget-removed` / `widget-replaced` / `verifying`）与
  更准确的 `reason`（`verifying_not_completed` / `click-no-effect` / `no-widget-frame`）。



> 主题：**挑战判定元素化**——组件在 closed shadow root 里也能识别、也能点。

### Fixed

- **修正 0.7.1 的错误结论：Turnstile 不是「没下发」，而是藏在 closed shadow root 里**。
  CF 的托管挑战把 `challenges.cloudflare.com` 的 iframe 放进页面的 closed shadow root，
  页面 JS 的 `querySelectorAll('iframe')` 恒为 0、`#challenge-stage` 也不存在。
  旧实现据此判 `stalled: turnstile_not_rendered` 并放弃，实际勾选框就在页面上。
  现在判据改为「CF 挑战页 + `window.turnstile` + 隐藏的 `cf-turnstile-response` 字段 +
  无可见 iframe/stage」，输出 `turnstile.shadow_widget:true`、`interactive:true`。
- **`open --challenge auto` 不再因为「页面脚本说没有可点组件」就跳过求解**。组件是异步
  渲染的，open 完成那一刻往往还没建出来；现在一律先跑一次元素驱动的求解器，没有组件时
  它会很快返回，不会白耗预算。
- **点完不再按固定秒数等**。改为看元素证据：token 有值 / 已离开挑战页 / 组件被摘掉 /
  正文出现「正在验证」。`verifying` 一旦出现就锁住点击（连点会把 Turnstile 打回原点），
  只有在「完全没有证据」时才间隔 5s 重点一次，最多 2 次。
- **别的扩展挡住 CDP 时自动绕过**：页面里存在别的扩展注入的 `chrome-extension://` iframe 时
  Chrome 拒绝 `chrome.debugger.attach`（"Cannot access a chrome-extension:// URL of
  different extension"）。现在先摘掉这类 iframe 再重试 attach，`diag` 记
  `step:"attach" / stripped:<n>`。

### Added

- 求解结论 `auto_solve.reason`：`no-widget-frame` / `click-no-effect` /
  `verifying_not_completed` / `widget-removed-no-nav`，并且一路带进 `open` / `solve`
  的超时结果里（以前超时只丢一句 `need_human`，看不出卡在哪一步）。
- `tests/challenge_detect.test.mjs`：用 vm 造四种页面形状（closed shadow root / 确实没下发 /
  普通 widget / 普通页面）锁死判定口径。

## [0.7.1] - 2026-11-03

> 主题：**挑战静默挂死可识别**、`--background` 窗口最小化修正、诊断字段。

### Fixed

- **挑战「卡死」10s 内给结论，不再傻等满超时**。CF 的托管挑战有一种静默失败形态：Turnstile 的
  `api.js` 已加载（`window.turnstile` 存在），iframe 文档也拿到了 200，但页面里始终**没有**
  `challenges.cloudflare.com` 的 iframe、也没有 `#challenge-stage`，于是「请稍候…」永远转圈。
  以前 `open` 只会等满 `--challenge-timeout` 再回一句 `need_human: true`；现在页面侧计算
  `stalled_ms`，超过 10s 直接返回 `reason: "turnstile_not_rendered"` 并给出处置建议。
  实测这类挂死等 45s 也不会放行，提前返回不损失任何成功率。
- **`--background` 打开挑战页时窗口还是最小化的**。`chrome.tabs.create({ active: false })` 不会
  恢复已最小化的窗口，页面 `outerWidth=0` / `visibilityState="hidden"`，CF 的 Turnstile 在这种
  页面里**根本不执行**。现在等待期间一旦发现环境不可见，先调一次 `tabs switch`
  （`allowFocus:false`，只恢复窗口、不抢系统焦点）再继续等。
- **`challenge` 结果带上可诊断字段**：`reason`（`turnstile_not_rendered` / `window_not_visible` /
  `timeout`）、`env`（`visibility` / `hidden` / `has_focus` / `outer` / `inner`）、
  `turnstile`（`api` / `iframes` / `stage`）、`stalled_ms`。
- **`hints` 改用最后一轮的结论**。之前取的是首轮 status，所以永远是「通常几秒内自动放行」，
  真正的结论（组件没下发 / 窗口不可见）反而被丢掉。

### Added

- `challenge status` / `open --challenge auto` 现在能区分三类失败：站点没下发 Turnstile、
  窗口不可见、单纯超时 —— 三者的处置完全不同（换 IP / 恢复窗口 / 加长超时）。

## [0.7.0] - 2026-11-03

> 主题：**全量更名 atb**、挑战速度、多浏览器可读身份、调试横幅收敛、Cookie 清理。

### Changed（破坏性）

- **全面更名：`agent-browser-cli` → `atb`**。CLI/daemon、Cargo 包名、npm 包名（`@g1728578755/atb`）、
  平台子包（`@g1728578755/atb-<platform>`）、仓库内目录（`assets/atb_bridge`、`skills/atb`）、
  扩展显示名（`ATB`）、配置目录（`~/.atb`）全部同步。旧目录 `~/.agent-browser-cli` 不再读取，
  首次运行会在新目录重建配置（扩展端口写回 18765）。
- **环境变量**：主前缀仍是 `ATB_*`；`AGENT_BROWSER_CLI_*` 作为兼容别名保留一轮，文档不再宣传。

### Fixed

- **挑战页「慢」的真因**：判定为拦截后，旧实现只在 `--challenge` 且非 `--background` 时才把标签切前台，
  后台标签的定时器被节流，CF 挑战脚本要 20~30s 才跑完。现在**只要判定拦截就切 active**
  （`allowFocus:false`，不抢系统焦点）。实测同一页面：`--challenge off` 基线 0.7s，改造后 auto 0.96s
  （此前 12~22s 且经常超时）。
- **不再自动 reload 挑战页**：reload 会把 CF 挑战脚本从头再跑一遍，等于把走到一半的验证打回原点
  （旧实现固定把超时的 60% 花在 reload 上）。需要时用 `challenge wait --reload-after` 显式开启。
- **纯 interstitial 不再走 CDP 求解器**：CF managed challenge 没有可点控件，旧实现每轮都去 CDP
  找 iframe / 算锚点，找不到就白等一个 round（5~6s）。现在 `interactive:false` 直接进等待。
- **调试横幅不再长期驻留**：`chrome.debugger` attach 会在页面顶部挂「已开始调试此浏览器」，
  且会把视口顶掉约 40px。旧实现靠 `chrome.alarms` 延迟 detach，而 alarms 硬下限是 30s——
  等于「点一下页面，横幅挂半分钟」。现在用 `setTimeout` 1.2s 内 detach，alarm 只做 SW 被回收后的兜底。
- **daemon 不再因为「空闲」掉线**：只要还有扩展连着就不退出。旧策略 300s 无请求即退，
  而扩展重连依赖 `chrome.alarms`（≥30s），于是「隔一会儿再敲一条命令」经常撞上
  「没有可用标签页」或首条命令等 30~60s。
- **扩展快速重连**：`onclose` / daemon 未启动时按 0.3s→0.6s→1.2s→2.5s→5s 退避重试（约 20s 窗口），
  alarms 只做长期兜底。实测 daemon 重启后 2~4s 内恢复连接（此前最长等一分钟）。

### Added

- **多浏览器可读身份**：扩展上报 `browser_label`（`品牌 · 系统 · 自定义名 · #短码`，如
  `Edge · Windows · #5531`）。`tabs` / `browsers` / `lookup` 都会带这个字段；
  `--browser` 支持完整 id、id 前缀（≥8 字符）和标签子串（`--browser Edge`）。
- **`cookies --clear [--url <页面> | --domain <域>]`**：反复测挑战页 / 切登录态时用。
  按 url 清理能删到分区 cookie（`cf_clearance` 这类 CHIPS cookie 只有带 `partitionKey`
  的查询才看得到）；单站点命中超过 200 条会直接拒绝执行，避免范围算错误删。

## [Unreleased]

> 主题：`assets/page_*.js` 的 23 条修复收口 —— 这批全是
> 「跑起来不报错、行为和文档不一致」的静默失效，要么让命令假成功，要么让调用方拿到错的数据。

### Fixed

- **口令/支付信息不再出现在命令输出里**（JS-10）：`read` / `find` / `snapshot` / `scan` 对
  `type=password` 与 `autocomplete=current-password|new-password|cc-number|cc-csc|one-time-code`
  的字段值统一返回 `•••` 并标 `redacted:true`。以前 fill 完登录表单再 read（LLM 代理的标准工作流）
  会把明文口令一路带进 CLI 输出、agent 上下文与日志文件。
- **`@e` 引用不再点错元素**（JS-01）：`find` / `snapshot` 生成的 CSS 路径逐层验唯一
  （`querySelectorAll(path).length === 1`），不唯一就继续加祖先直到 `documentElement`。
  重复结构页面（卡片/列表/表格行）上，`@e37` 以前会落到第 1 张卡片的同位按钮并返回 `ok:true`。
- **被遮罩盖住的点击不再算「成功」**（JS-06）：`click` / `mouse-click` 在目标中心点被别的元素
  盖住时，结果带 `blocked:true` + `covered_by`，并把 `warnings:[{code:"action_target_covered"}]`
  提到顶层。以前这条信息只在页面脚本的 `hit` 字段里，daemon 与 CLI 都不消费。
- **React 受控 checkbox 真的会被勾上**（JS-05）：`check` / `uncheck` 改走原生 `click()`
  （React 的 onChange 对 checkbox 挂的是 click 事件），返回 `via:"click"`；状态已一致时
  `unchanged:true`，disabled 或被页面回滚时报 `ok:false` 而不是假成功。
- **`net hook --capture-bodies` 不再挂死页面**（JS-02）：响应体读取改为旁路，日志先落
  `body_capture_pending` 再补 preview；`text/event-stream` 与超过 4MB 的响应直接标
  `body_capture_skipped`。以前 `await clone.text()` 会让 SSE 页面的 `fetch` 永不 resolve。
- **响应改写不再整段缓冲、也不再丢字段**（JS-03）：只改 status/headers 时透传 `response.body` 流
  （日志标 `body_stream_passthrough`），并补回 `url` / `type` / `redirected`（补不上标 `url_lost`）；
  目标状态码是 204/304 时显式置空 body（`body_dropped:"null_body_status"`），
  不会因 `new Response(stream, {status:204})` 抛 `TypeError`。
- **mock 规则异常不再打断页面请求**（JS-12）：非法 status、204 带 body 时记 `mock_invalid`
  并放行真实请求，不再把 `TypeError` 抛给页面、也不再「页面 reject 而日志里查不到」。
- **XHR 的 abort/mock 现在符合原生状态机**（JS-08）：abort 置 `readyState=4` / `status=0` 并派发
  `ProgressEvent`；复用同一个 xhr 再发真实请求时，`open()` 会清掉上一次打的 `readyState` 补丁
  （只删自己打的，靠 `__abcNetOwnReady` 标记），不再让 `onreadystatechange` 永久被遮蔽。
- **规则匹配口径统一**（JS-09 / JS-16 / JS-17）：相对 URL 先解析成绝对 URL（`fetch('/api/x')`
  能命中 `match:"host/api/x"`）；默认按子串匹配、`regex:true` 才按正则（`app.js` 不再命中
  `appXjs`）；`net route` 更新同 (match, method) 规则改为**原位替换**，不改变命中优先级；
  重放超过 50 条时返回 `routes_dropped` 而不是静默截断。
- **`net fetch` 不再被自己的规则骗**（JS-21）：诊断请求改用 hook 之前的原始 `fetch`，
  返回体标 `bypassed_hooks:true`。以前它会命中自己写的 mock，还每次都进一条日志。
- **scan 的浮层广告剔除链路复活**（JS-04）：`simphtml_opt.js` 的 `nodeInfo` WeakMap 以前从未
  `set`，`analyzeNode` 入口守卫必然命中 → `R:covered` / `R:floatingAd` 一个都打不上，
  每次 scan 都白做分析还把广告层写进正文。现在固定定位的图片广告会被剔出结果。
- **超深 DOM 不再让 scan 崩掉**（JS-19）：页面侧克隆/分析加 512 层上限（与 `html.rs` 的
  `MAX_DOM_DEPTH` 同阈值），超限返回 `truncated: DOM deeper than 512 levels`；其它异常统一成
  `__ABC_SCAN_ERROR__<msg>` 由 daemon 转成结构化错误。
- **页面不再能劫持页面脚本入口**（JS-07）：扩展每次调用都重新注入页面脚本（不再「探测到同名
  函数就跳过」），并对 MAIN world 的 `__abcNet` / `__abcStealth` 校验不可枚举标记 `__abcOwn`；
  被页面抢占时明确报错，不执行页面提供的实现。
- **调试日志不再打到被访页面**（JS-18）：`simphtml_opt` 的 `console.log`（含遗留的
  `area / totalArea`）默认全关（`{debug:true}` 才输出）；文件末尾多余的 `optHTML()` 裸调用已删 ——
  它让每次注入都先对整页做一遍全量克隆。
- **`wait --js --interval` 生效**（JS-14）：daemon 传的键名由 `intervalMs` 改为 `minIntervalMs`。
  实测 `--timeout 11 --interval 5` 只巡检 3 次；以前被静默按 50ms 巡检（后台标签被节流到 1s 也有 11 次）。
- **`find_list` 不再改页面 DOM**（JS-13）：不再给容器写 `id='_ljq..'`，改用不落地 DOM 的
  nth-of-type 路径，避免破坏依赖 id 唯一性的页面逻辑与 `[id^=...]` 样式。
- **`--text` 定位不再给大页面强加整页布局**（JS-22）：元素数超过 20000 时降级 `textContent`，
  与 `page_wait.js` 的阈值一致。
- **非法选择器有明确错误**（JS-23）：`read --selector` 对非法选择器返回
  `invalid selector: ...`，与 `page_wait.js` 口径一致，不再表现为通道级 `pageOp failed`。

### Changed

- **`challenge status` 新增 `suspected`**（JS-11）：标题关键词分强/弱两档，`One more step` /
  `Access Denied` 这类通用文案只记 `suspected:true` + `signals:title-only-suspect`，
  需要 DOM/脚本实锤（`#challenge-stage` / orchestrate / `#sec-if-cpt-container`）才升级为拦截。
  daemon 侧 `open --challenge` 的标题预判同步去掉 `access denied`，不再让每个 403 页白付一次注入。
- **不再共用同名全局**（JS-15）：`page_action.js` / `page_find.js` 的可见性判断改用各自前缀
  （`__abcActVisible` / `__abcFindVisible`）。两份脚本注入同一个 world，同名函数会被后注入者覆盖，
  只改其中一份就变成静默的行为漂移。

## [0.6.0] - 2026-11-02

> 主题：命令改名 `atb`、反检测审计、挑战自动等待、扩展 5.0.0（ATB）。


### Changed

- **命令改名为 `atb`**：Cargo `[[bin]] name = "atb"`，clap `name` / `bin_name` 同步；npm `bin` 提供 `atb`（主）与 `atb2`（兼容）两个名字，指向同一份 `npm/bin/atb.js`（原 `atb.js` 改名）。
- **环境变量新前缀 `ATB_*`**：`ATB_AGENT`、`ATB_EXIT0`、`ATB_IDLE_TTL`、`ATB_NO_DEBUGGER`、`ATB_BIN`、`ATB_PACKAGE_DIR`；旧前缀 `AGENT_BROWSER_CLI_*` 继续生效，新名优先。
- **配置目录保持 `~/.atb`（有意不改）**：换目录会让升级前后的 daemon 各用一套配置与端口，同时跑起来更难排查。
- **CLI ↔ daemon 协议 4 → 5**：升级二进制后必须 `atb restart`，新版 CLI 会拒绝连旧 daemon 并给出修复步骤。扩展协议仍为 4。
- **扩展升级到 5.0.0（ATB）**：页面操作脚本默认在 ISOLATED world 执行（`read`/`find`/`snapshot`/`action`/`wait`/`challenge`），不再把 `window.__abcRead` 之类写到页面里；只有 `net` 与 `stealth` 留在 MAIN world。
- **页面角标默认关闭**（`chrome.storage.local.showBadge` 缺省 false），改为 closed Shadow DOM + 中性宿主 id `atb-status-host`；popup 里可开启，历史版本留下的 `#atb-ind` 会被清理。
- **网络钩子状态槽隐蔽化**：改为不可枚举的 `window['__atb_net_<随机>']`，包装后的 `fetch` / `XHR.open` 做 `toString()` 伪装与 `name` / `length` 对齐；`stealth scrub` 依赖 `__atb_` 前缀清理。

### Fixed

- **扩展 5.0.1：`ext_ready` 上报时 `groupTitles` 未定义**（`Uncaught (in promise) ReferenceError: groupTitles is not defined`）：
  该变量在 `socket.onopen` 里没有声明，异常发生在 `finally` 中，导致首批 `ext_ready` 根本发不出去 ——
  表现是「扩展显示已连接、`active_tabs` 却是 0」，要等下一次 `tabs_update` 才补上。现在按 `tabs_update` 的同一套字段取分组标题。
- **扩展 5.0.3：Service Worker 求值阶段的防御**：顶层 `removeLegacyCspRule()` 的 rejection 以前没人接，
  在部分 Edge 版本上会被记成启动失败（扩展页那条笼统的 `Service worker registration failed. Status code: 2`）；
  `chrome.runtime.getManifest()` 也加了兜底。这类失败一旦发生，浏览器不会自动重试，必须重载一次扩展。
- **部署改为原子替换**：新增 `scripts/deploy-extension.mjs`，逐文件「临时文件 + rename」再逐字节校验。
  之前的 `cp` 覆盖如果撞上 daemon 自动重载，浏览器会读到半截 `background.js`，直接注册失败并卡住。
- **`open --challenge auto` 卡住不再干等**：等待过程中到达「超时的 60%（夹在 3–12 秒）」就整页 reload 一次再继续等；
  检测到挑战页且是前台打开时，先把该标签切为 active（`allowFocus:false`，不抢系统焦点），避免后台标签的定时器被节流到分钟级。
  `challenge` 结果新增 `reloaded` 字段。
- **旧扩展下审计结果不可信**：扩展低于 5.0.0 时页面操作仍在写 MAIN world 全局，`stealth` 现在返回 `extension_ok:false` + `fixes`，明确要求重新加载扩展。
- **挑战页等待与 `open` 判定分叉**：`open --challenge` 复用 `challenge wait` 的同一套判定、超时语义与 `need_human` 行为，不再各写一份。
- **`atb2` 名字残留**：文档、帮助文本、示例统一为 `atb`；旧名只作为 npm 兼容别名保留。

### 作用范围

- `stealth` 读页面信号、清本工具自己写进去的东西（注入全局、角标、网络钩子包装）；UA / canvas / WebGL / 硬件参数保持浏览器原样。
- **验证码页面的处理方式**：CF 的非交互 JS 挑战会自己放行，工具做的是「不打断它 + 等」；8 秒未放行把标签页带到前台并返回 `need_human` + 控件坐标，由调用方或人接手。
- 验证方式：构建 + 单元/合约测试通过；扩展 5.0.0 重载后已在真实 Chrome 上复验（`stealth audit` 100/clean、
  `read` 后页面无 `__abcRead` 等全局、无角标节点、`net hook` 后 `fetch.toString()` 为 native、`scrub` 后无残留）。

## [0.5.0] - 2026-11-02

> 主题：多智能体隔离、三方版本配套、真·多窗口、非接触打开、扩展改名（ATB 4.0）。

### Changed

- **命令改名为 `atb2`**：二进制、帮助、文档、npm bin 全部更新；npm 仍保留
  `atb` 作为兼容别名，预编译子包在过渡期会同时尝试两个文件名。
- **扩展改名 `ATB`，版本 4.0.0**：manifest 名称/描述更新，避免与旧版混装。
- **`status` 增加 `compatibility` 块**：CLI 协议、daemon 协议、扩展协议三方比对，
  不配套时给 `upgrade_recommended`、`warning`、`next_steps`。
- **CLI 启动前校验 daemon 协议**：不再"尽力而为"地解析旧响应，直接给出重启/结束端口占用的两条修复路径。

### Added

- **多智能体隔离**：`--agent` / `AGENT_BROWSER_CLI_AGENT` / 终端标识三层身份；
  daemon 为每个身份维护独立默认标签页，`open` 只影响自己。
- **`cleanup` 命令**：按「本工具 open 过的会话」收尾，支持 `--dry-run` 与 `--all-agents`，
  不按 URL 匹配，不会误关用户标签页。
- **归属记录落盘**：`~/.atb/agent-tabs.json`，daemon 空闲退出后重启，
  `cleanup` 仍能清到之前打开的标签页（已关闭的自动剔除）。
- **多窗口**：`TabInfo` 上报 `window_id`，`tabs --ids` 可见，`--tab window:<id>` 按窗口批量操作。
- **`extension_outdated` 错误**：依赖新字段的选择器（window/group/current）在旧扩展上
  直接报错并附三步修复，不再表现为"找不到标签页"。
- **非接触截图**：可见区域截图走 `chrome.tabs.captureVisibleTab`，不附加调试器
  （`screenshot --full-page/--selector` 仍需 CDP）。
- **`AGENT_BROWSER_CLI_NO_DEBUGGER=1`**：彻底禁止 attach，CDP 命令返回替代方案。
- **扩展启动错误记录**：Service Worker 启动异常写入 `chrome.storage.local.lastServiceWorkerError`，
  便于排查 Edge 的 "Service worker registration failed"。
- **文档**：`docs/BEST_PRACTICES.md`、`docs/EDGE_TROUBLESHOOTING.md`，SKILL 重写。

### Fixed

- **多窗口 / 多 profile 下 `tabs --ids` 缺窗口信息**：扩展 `tabs` 响应统一为 `window_id`，
  daemon 兼容旧的 `windowId`。
- **`--tab group:` 在扩展未上报 group 时静默返回空**：改为明确的能力检查错误。
- **兼容性报告按会话重复列出同一浏览器**：按 browser+version+protocol 去重。

### Security / 合规

- 明确**不做**验证码自动破解与指纹伪装；`challenge` 保持"识别 / 等待站点放行 / 转人工"。

## [0.4.0] - 2026-11-02

> 主题：多标签并发、命令预检、报错可执行。目标是一条命令打多个标签、智能体一次写对、失败能自解释。

### Added

- **`--tab` 多目标语法**：`--tab 111,222`（并发）、`all`、`@1,@3` / `@1-5`（按 `tabs` 顺序）、`url:<子串>`、`title:<子串>`、`group:<标签组>`、`current`。单 id 行为不变，向后兼容。
- **`/multi` 并发分发**：一次请求完成「解析目标 → 并发执行（默认最多 8 路）→ 按输入顺序汇总」。结果逐条带 `index/tab_id/title/url/ok/elapsed_ms`，失败项额外带 `error_code` 与 `hint`，单条失败不影响其它标签页。
- **`open` 支持一次多目标**：`open url1 url2 url3` 由 daemon 内部并发打开，只占一个 HTTP 往返；返回逐条 `tab_id / session_key / ready`，失败逐条说明。
- **`--dry-run`**：只做参数预检与目标解析，输出计划（含解析后的 tab 列表），不触碰页面。
- **命令预检（本地）**：CSS/XPath 括号与引号配平、JS 字符串/注释/括号配平、`--tab` 语法、URL 与本地文件、数值范围、输出路径冲突、`--async` 组合限制。错误带 `error_code` + `hint` + `example`。
- **退出码 2**：参数/预检错误独立成码，脚本可区分「改命令」与「重试」。
- **`tabs --ids`**：只回 `tab_id/title/url/active/group`，选目标时不必在长 JSON 里翻。
- **`close --tab 111,222`**：支持批量关闭；拒绝 `all`，避免误伤用户标签页。
- **截图/PDF 输出占位符**：多目标时 `--out` 支持 `{tab}` / `{index}`；缺占位符会在预检阶段拒绝，避免互相覆盖。
- **`--tab group:`**：依赖扩展 3.5 上报的标签组标题；扩展未上报时给出明确提示而非静默失败。

### Changed

- **`open` 响应精简**：移除与顶层字段重复的 `metadata.raw`，一次开 6 个标签的体积下降约 60%。
- **`tabs` 保证新鲜**：命令执行前主动向扩展拉一次列表（1.5s 超时、失败回落快照），不再依赖可能被 Service Worker 节流的推送。
- **`tabs` 顺序稳定**：会话统一按 browser → profile → tab id 排序，`@N` 不再随 HashMap 迭代顺序漂移。
- **目标不存在的报错**：直接列出「当前可用」的 tab id + 标题，并提示 `tabs --ids` / `--tab @1`，不再只有一句「会话ID xxx 未连接」。
- **`--async` 与多目标**：明确拒绝并说明替代写法（一条 job 只对应一个目标）。
- **动作类 `--async` 修复**：`hover/select/check/uncheck/scroll/focus` 之前提交会被判 unsupported op，现已加入支持列表。

### Performance

- **打开多个标签**：6 个标签从 ~3.4s 降到 ~0.6s（单请求 + 并发 + 打开后只刷新一次标签列表）。
- **单标签就绪**：不再盲等 3 秒；`open` 后主动拉取标签列表，通常 50-200ms 内 `ready:true`。
- **多标签操作**：6 个标签并发 `exec` 从 ~1.0s（6 个进程串行）降到 ~0.2-0.3s（1 个进程 + 并发）。

### Fixed

- **多个 daemon 可同时监听 18767**：daemon 启动失败时的清理路径与端口竞争导致偶发「命令打到没有扩展连接的实例」。现在标签快照与刷新统一走同一份会话表，刷新失败会退到更老的已就绪会话重试。
- **`exec`/`read` 在刚打开的标签上偶发失败**：新标签的 content script 尚未注入时，刷新命令发不进去；现在自动退到其它已就绪会话。
- **`open` 的 `ready` 误报**：并发打开时最后创建的标签可能晚一拍才出现在 `chrome.tabs.query` 里，现在会重试补齐。

## [Unreleased]

> 2026-11-01 修复批次：退出码契约统一、`--monitor` 输出结构化、若干长跑/持久化缺陷。下方「未发布」为同批次更早的累积条目，两者都尚未发版。

### Fixed

- **`net hook --persist` 刷新后 mock 规则凭空消失**：持久化时把页面返回的「规则条数」当成规则数组写进配置，刷新重装时因类型不符被页面侧丢弃，表现为「规则只在当前页面有效，一刷新就没了」。现在先取规则列表再兜底过滤，任意操作顺序（先 hook 后 mock、先 mock 后 hook）刷新都不再丢规则（BUG-08）。
- **`net routes` 会误清正在生效的 CDP 拦截**：`net routes` 本意是查询，实际把整份配置当 CDP 规则数组下发，把 `--engine cdp` 的规则整批覆盖掉。现在查询不再有副作用（N-01）。
- **`challenge` 的「N 秒后转人工」永不触发**：`focusAfterMs` 每片都从 0 重新计时，而单片等待上限只有 4 秒，等多久都到不了阈值，需要人点的验证码页一路静默到超时。现在按总经过时间累计，到点自动把标签页带到前台（BUG-05/BUG-06 同族）。
- **`exec --monitor` 吞掉页面脚本异常**：页面里抛错时命令仍返回 `ok:true`、退出码 0，调用方以为执行成功。现在照常上抛失败（BUG-04）。
- **CSP 误判导致提交按钮被点两次**：`is_csp_error` 用裸子串 `csp` 匹配，`#csp-banner not found` 这类普通报错也被当成 CSP 拦截，于是换通道重试一次，点击被重复执行。现在只认真正的 CSP 错误特征（BUG-11）。
- **扩展发送失败后命令空等**：WebSocket 发送失败时没有回收在途请求，命令只能等到超时。现在发送失败立即收口并返回错误（BUG-10）。
- **长跑 `exec`/`scan` 期间 daemon 自己退出**：空闲判定只数后台 job 数量，前台长命令不算在内，daemon 误判空闲后退出，命令被腰斩。现在在途命令同样计入（BUG-02）。
- **`close` 失败会丢会话**：以前拿到扩展确认前就删会话，关闭失败时既丢了会话状态，又让 `default_session_key` 指向已删除的会话。现在确认后才清理（BUG-20）。
- **会话级缓存只增不减**：`snapshots` / `page_scale` / `session_locks` 不随会话销毁，长跑 daemon 的内存单调上涨。现在随会话一并回收（BUG-15）。
- **`tabs` 的 `connected_at` 恒为 null**：现在按扩展上报的连接时间填写（BUG-24）。
- **迟到/重复 `ack` 永久驻留**：ack 集合只增不减，长跑 daemon 缓慢泄漏。现在超出等待窗口的 ack 直接丢弃（BUG-26）。
- **`read`/`scan` 的 `--max-nodes` 被悄悄抬高**：给小于 1000 的值时被静默改成 60000，用户想省 token 反而拿到上限。现在按传入值生效。
- **`--max-chars` 被静默收敛**：超出范围时不再无声改写（部分路径仍会收到服务端上限，但不会再出现「给了一个值、拿到另一个值却毫无提示」）。
- **`job cancel` 对 `wait` 现在真能停**：`wait --js` 以前把剩余时间整段注入页面，取消要等整段返回才被看见（30s 的等待取消后仍然跑满 30s）。现在可取消的任务按 1s 切片，取消约 1s 内生效，并如实标为 `cancelled`；同步调用不受影响（仍是一次注入）。
- **内部提示词泄漏到对外文案**：`chat`/`monitor` 相关错误里混入了只该给模型看的内部指令，对外部用户无意义，已中性化。

### Changed

- **退出码契约统一（会 break 脚本）**：`ok:false` 一律非 0，设置 `AGENT_BROWSER_CLI_EXIT0=1` 可恢复旧的「恒 0」行为。具体落地：
  - `status` 的 `ok` 现在跟随 `healthy`，daemon/扩展不健康时退出码非 0；
  - `job wait`：`failed` → 非 0，`cancelled` → 130（沿用 shell 的 SIGINT 惯例），等待超时 → 3；
  - `download-wait` 超时 → 3；
  - `challenge wait` / `challenge solve` 超时 → 非 0，并带 `error_code: challenge_timeout`（此前是 `ok:false` + 退出码 0）。
- **`challenge --timeout` 改为可选**：不传时由服务端按 action 取默认（`wait` 30s、`solve` 120s）。此前 CLI 恒定发 30s，`solve` 的 120s 永远用不到，需要人操作的验证码必被提前判超时。
- **`--monitor` 输出结构化（会 break 读 `top_change` 的代码）**：`click`/`fill`/`mouse-click`/`send-keys --monitor` 与 `exec --js` 的监控输出统一，`change` 变成：

  ```json
  { "changed": 2, "added": 1, "removed": 0, "attributes": 0, "text": 1,
    "samples": [{ "type": "childList", "target": "div#app", "detail": "+span" }] }
  ```

  - `changed` 从 `0|1` 改为「变更记录条数」（仍可用 `changed > 0` 判有无变化）；
  - 新增 `added` / `removed` / `attributes` / `text` 与 `samples`（最多 8 条）；
  - **移除 `top_change`**：旧实现把变更前页面前 2000 字符原始 HTML 塞进返回体，白烧上下文预算；
  - 监控期间发生导航时返回 `{"changed":1,"navigated":true,...零值键,"samples":[]}`；
  - 拿不到计数时才出现 `monitor` 字段，`monitor.reason ∈ {unsupported, navigated, superseded, injection_failed}`，此时**动作本身已成功执行**。
- **`net unroute` 支持 `--engine page|cdp`**：此前无法单独删除 CDP 规则，只能 `net reset` 全清。
- **CDP 引擎的放行默认不写日志**：此前每条放行都记一条，200 条环形缓冲几秒被挤满，`net log` 里看不到真正被拦的请求。
- **`broadcast` 参数校验收紧**：`--args` 不是对象时直接报错，不再静默丢参后照常广播。

### Added

- **`trust-extension`**：一条命令维护扩展白名单与握手 token（`<id>` / `--remove` / `--list` / `--clear` / `--token` / `--clear-token`），写入后 daemon 每次握手重新读取，无需重启。可选的加固手段。
- **`scripts/sync-extension-assets.mjs`**：新增 `npm run sync:extension` / `npm run check:extension`，`ops/` 与 `assets/` 的同步由脚本完成并有 CI 守卫。
- **`net status.patterns`**：标注每条规则推导出的 `urlPattern` 及其来源，带 `fallback` 标记，规则没生效时能直接看出是不是 pattern 推导的问题。

### Security

- **WebSocket 身份加固（默认不启用）**：`<用户配置目录>/ws-trust.json` 支持 `extension_ids`（白名单）与 `token`（扩展握手时带 `?token=`）。两者都为空时保持「装扩展即用」的兼容行为。
- **扩展侧支持 `chrome.storage.local.wsToken`**：配置后自动带 token 连接；URL 日志里的 token 脱敏为 `token=***`。

### 升级注意

- 删掉读 `change.top_change` 的代码；需要预览请自己截取页面内容。
- `change.changed` 当布尔用可以保持不变（`0` 即未变化），但它现在是计数而非 `0|1`。
- 区分「导航」用 `change.navigated === true`，不要靠 `samples` 为空来推断。
- 脚本里若判断 `$?`，请复核 `status` / `challenge wait|solve` / `job wait` / `download-wait` 的退出码变化；需要旧行为时设 `AGENT_BROWSER_CLI_EXIT0=1`。
- `trust-extension` 与 `ws-trust.json` 都是可选的，不配置不影响任何现有用法。
- 改过 `assets/page_*.js` 后必须 `npm run sync:extension`，否则 `ops/` 里的副本与扩展不同源（`npm run check:extension` 可在提交前自查）。

### 第三轮验收补充（同日，v3 复核后）

#### Fixed

- **响应改写会把响应体吞成空串**：规则只改 `response.status` 时，若原响应体读不出来（已消费 / 不透明响应），页面会收到「200 + 空 body」，日志却写着改写成功。现在放行原响应，并在日志标 `response_modified: false` + `response_rewrite_skipped`（fetch 与 XHR 两条路径都改）。
- **`net` 规则活不过 daemon 重启**：配置只在内存里，重启后 `net routes` 读空，而页面里的 hook 还在（persist 注入挂在扩展侧），配置与实际不一致。现在落盘到 `<用户配置目录>/net-hooks.json`，启动时读回，`net reset` 会一并删除该 profile 的持久化条目。
- **`challenge` 内外两层 `ok` 相反**：超时返回 `{"ok":true,"result":{"ok":false,...}}`，直接调 HTTP API 的集成方会看到成功。现在外层 `ok` 与业务结果一致。
- **`inflight` 没有上限也没有 TTL**：某个 handler 永久挂住时（扩展失联 + 长 `--timeout`），daemon 再也不会空闲退出。现在超过 2× 空闲 TTL（不少于 30 分钟）视为卡死放行，并打日志说明已挂时长。
- **持久注入的 `matches` 损坏时全站注入**：`patterns` 存在但不是非空数组时，以前回落成 `<all_urls>`；现在不注入并 `console.warn`（fail-closed）。`background.js` 与 `persist_boot.js` 行为一致。

#### Changed

- **`net log` 支持 `--engine page|cdp|both`**：以前只能合并双源，想看 CDP 侧单独一条得靠翻全量。
- **`net log --debug-pass`**：文档里承诺过但代码里根本不存在（`NetRequest` 没这个字段），CDP 放行日志永久为关。现在打通到扩展的 `debugPass`，`net status` 可验证。

#### Added

- **`POST /reload-trust`**：重读信任配置并对已建立连接复核，不可信的连接在下一次轮询（≤500ms）内收到 Close 并被断开。

#### Security

- **`ws-trust.json` 解析失败不再降级为「任意扩展可连」**：以前写坏一个尾逗号，整套白名单与 token 校验立刻失效，而 daemon 在后台看不见 stderr。现在配置损坏即拒绝所有握手（503），`/health` 的 `ws_security.trust_config_error` 给出具体解析错误——与「从没配过」可以区分了。
- **信任变更即时生效**：`trust-extension` 之后调用 `/reload-trust`，此前已连上的不可信扩展会被真正踢掉，不用等它自己断开或重启 daemon。

### 第四轮验收补充（同日，v4 闭环）

#### Fixed

- **只改 token 踢不掉已连接的扩展**：`/reload-trust` 原先只看白名单，token-only 配置（白名单为空 + 有 token）时**恒不撤销**——用户以为"加了 token 就锁死了"，但那条可疑连接还活着。现在握手时把 token 指纹随连接登记，改 token 即按 `token_mismatch` 断开（真机实测：换 token 后连接 2.5 秒内被关）。回包的 `revoked[]` 新增 `reason`（`untrusted_id` / `token_mismatch` / `corrupt_config`）。
- **`trust-extension` 把所有 reload 失败都说成"未连接到 daemon"**：daemon 在跑但 token 不匹配（401）、配置损坏（503）时，用户会得到"未连接"这个错误归因，被引向错误方向。现在区分四种结果，失败时原样带出原因，并给出机器可读的 `reload_applied` / `reload_error`。
- **空的 `ws-trust.json` 被当成"未配置"**：手工 `> ws-trust.json` 或编辑器保存中断会静默回到"任意扩展可连"。现在"存在但为空"按损坏处理（daemon 拒绝握手），CLI `--list` 也报同一处错误；写入类命令（`<id>` / `--token` / `--clear`）仍可直接覆盖，不至于把自己锁死。

#### Changed

- **`cargo clippy` 从"只告警"变成"卡 CI"**：补上 `-- -D warnings`，并把仓库里遗留的 8 条告警清零（多余 cast、可折叠 `if`、`format!` 嵌套、`clamp` 写法、测试里的 `get().is_none()` 等）。同时全量跑了 `cargo fmt`——此前的 `cargo fmt --check` 步骤对每次推送都是红的。

### 全量排查修复（2026-11-02，首次覆盖扩展与构建链）

> 这一轮把排查范围从 `src/` 扩到 Chrome 扩展、`scripts/`、`npm/` 与 CI，共处理 62 条中的 55 条（余下见文末「已知未修」）。

#### Fixed（严重）

- **扩展 service worker 被回收后页面请求永久挂死**：`debugSessions` 只在内存，SW 被杀即空；但 CDP 侧的请求已暂停。新 SW 被 `Fetch.requestPaused` 唤醒时找不到 session 就直接 `return`，**没有任何 continue/fulfill/fail**，页面一直转圈。现在：该分支按阶段强制放行；并把每标签的规则落 `chrome.storage.session`，冷启动时对已 attach 的标签重建会话并重新 `Fetch.enable`。`ext_ready` 新增 `restartSeq`，daemon 能识别扩展重启。
- **`--frame` 在 CSP 回退路径上系统性丢失**（5 处）：`net`/`read`/`find`/`challenge`/`wait` 的回退调用全部硬编码 `frame_id=None`，CSP 页面上带 `--frame` 会**静默落到主框架**——读到错误框架的内容且无任何提示；`net route` 更会把规则装到主框架。这是 v1 BUG-09 的同类项，当时只修了正常路径。
- **`net` 字符串 rule 解析失败后校验被绕过**：解析得 `null` 仍写入 `opts["rule"]`，而校验用 `is_none()`——键存在即通过，于是错误归因成「需要 rule」（用户明明传了）。
- **`net` 的 host 解析会把持久化注入注册到所有网站**：`https://x.com?q=1` 的 host 被切成 `x.com?q=1`（pattern 非法），且 `starts_with("chrom")` 误杀 `chromium.org`，最终兜底成 `<all_urls>`——与「收窄注入范围」的目标相反。现在按 scheme 判定，且 merged 为空时**不注册**并告警，不再兜底全站。
- **持久化注入名未按 profile 隔离**：扩展侧 `persistInjections` 的键就是 name（全局），profile B 的 `net hook --persist` 会覆盖 A 的条目，A 的 `net reset` 又删掉共享条目 → daemon 配置、磁盘、扩展三方失同步。注入名改为带 profile 摘要，并 best-effort 清理旧名 `__net_hook`。
- **`ws-trust.json` 写入非原子**：Windows 分支是先删后改名，中途失败/断电则文件消失 → daemon 把 `NotFound` 判为「从没配过」→ **回到任意扩展可连**（v3 修的是「解析失败」，这是同一 fail-open 的另一个入口）。现在复用 `config::replace_file_atomically`（backup → rename → 失败回滚），读取侧也走 `read_with_backup`。
- **popup 一打开就把当前页全部 cookie 静默写进系统剪贴板**（含 httpOnly 会话 cookie）。改为显式「复制 cookies」按钮，默认排除 httpOnly/secure，复制后给出提示。

#### Fixed（假成功族）

- 页面动作的无 eval 回退恢复路径漏调 `ensure_page_action_ok`：动作报 `ok:false` 却返回 `status:"success"`（同函数另外 4 条路径都有检查）。
- `send-keys --target`（无 `--frame`）的 focus 失败被吞，按键照发并返回成功——实际打到了当时聚焦的元素。
- `downloads wait` 的超时/中断只体现在内层 `ok`，外层仍是 `ok:true`（CLI 无影响，HTTP 直连方会误判）。
- `net reset` 的页面调用失败被 `.unwrap_or_else` 伪造成 `ok:true`，错误信息完全丢失。
- `smartProcessResult` 序列化失败返回字符串 `'[无法序列化: ...]'` 且外层 `ok:true` → 改为 `ok:false`。
- 扩展收到「有 id 但 code 无效」的消息时**不回任何响应** → daemon 空等到超时；现在回同样的 error 形状。

#### Fixed（其他）

- 扩展 `connectWS` 的 `await loadWsPort()` 在 try/catch 之外，storage 被写坏后**永久断连且不自愈**；`onopen` 的 async 无 try/catch，任一 API 失败则 `ext_ready` 永不发出（daemon 已 accept 却等不到就绪）。
- `Fetch.enable` 抛错后状态停留在「`enabled:false` 但 `rules:N`」，原有拦截静默全失效。
- keepalive 把 `CONNECTING` 当成断开 → 丢弃仍在连接的 socket，之后「实际连着却发不出响应」。
- `scheduleTabsUpdate` 用内存 `setTimeout` 去抖，SW 被杀则该次更新永久丢失。
- `handleCookies` 对非 http(s) 页面抛 `TypeError`；`handleNetworkDetail` 在未 attach 时不区分「未采集」与「取不到」。
- `--monitor` 的 MutationObserver 在动作提前返回时泄漏（错误路径不回收）。
- 组合键中途失败时修饰键永不抬起（此后该标签所有操作都等同按住 Ctrl）；现在无论成败都释放。
- job 淘汰按 HashMap 无序删除，可能删掉刚完成的结果，且全 Running 时无上界；`job cancel` 在完成窗口内会覆盖终态、丢弃已产生的结果。
- `profile-label` 查重与写入之间的竞态（两个 profile 可挂同一 label）；`session_locks` 会为已消亡会话新建条目。
- `--browser/--profile` 选会话回退 `matches.first()`，结果依赖 HashMap 顺序（同一条命令重启前后可能打到不同标签）。
- `screenshot`/`save-pdf` 不校验产物：0 字节或损坏的文件仍返回成功、退出码 0。现在校验非空 + 魔数 + 最小长度。
- `extension_control` 硬编码 30s，忽略调用方 timeout —— `scan --timeout 300` 在 CSP 回退通道上被截断到 30s。
- `offset + limit` 无上限校验（debug 构建可 panic，release 回绕导致分页错乱）。
- 5 处 `tab_id` 解析失败静默用 0，用户看到的是「tabId is required」而非真实原因。
- `page_element_count` 的缓存可能存成「新 URL → 旧计数」（导航竞态）。
- close 失败无条件标 `disconnected`，网络抖动时用户看到「标签未连接」但浏览器里明明开着。
- `ref_relocated` 字段类型不一致（一处字符串、其余布尔）。

#### Changed（会 break 的地方）

- **`downloads erase` 不带范围现在是错误**：需要显式 `confirmAll:true` 才允许清空全部下载历史。此前 `erase` 无参会 `chrome.downloads.erase({})` 清空所有记录。
- **`ref_relocated` 统一为布尔**，新选择器改由 `ref_selector` 返回（此前那条路径把选择器写进去了）。
- **`net hook --engine cdp` 直接报错**（该 action 只支持页面引擎），不再「走页面注入却回报 cdp」。
- **`net` 的持久化注入名带 profile 摘要**，`__net_hook` 不再使用（旧条目会被 best-effort 清理）。
- **`--header` 缺冒号、`--data` 以 `{`/`[` 开头但非法 JSON，现在都直接报错**，不再静默丢头/把坏 JSON 当字符串发出去。

#### Added

- 47 个子命令补齐 `--help` 说明（此前 49 个里只有 2 个有），并区分了 `scan`/`read`/`snapshot` 这类易混命令。
- `manifest.json` 增加 `minimum_chrome_version`（依赖 `world:'MAIN'`、`storage.session`、alarms 的 30s 下限）。
- 扩展冷启动恢复：`chrome.storage.session` 保存每标签的拦截规则，SW 重启后重建；`ext_ready.restartSeq` 供 daemon 识别重启。
- CI 增加 `npm ci --dry-run` 与 lock 版本一致性校验。

#### Fixed（构建链）

- **`npm run sync-version` 在 Windows 上必炸**：`new URL(...).pathname` 缺 `fileURLToPath`，路径拼成 `D:\D:\...`。此前只在 ubuntu 上跑，所以长期潜伏。
- **`npm ci` 直接失败**：lock 顶层版本停在 0.3.6、5 个平台包条目没有 `version` 字段。现在 lock 与 `package.json` 同步生成并由脚本校验。
- **win32-arm64 三处一致缺失**（bin 映射 / optionalDependencies / publish matrix）：现在明确返回可操作错误（说明本平台无预编译包、可用 `cargo build --release` 自编并设置 `AGENT_BROWSER_CLI_BIN`），README 平台表同步。

#### Security

- 澄清了「加固是单向的」：`trust-extension` 只约束「谁能连 daemon」，不约束「扩展连的是谁」。README 与 popup 都如实说明：daemon 空闲退出后端口可能被本机其它进程抢占，配置 token 防不住这一点（双向握手需要协议升级，暂未做）。

#### 已知未修（第五轮）

- 扩展 `alarms` 最小 30s 与 SW 30s 回收的根本矛盾：正解是用 offscreen document 持有 WebSocket，改动面大，本批未做（已记录为下一步）。
- `job wait` / `wait` 的内层 `ok` 是否统一（与 B-02 同族）：会改变「匹配超时算不算失败」的语义，需产品决策。
- `ext_ready` 无条件信任扩展上报的 browser/profile（兼容模式下任意扩展可冒充）：与 AUTH-01 同源，待双向握手方案落地后一并解决。

## 未发布

### 新增

- 新增 `cookies`：读取当前页面/指定 URL 的 Cookie（不再依赖 `exec '{"cmd":"cookies"}'` 逃生口）。
- 新增 `upload <selector|@e> <files...>`：用 CDP `DOM.setFileInputFiles` 设置文件输入框，支持多文件。
- 新增 `run --file steps.json`：一次进程执行多步操作（默认遇错即停，`--continue-on-error` 可继续），返回逐步结果与耗时。
- 新增 `tabs --full`：输出完整 URL（默认截断到 80 字符）；`exec --file -` 支持从 stdin 读脚本。
- 新增全局 `--pretty`：默认改为单行 compact JSON，节省 token。
- 后台运行：`open` 默认前台打开（切到新标签页），`--background` 才后台创建；两者都会让新标签成为后续命令的默认目标，返回新增 `result.ready`。
- 异步任务：`--async` 可提交 `exec/scan/snapshot/click/mouse-click/fill/send-keys/upload/screenshot/save-pdf`，立即返回 `job_id`；新增 `job list|get|wait|cancel`，任务状态为 `running|succeeded|failed|cancelled`。
- `/health` 新增 `jobs: {running,total}`；daemon 在任务运行期间不会因空闲退出。
- 受控标签页设置 `autoDiscardable:false`，避免被 Chrome 内存节省机制丢弃。
- 新增 `tests/mock_extension.mjs`：不依赖 Chrome 的 WebSocket 假扩展 + 端到端自测。
- 新增页面读取与查找：`read`（标题/正文/元信息/表单/计数，支持 `--selector`/`--max-chars`/`--links`）、`find`（CSS / 可见文本 / XPath，返回可操作信息与简短 CSS 路径）。
- 新增页面动作：`hover`、`select`、`check`、`scroll`、`wait`（`--selector`/`--text`/`--js`）、`back`、`reload`，全部为单次注入往返。
- 新增并发批量：`POST /batch` + `batch <steps.json> --concurrency N`，一次请求内最多 N 步并发；与串行 `run` 并存，分别对应「无依赖批量」和「有依赖流程」。
- `open` 支持一次给多个 URL，客户端并发打开并返回 `elapsed_ms`。
- `read` / `find` / `hover` / `select` / `check` / `scroll` / `wait` 均支持 `--async`。
- `exec --monitor` 改为页面内 MutationObserver：脚本执行期间直接累计 added/removed/attributes/text 与样本，不再「执行前后各抓一次整页 HTML 再 diff」（大页面上省掉两次全量序列化）。点击/填写仍走旧路径。
- 新增 `browsers`：按 `browser_id + profile` 列出所有已连接目标与代表标签。
- 新增 `broadcast <op>`：一次请求在所有匹配的 browser/profile 上**并发**执行同一操作（`exec/read/find/scan/net/downloads/...`），逐目标返回结果与耗时。
- 新增下载监听：`downloads`（列表）、`download <url>`（开始）、`download-wait --id`（等待完成）；扩展启用 `downloads` 权限，wait 在 daemon 侧轮询。
- 新增页面网络：`net fetch`（页面上下文发请求，带 cookie）、`net hook`（包装 fetch/XHR 记录）、`net log`、`net mock`（按片段/正则返回伪造响应或 abort）、`net routes`、`net unroute`、`net reset`。
- `read` 新增 `--include-hidden`（textContent 语义，包含隐藏内容）、`--budget-ms`、`--max-nodes`、`--frame`；返回新增 `text_mode` / `partial` / `stats{node_count,scanned,visited,elapsed_ms}`。
- `scan` 新增 `--fast`（一次注入的紧凑文本抽取）与 `--max-nodes`；页面元素数 > 6 万时自动改走文本，并在内容头部注明原因。
- `click` / `fill` / `send-keys` 新增 `--frame`：指定 iframe 时强制走页面内动作（CDP 的 `DOM.resolveNode` 只对主框架有效）。
- `read` / `find` / `action` / `wait` 全部支持 `--frame`。
- 新增 `tests/gen_huge.mjs`：生成 26 万元素的超大页面用于性能验收；新增 `tests/fixtures/read_hidden.html`、`read_hidden_2.html`（隐藏文本 + 跨页重定位）。
- 新增**无 eval 通道**：严格 CSP 页面（GitHub/Google）上，`read`/`find`/`snapshot --js`/`hover`/`select`/`check`/`scroll`/`click`/`fill`/`wait` 自动改走「扩展注入 `ops/page_*.js` + 调用全局函数」，结果带 `via:"no-eval"`。
- `net hook --persist`：把「页面网络库 + 当前规则」注册成 document_start 持久注入，刷新/跳转后自动重装；`net reset` 会移除。
- `net mock` 支持请求与响应改写：`--request-header` / `--request-body`（放行真实请求但改请求）、`--response-status` / `--response-body`（放行后改响应）。
- `wait --selector` / `wait --text` 改为页面内 MutationObserver 一次注入完成（`mode:"observer"`），不再每 100ms 注入一次。
- `wait --js` 同样改为一次注入：谓词在页面内编译一次，DOM 变化由 MutationObserver 触发，非 DOM 条件由定时器兜底；返回 `mode:"js-observer"` / `injections`（注入次数，正常为 1）/ `checks`（页面内求值次数）。
- `exec --wait-js`、`click/fill --wait-js` 复用同一实现，输出形状仍是 `js_return: { result, wait }`，`wait` 里多了 `injections` / `checks` / `mode`。
- 新增 `tests/live_xhr_wait.mjs`：真实浏览器验收 XHR 改写可靠性（监听器先注册、mock/abort/请求改写/fetch 对齐/刷新持久化）与 `wait` 单次注入（含跨导航、CSP 页、等待期间不阻塞同标签）。
- `tests/mock_extension.mjs` 支持 `ABC_CLI=target/release/atb.exe` 指定被测二进制，并新增「wait 只注入一次」断言。
- `net log` 的 XHR 条目新增 `response_modified` / `response_rewrite_skipped` / `rewritten_status`，可直接确认改写是否生效。
- 新增 `net mock --engine cdp`：走扩展的 `chrome.debugger` + `Fetch` 域做**网络层**拦截，能改 `responseType=blob` 的原始二进制体，也能拦 `<img>` / `<script>` / JSONP 这类不经过 fetch/XHR 的请求；不换 body 时用 `Fetch.continueResponse` 原地放行（避免压缩响应被重新编码）。
- `net` 规则改为**按 `browser:profile` 共享**：同一 profile 下任意标签 `net routes` / `net mock` / `net reset` 都作用于同一套规则，`--persist` 注册的规则在新标签加载时自动生效。
- `responseType=blob` 在页面引擎下支持改写：规则显式给 `--response-body` 时构造新 Blob 并沿用原 `content-type`；只改状态/头时如实记 `blob-without-body`。
- 新增 `tests/extension_net_intercept.test.mjs`：用 `node:vm` 把真实 `background.js` 跑起来，假 `chrome.debugger` 记录 CDP 调用，覆盖 patterns 阶段选择、UTF-8 base64、abort、请求头合并、continueResponse/fulfillRequest 两条路径、stats 与 stop。
- 新增 `tests/live_cdp_net.mjs`：干净 profile 真机验收网络层拦截（页面引擎拦不到的 `<script>` / `<img>` 被 cdp 引擎改掉；blob 只改状态、二进制改 body；reset 全恢复），共 15 条断言。
- `net routes` 响应新增 `cdp: {enabled, count, rules, applied}` 与 `profile` 字段；`net` 命令响应新增 `engine` / `profile`。
- 新增 `challenge`（挑战页/验证码）：`status` 识别 Cloudflare / Turnstile / reCAPTCHA / hCaptcha / PerimeterX / Akamai / DataDome / AWS WAF 与通用验证码，返回 `{detected, vendor, kind, blocking, interactive, auto, pending, frames, signals, hints}`；`wait` 等自动放行（页面内 MutationObserver，daemon 按 4 秒一片重注入，换页后自动重试）；`focus` 把标签页带到前台交给人工（`--no-focus` 不动系统焦点）。**不提供自动破解**。
- `read` 在标题/正文命中挑战特征时附带 `challenge` 字段，智能体能立刻区分「CF 拦截页」和「页面没内容」。
- 新增 `challenge solve`：一条命令完成「自动放行 / 等验证码 token / 转人工」。拦截页等它自动通过；Turnstile / reCAPTCHA / hCaptcha 等 `*[name$="-response"]` 隐藏字段写入 token 即判定完成（隐形挑战会自动写，需要人点时聚焦后等人点完），返回 `{passed, via:"token"|"clear"|"no-challenge", focused, elapsed_ms}`；超时返回 `passed:false, reason:"timeout"`。**仍不提供自动破解**。
- `net log` 合并 CDP 侧记录：条目带 `source:"page"|"cdp"`、按时间排序、统一过滤与截断，返回 `sources:{page,cdp}`；`--clear` 同时清空两边；换过 body 的条目带 `preview`（前 160 字符）。
- 新增 `tests/live_challenge.mjs`（22 条真机断言：不误报、CF 识别、read 带出 challenge、自动放行、超时、focus、reCAPTCHA 组件页、阿里云/极验滑块）。
- `net hook --persist` 的 `matches` 默认收窄到当前站点（`*://host/*`），不再一律 `<all_urls>`；已有配置取并集，`net routes` 可见。
- 修复持久注入的匹配器：`persist_boot.js` 之前把 match pattern 当通配转正则，带端口的 URL（`http://127.0.0.1:55086/`）匹配不上，表现为「刷新后规则消失」。现在按 scheme/host/path 三段匹配，host 只比 hostname（忽略端口）。
- 同一标签的命令改为**每会话串行**（一把异步锁），不同标签仍并发；同标签 50+ 并发不会互相踩踏。


- `snapshot` 支持 `--interactive-only`（只保留可操作元素）与 `--max-nodes`（截断大树），AX 与 DOM 两种模式都生效。
- 持久注入改为 **document_start** 生效：动态注册 `persist_boot.js`（ISOLATED, runAt=document_start），命中后立即在 MAIN world 执行；注册失败时退回导航后重放，两条路径互斥，不会重复 hook。扩展升到 **2.5**。
- 新增 `inject`：把 JS 注入拆成独立命令，支持 `--world main|isolated`、`--frame`、`--all-frames`、`--file`；语义与 `exec` 一致（`return` / 裸表达式 / `await` 都可以）。
- 新增持久注入：`inject --persist <name> [--match <pattern>]`，页面每次导航后自动重放；`--list` / `--remove` 管理。
- 新增 `frames`：列出标签页内所有 frame（frame_id / parent / url），配合 `exec --frame` 使用。
- `exec` 支持 `--world isolated`（不受页面 CSP 限制，但看不到页面变量）与 `--frame` / `--all-frames`。
- 新增 `snapshot --js`：DOM 版快照（不经 CDP），产出带 CSS 选择器的 `@e`；CDP 不可用时**自动回退**并给出 `mode:"js"` + `fallback_reason`。
- `@e` 引用来自 JS 快照时，`click` / `fill` 直接走页面内动作，不再为一次点击去 attach debugger。
- 新增 `exec --max-bytes N`：返回值超限时返回截断预览（`truncated:true` + `total_chars`），避免单条命令塞满上下文。
- 新增 `docs/FEATURE_STATUS.md`：已实现 / 下一步 / 边界与实测性能。
- 新增 `tests/concurrency_bench.mjs`（真实浏览器并发基准）、`tests/fixtures/form.html`（本地动作测试页）。
- 新增扩展版本自检：`/health` 的 `extension` 段给出 `bundled_version` / `connected_versions` / `stale` / `hint`，`status` 在过期时输出 `warning`，`doctor` 增加 `extension_fresh` 检查项。扩展版本升到 **2.3**。
- 扩展支持**文件变化自动重载**：每分钟比对 `background.js` 内容哈希，发现磁盘改动就在空闲时 `chrome.runtime.reload()`，以后改扩展不必再手动点「重新加载」（首次仍需手动一次）。
- daemon 在扩展连上来时比对版本，**低于随包版本就直接要求浏览器重载扩展**（协议走 `management.reload`），无需用户操作。
- 新增扩展诊断命令：`injectProbe`（分别验证 MAIN world 纯函数 / MAIN world eval / ISOLATED world eval）、`selfCheck`（哈希基线、定时器状态、命令占用）。
- 新增 `tests/check_page_script.mjs`：校验扩展生成的注入脚本能被解析且返回值正确（纯 Node，不依赖 Chrome）。

### 修复

- **导航中的空白文档被误判成「已放行」**：`challenge wait` 在页面 `readyState=loading` 时会把它当成「没有挑战」。现在检测返回 `pending`，wait 遇到 pending 继续等；命中「清除」后还要稳定 250ms 才算数。
- **`DriverState` 全局锁拖慢结果回传**：`pending` / `acked` / `active_exec_sessions` 拆到独立的 `ExecRegistry`（新增一把锁），Ack/Result/Error 不再和会话读写抢同一把锁；超时/断连/关标签三处清理统一收口到 `register` / `take` / `take_session` / `retain_sessions`。
- **扩展错误双重转义**：扩展把错误再包一层 `{"error": "..."}` 时，命令错误里会出现 `{\"error\":\"...\"}`。现在会剥到最内层文本（`extension_control` 与网络层下发都适用）。
- **压缩响应改写可能破坏字节**：CDP 响应阶段只改状态码/响应头时，改用 `Fetch.continueResponse` 原地放行，不再 `getResponseBody + fulfillRequest` 重新编码；只有确实替换 body 时才去掉 `content-length` / `content-encoding`。
- **严格 CSP 页面 JS 全废**：`eval` 被 CSP 拦下后，`read`/`find`/`snapshot`/`action`/`wait` 原本直接失败。现在扩展注入固定的 `ops/page_*.js` 并调用其函数，不经过 eval，GitHub 首页实测 `read` 269 字、`find` 19 个链接、`snapshot` 5 个 `@e`、`hover/scroll/wait` 全可用。
- **XHR 响应改写不可靠**：旧实现在 `loadend` 回调里改 `xhr.responseText`，而页面自己的 `onload` / `addEventListener` 往往比我们先注册，读到的是原始响应。现在接管 `XMLHttpRequest.prototype` 的 `responseText` / `response` / `status` / `statusText` / `getAllResponseHeaders` / `getResponseHeader`，规则在 `send()` 前挂到实例上，任何时刻、任何监听器读到的都是改写值；覆盖 `text` / `json` / `arraybuffer` / `document`，`blob` 记 `response_rewrite_skipped`。
- **XHR mock 的响应头为空**：mock 以前只伪造 `status` / `responseText`，`getResponseHeader('content-type')` 拿不到默认头。现在与真实响应共用同一套访问器，默认 `content-type: application/json` 可见。
- **XHR 请求头被追加而不是覆盖**：原生 `setRequestHeader` 对同名头是追加，页面设过 `x-token: origin` 再被规则改写会变成 `origin, rewritten`。现在请求头缓冲到 `send()` 统一下发，规则可以真正覆盖。
- **`wait --js` 仍是每 100ms 注入一次**：谓词改为在页面里编译一次、反复求值（MutationObserver + 微任务合并 + `setInterval` 兜底 + `visibilitychange`），daemon 只注入一次；换页销毁上下文时按剩余时间重试。返回新增 `mode:"js-observer"`、`injections`、`checks`。
- **`wait` 会堵住同一标签**：等待期间不再持有会话锁，`wait --js` 跑 30 秒时同标签的 `read` / `click` 仍能正常执行。
- **`net mock --response-status/--response-body` 的 `-H` 被忽略**：响应改写只读 `response.headers`，`--header` 设在规则顶层时静默失效。现在顶层 `headers` 对 mock 与改写都生效，`response.headers` 优先级更高。
- **`wait` 在 CSP 页面必然超时**：旧实现每 100ms 注入一次 eval 脚本，CSP 页面每次都失败，2 秒内白跑 46 次。现在改用页面内 MutationObserver，一次注入完成，同时也修掉了「后台标签页定时器被节流」的老问题。
- **`net mock --status` 默认值吞掉响应改写**：`--status` 原本默认 200，导致任何规则都被当成「伪造响应」，`--response-status/--response-body` 永远不生效。现在只有显式给 `--status` 或纯 mock 时才伪造。
- **同标签并发互相干扰**：同一标签上并发注入多个脚本时，后一个可能打断前一个的上下文。现在每会话串行排队。

- **`@e` 引用跨导航直接失效**：原先只要 URL 变化就报 `ref expired`。现在按 role+name 在新页面自动重定位（最多扫 600 个可操作元素），成功后返回 `ref_relocated:true` 与新的 `ref_selector`；确实找不到才报 `ref_expired`，并说明旧/新 URL。SPA 重渲染导致 backendNodeId 失效时也会走同一套重定位。
- **超大页面 `scan` 近乎卡死**：26 万元素页面上，`optHTML` 逐节点 `getComputedStyle` + 整棵 DOM 序列化会让 `scan` 超过 230 秒仍未返回。现在：`optHTML` 有节点预算、元素数 > 5 万时跳过布局读数、> 6 万时 `scan` 直接返回文本；`scan --text-only` 改走 `page_read` 的单遍 TreeWalker（4.7 s → 0.3 s）。
- **页面动作失败仍返回 `ok:true`**：`click`/`fill`/`hover`/`select`/`check`/`scroll` 命中不到目标时，页面动作返回 `{ok:false,error}`，但命令信封仍是成功。现在会向上抛出失败，调用方不会再误以为点到了。
- **`read` 只返回可见文本**：新增 `--include-hidden`，并在 `text_mode` 里明确当前是 `innerText`、近似可见还是全量文本。

- **daemon 随调用方一起被回收（Windows）**：拉起 daemon 时补 `DETACHED_PROCESS` 与 `CREATE_BREAKAWAY_FROM_JOB`（不允许 breakaway 的环境会自动退回），避免在带 Job Object 的终端/CI 里 CLI 一退出 daemon 就消失、每条命令都冷启动。
- **`exec` 永远返回 null**：扩展侧原来用「代码里出现 return 就当函数体编译」的启发式，而 daemon 已经把用户脚本包成 `(async () => {...})()`；按函数体编译后末尾没有 return，IIFE 的返回值被整段丢掉，所有 `exec` 都得到 `null` 并回退 CDP。现在先按表达式求值并等待 Promise，只有 eval 因语句级 return / 顶层 await 报 SyntaxError 时才退回函数体执行，且用「是否真的存在语句级 return」决定要不要补 return。
- **注入失败信息误导**：CDP 回退失败时只报 "CDP fallback failed"，把真正的注入错误盖住了。现在会同时带上首次注入的错误，并在页面返回 null 时再做一次探测，区分「页面 CSP 禁止 eval」和「注入被拒绝」。
- **`exec --frame` 失败会静默落到主框架**：指定 frame 时若 frame 不存在，原先会走 CDP 回退并在主框架执行成功，看起来“执行了但结果不对”。现在带 frame 目标时禁用 CDP 回退，直接报 `No frame with id N`。
- **页面内点击重复触发**：`__abcAct('click')` 手动 dispatch 了一次 click 又调用 `el.click()`，页面会收到两次点击，提交类按钮被重复触发。现在只走原生 `el.click()`。
- **错误信息嵌套 JSON**：CDP/扩展错误常是 `{"error":{"message":...}}` 形式的字符串，之前整段塞进提示。现在统一剥出可读 message；CDP 与 DOM 回退都失败时给出合并说明。
- **扩展 WS 空指针崩溃**：`[TMWD-WS] message parse error TypeError: Cannot read properties of null (reading 'send')`。命令执行中途遇到 daemon 重启 / 插件重载时，`onclose` 会把全局 `ws` 置空，后续 `ws.send` 抛异常且该命令永远没有回复。现在每条连接持有自己的 socket，发送统一走 `socketSend` 安全封装，`onclose` 只清空属于自己那条连接的全局引用。
  - 补充：这条报错**不会**因为改文件而消失，Chrome 里跑的始终是加载时那份代码。若 `atb status` 显示 `extension.stale: true`，说明浏览器仍在跑旧扩展，需要到 `chrome://extensions` 点一次「重新加载」。
- **扩展断开时命令立即失败**：daemon 在 WebSocket 断开时，把挂在这些会话上的 pending 命令以 `extension_disconnected` 失败，不再让 CLI 干等 15~60 秒超时（实测 66ms 返回）。
- **失败可编程**：`ok:false` 现在退出码为 `1`，`job wait` 超时为 `3`；设置 `AGENT_BROWSER_CLI_EXIT0=1` 可恢复旧的恒 0 行为。
- **并发串台**：删除 `execute_raw_js_on_tab` 里"临时改写全局 `default_session_key` → 执行 → 还原"的做法，改为显式携带 session_key；`get_html` 同样改为显式会话。后台 job 与前台命令并发时不再互相覆盖路由。
- **冷启动 token 缓存 bug**：等待 `/ping` 时不再把旧 token（或 `None`）固化进进程缓存，避免 daemon 刚启动后首条业务请求 401。
- **断开会话不清理**：超过 10 分钟的断开 session 连同 snapshot 缓存一起清理，长跑 daemon 不再只增不减。
- **WS 端口绑定失败被吞**：绑定失败写入状态，并在 `/ping`、`/health` 暴露 `ws_bind_error`，doctor 不再"端口已监听"误报。
- **截图/PDF 默认路径**：不再硬编码 `/tmp`，改用系统临时目录（Windows 不再落到当前盘符根目录）。
- **`snapshot --details` 串行 CDP**：每 40 个元素的 `resolveNode + callFunctionOn` 合并成一条 batch，200 个引用从 200 次往返降到 5 次。
- **鼠标三事件串行**：`mouseMoved/Pressed/Released` 合并为一条 batch，并补 `pointerType`。
- **扩展 attach 报错不可读**：页面内嵌其他扩展 iframe 时 Chrome 会拒绝 attach（crbug 40753571），错误信息现在附带可执行建议。
- **`exec --timeout` 真正生效**：默认上限从写死的 15 秒改为 60 秒（可按需放大到 600 秒）；超时返回 `ok:false` + `exec_timeout`，不再用 `ok:true` + 提示字符串伪装成功。
- **`scan --timeout` 真正生效**：页面内容抓取超过上限返回 `scan_timeout`；CLI 侧超时比服务端多留余量，避免只看到 reqwest 的裸超时错误。
- **单行脚本返回值错误**：`a(); return b` 这类写法会被扩展侧"自动补 return"的启发式误改成只返回 `a()`。现在 daemon 侧统一做确定性包装（`return x` / 裸表达式 / 多语句 + await 三种写法语义一致），扩展侧也不再对含语句级 `return` 的代码做自动补全。
- **后台标签页 `--wait-js` 必然超时**：等待循环从页面内 `setTimeout` 改为 daemon 侧轮询（隐藏标签页定时器会被 Chrome 节流到秒级/分钟级）。输出形状保持兼容，新增 `wait.polls`。
- **Windows 上 CLI 卡死**：CLI 持有启动锁时拉起 daemon，子进程继承锁文件句柄导致后续命令永久阻塞；现在对锁文件与 std 句柄关闭继承。
- **token 文件异常时误判 daemon 未启动**：新增免 token 的 `/ping`，401 不再触发"重复拉起 daemon + 白等 15 秒"；token 不匹配时给出明确错误。
- **`open` 后第一条命令可能打到旧标签页**：`open` 等待新会话就绪（最多 3s），`select_tab` 优先使用待生效的默认会话。
- 空 token 文件不再让命令直接失败。

### 性能

- `select_tab` 不再无条件抢写锁：默认会话已指向目标标签时走纯读路径（抽出 `resolve_session` 做纯计算），同标签连续命令不再互相排他。
- `AppState.driver` 从 `Mutex` 改成 `RwLock`：tabs / browsers / health / 会话查询等只读路径并发，写路径（tabs_update、选默认会话、快照缓存、net 配置）保持排他；13 处写点由编译器（E0594/E0596）逐个确认，无遗漏。


- 内部调用（`read`/`find`/`action`/`wait`）不再枚举全部会话做新标签检测，每次少一次全量会话序列化。
- 实测 12 标签 × 5 轮共 60 步（`read` 4KB + `exec` 15ms 忙等）：并行 `batch` 82 ms，串行 `run` 542 ms，加速 **6.6x**；单标签 12 条并发命令 5 ms。基准会预热标签并对并行取两轮较小值。

- CLI 复用同一个 HTTP Client（原先每条命令新建 2 个 blocking client，各带独立运行时线程），热路径探活改为 TCP 探测，少一次 HTTP 往返。
- `tabs` 数据模型保留完整 URL，截断只发生在展示层；`snapshot` 与 `@e` 过期校验因此省掉两次 `location.href` 往返。
- 新增 `run` 管道：多步流程从 N 个进程降到 1 个。
- 移除 `scan` 中"跑一次列表识别脚本然后丢弃结果"的无效路径（全量 `querySelectorAll('*')` + 20 个容器强制布局），扫描明显变快；列表识别后续以 `scan --list` 形式回归并返回结果。
- 扩展：只有脚本涉及 `alert/confirm/prompt` 时才做弹窗抑制注入（原先每条 exec 固定 2 次）；只有脚本可能新建标签页时才等待 120ms（原先固定 200ms）。

### 调整

- 扩展：标签页列表同步改为 250ms 去抖；内容脚本按扩展版本只补注入一次；状态角标仅在页面可见时轮询。
- 扩展：CDP attach 后调用 `Page.setWebLifecycleState('active')`，降低后台长任务被冻结的概率。

## v0.3.7 - 2026-08-02

### 修复

- Chrome 扩展不再全局移除页面 CSP 响应头或 meta 标签；升级时会清理旧版持久化的动态规则，避免与 Tampermonkey 等注入脚本共同导致 YouTube renderer 满载卡死。受 CSP 限制的页面执行继续使用现有 CDP fallback。

## 已发布

## v0.3.4 - 2026-05-19

### 修复

- 修复 daemon 非 graceful 退出后扩展侧 CDP attach 状态残留，导致后续 exec/CDP fallback 报 `Another debugger is already attached` 的问题。

### 调整

- 调整 `atb` skill 的默认操作流程：浏览器任务优先直接执行 `tabs` / `open` / `exec` / `scan`，由 CLI 按需自动启动 daemon。
- 明确 `daemon_not_running` / `running=false` 只是未常驻状态，不能在尚未执行目标命令前阻塞任务。
- 将 `status` / `doctor` / `logs` 收敛为目标命令失败、连接异常或用户明确排障时才使用的诊断入口。

## v0.3.3 - 2026-05-18

- `tabtree` 默认改为 compact 输出，截断长 URL 并省略 `session_key` 以降低 token 消耗；新增 `tabtree --full` 输出完整 URL 和 `session_key`。
- 优化 `tabtree` 实现，daemon 锁内只复制必要会话字段，锁外完成排序和 JSON 组装。
- Chrome 扩展 popup 增加 label 唯一性提示：推荐用 CLI 设置 label 以校验当前 daemon 内跨 Profile 唯一性。
- 平台 npm 包打包时默认对复制后的原生二进制执行 `strip`，减小发布体积；本地 `target/release` 二进制保持不变。
- 新增 `tabtree` 树形查询命令，支持按 `tab_id`、`profile_id/profile_label`、`browser_id` 过滤，并保留 browser → profile → tab 父子节点；原先临时增加的 `profiles` / `browsers` 摘要命令已移除，统一使用 `tabtree` / `lookup`。
- 新增 `lookup tab|browser|profile` 反查命令，可由 `tab_id` 反查 `browser_id` / `profile_id` / `profile_label`，或由 `browser_id` 反查所属 profile。
- 移除扩展默认注入的全局 `alert` / `confirm` / `prompt` 重写，改为 CLI 页面执行期间临时抑制弹窗并在命令结束后恢复原生函数。
- 新增 `profile-label set|clear`，并在 Chrome 扩展 popup 中支持设置 Profile Label；label 冲突时 CLI 按歧义处理，不参与内部路由主键。

## v0.3.2 - 2026-05-17

### 修复

- 修复长 URL 页面中 `snapshot` 后立即使用 `@e` 可能误报 `ref expired` 的问题。

## v0.3.1 - 2026-05-17

### 新增

- 新增 `logs`：读取 `~/.atb/logs/daemon.log`，支持 `--tail`。
- 新增 `doctor`：输出 JSON 诊断信息，不自动启动 daemon。
- 新增 `install-skill`：支持 `--dry-run` 和 `--yes`，用于安装/更新 Agent skill。
- 增强 `status`：新增 `healthy`、`summary`、`message`，保留旧字段。
- 新增 `snapshot`：基于 Chrome Accessibility Tree 生成 `@e` 操作引用，支持 `--offset`、`--limit`、`--details`、`--tab`。
- 新增高层操作命令：`click`、`fill`、`send-keys`、`mouse-click`，支持 `--monitor`、`--wait-js`、`--wait-timeout`。
- 新增 `screenshot`：支持视口截图、全页截图、元素截图、PNG/JPEG、默认落盘目录。
- 新增 `save-pdf`：支持纸张、横向、缩放、打印背景、默认文件名清理和 50MB 上限。
- 新增 `network` 调试命令：`start`、`list`、`detail`、`clear`、`stop`。
- 新增 `console` 调试命令：`start`、`list`、`clear`、`stop`。
- 新增多浏览器 / 多 Chrome Profile 会话隔离：扩展上报 `browser_id`、`profile_id`、`profile_label`，daemon 内部使用 `session_key = browser_id:profile_id:tab_id` 路由。
- CLI 主要浏览器命令新增 `--browser` 和 `--profile`，支持按浏览器实例和 Profile 过滤/定位 tab。
- `tabs` 输出新增 `browser_id`、`profile_id`、`profile_label`、`tab_id`、`session_key`。
- `open` 新增 `--session` 和 `--group-title`，支持 Chrome 原生 tab group。
- `open` 新增 `--window`，支持新开独立 Chrome 窗口。
- `open` 新增 `--focus`，只有显式传入时才请求聚焦窗口。
- `open --window --group-title/--session` 支持对新窗口首个 tab 创建 Chrome tab group。
- 新增 `close --tab <tabId>`，通过扩展原生 `chrome.tabs.remove` 关闭标签页。
- Chrome 扩展新增图标，显示名改为 `Agent Browser CLI Bridge`。

### 调整

- `open` 返回结构改为以新打开目标为主：`opened_tab_id`、`opened_session_key`、`window_id`、`window`、`group`。旧执行通道移入 `metadata.executor`，避免 AI 误用旧 tab。
- daemon 在默认 session 失效时会回退到当前 active session，避免重启后卡在旧 `session_key`。
- `snapshot`、`@e` 缓存、调试缓存、截图/PDF、network/console 等路径统一按 `session_key` 隔离。
- daemon stdout/stderr 日志固定写入用户目录，不再落到当前目录或 npm 包目录。
- npm wrapper 设置 `AGENT_BROWSER_CLI_PACKAGE_DIR`，方便全局安装后定位内置 skill。
- `network stop` / `console stop` 会停止监听并清理对应缓存。
- daemon 退出时清理 daemon 缓存，并通知扩展清理 network/console 调试缓存。
- skill 文档重写为 Agent 操作 SOP，明确 `scan`、`snapshot`、`exec/CDP` 的职责边界，并补充多 Profile、多浏览器、`open --window`、`--focus` 和 `opened_tab_id` 使用说明。

### 验证

- 已覆盖真实 Chrome 场景：百度搜索、页面扫描、DOM 定位、输入/点击、截图、PDF、network/console 监听、缓存清理、tab group、关闭测试 tab。
- 已覆盖多 Profile 字段上报、`tabs --profile`、`tabs --browser --profile`、`exec/scan/snapshot/click/fill/send-keys/mouse-click/screenshot/save-pdf/network/console --profile --tab`、`open` tab/window/background/focus/group/session 组合。
- 已执行 `cargo fmt --check`、`cargo check`、`cargo build`、`cargo test`、`node --check`、`npm pack --dry-run`。

## v0.2.10-extension - 2026-05-15

### 调整

- Chrome 扩展默认不抢占系统前台窗口。
- `Page.bringToFront` 这类可能聚焦浏览器的行为改为显式 `allowFocus=true` 才执行。

## v0.2.9 - 2026-05-15

### 修复

- 同步平台包版本，保证 npm 包与 Rust crate 版本一致。

## v0.2.8 - 2026-05-15

### 新增

- 支持自定义 Chrome 扩展 WebSocket 监听端口。
- 新增 `set-extension-port <port>`，写入 `~/.atb/config.json`。
- daemon 运行中修改端口时自动重启，使新端口立即生效。

## v0.2.7 - 2026-05-15

### 调整

- 发布版本 `0.2.7`。
- 整合 release/publish 流程，为后续多平台包发布做准备。

## v0.2.6 - 2026-05-15

### 新增

- 支持 Linux ARM64 平台包。

### 文档

- 更新 Chrome 扩展安装说明，推荐从 Release 下载 `chrome-extensions.zip`。
- 补充 Linux 与 WSL 使用前提。

## v0.2.5 - 2026-05-13

### 调整

- 准备 `0.2.5` 发布。
- 更新支持平台说明。
- 改进 Chrome 扩展右侧提示角标，并补充文档说明。
- 增加 GenericAgent 友情链接。

## v0.2.4 - 2026-05-13

### 调整

- 准备 `0.2.4` 发布。
- 改进 release notes 保留逻辑。
- 增加 Linux 构建验证。

## v0.2.3 - 2026-05-12

### 修复

- 修复 Release npm artifact 匹配逻辑，让发布产物查找更稳健。
- 更新 `Cargo.lock` 到 `0.2.3`。

## v0.2.2 - 2026-05-12

### CI

- npm token 不存在时跳过 npm publish，避免发布流程失败。
- 增加 npm 发布 GitHub Actions 工作流。
- 调整 macOS Intel runner。

## v0.2.1 - 2026-05-11

### 移除

- 移除旧 Python runtime，CLI 主链路切换到 Rust 实现。

## v0.2.0 - 2026-05-11

### 新增

- 新增 Rust npm CLI release。
- 通过 npm 包分发 Rust CLI。

### 文档

- 补充性能参考和延迟数据说明。

## v0.1.1 - 2026-05-11

### 修复

- CLI 关闭时主动关闭 bridge，避免连接残留。

### 文档

- 补充安装指引，强调替换项目路径占位符。

## v0.1.0 - 2026-05-09

### 新增

- 新增 `open 命令。
- 支持通过 Chrome 扩展原生创建标签页。
- 建立基础浏览器控制 CLI 能力。
