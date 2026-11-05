# 更新日志

记录对外可见的功能、行为与修复。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。日期为北京时间。

`atb` 有两条独立版本线：CLI/daemon（Cargo 与 npm 同号）和 Chrome 扩展（`assets/atb_bridge/manifest.json`）。
两者的消息协议另有版本号，升级时以 `atb status` 的 `compatibility` 块为准。

| 版本线 | 当前 | 真相源 |
| --- | --- | --- |
| CLI / daemon | `0.7.2` | `package.json` / `Cargo.toml` |
| CLI ↔ daemon API 协议 | `5` | `atb status` |
| Chrome 扩展 | `5.3.0` | `assets/atb_bridge/manifest.json` |
| 扩展 ↔ daemon 消息协议 | `4` | `atb status` |

0.3.7 及更早版本、以及重构前的逐条修复记录，见 [docs/CHANGELOG_ARCHIVE.md](docs/CHANGELOG_ARCHIVE.md)。

## [0.7.2] - 2026-11-04

截至 2026-11-04，CLI 仍为 `0.7.2`；扩展同步升到 `5.3.0`（消息协议仍为 `4`）。本节汇总 0.7.1 之后已合并的 CLI/daemon 改动与 5.3.0 扩展改动；向后兼容，不删除、不改名已有字段。

### Added

- **`atb schema --json`**：输出全部命令的机器可读 schema（参数名、类型、默认值、可选值，以及主要 `error_code`）。Agent 一次拿到用法，不必逐条 `--help` 或试错。
- **`atb capabilities`**：一次探测 daemon 版本、扩展版本与能力门禁（`core` / `open_session` / `group` / `window` / `current` / `challenge`），阈值随输出给出；daemon 未运行时报结构化的 `daemon_not_running`。
- **`atb doctor --fix [--force]`**：在诊断之外自动修复旧 daemon 占用 18767、API token 失配、扩展过旧。`--force` 才真正结束占用端口的进程；每条修复输出 `{fix, executed, result}`。
- **`atb scrape`**：批量抓取。`--urls` 接受文件或逗号列表（自动去重），分批后台开页、并发执行 JS、结果逐行写入 JSONL、批完关页；失败按 3s/5s/7s 退避重试。默认脚本取 `{title, url, text(前 9000 字)}`。
- **`open --wait-for <dom-quiet|selector:S|text:T>`**：打开后继续等页面条件满足才算成功。`dom-quiet` 是正式名（资源计数稳定 + DOM 静默，近似 network idle）；`networkidle` 保留为语义相同的兼容别名。超时跟随 `--timeout`，失败目标返回 `wait_for_timeout`，不影响同批其它目标。
- **`open --warm`**：打开前先 ping 一次扩展唤醒 service worker；ping 失败重试一次后照常打开。
- **`tabs --fields / --limit / --mine`**：字段白名单（未知字段报 `unsupported_flag`，退出码 2）、条数截断、只看本 agent `open` 过的标签页。
- **`close --tab group:<标题> --yes`**：按组关闭必须显式确认。不带 `--yes` 时报错并告知将关闭的标签数；`all` / `url:` / `title:` 仍然拒绝。

### Changed

- **`open` 返回统一**：单 URL 与多 URL 都带顶层 `targets: [{url, tab_id, ok, ready, elapsed_ms}]` 与 `count`。旧的 `result.opened_tab_id`、`tabs[]` 原样保留，新代码建议只解析 `targets`。
- **结果未知语义**：POST 已发出但响应未回（超时、写后读失败、连接中断）改为 `error_code:"outcome_unknown"`，退出码 **4**，不再误报 `unexpected_error`。配套新增全局 `--idempotency-key`：daemon 对 60 秒内同 key 的 POST 回放首次响应（`idempotent_replay:true`），动作类命令可安全重发。
- **迟到结果可查**：扩展在请求超时后才回包的 exec 结果暂存 5 分钟，新增 `GET /exec-result/:exec_id`；exec 超时错误会附带 exec_id 与查询指引。
- **目标解析错误细分**：`no_tabs` 拆成 `tab_closed` / `session_gone` / `session_mismatch` / `no_target_tabs` / `extension_disconnected` / `target_not_found` / `target_ambiguous`，统一带 `retryable` / `retry_after_ms` / `hint`。CLI 按码映射退出码，不再依赖中文错误文本。
- **源码模块化**：`src/server.rs` 拆为 `src/server/` 下 29 个领域模块，`src/cli.rs` 拆为 15 个模块；`run()` 拆成按命令族的处理函数。行为与调用关系不变。
- **退出码契约归一**：失败统一输出结构化 JSON；参数类错误统一退出码 2；`ATB_EXIT0` 只在值为 `1` / `true` 时生效。
- **`--async` 语义统一**：不支持后台执行的命令直接报 `unsupported_flag`（退出码 2），不再静默按同步执行；`inject` / `frames` 补上真正的后台提交。
- **`--dry-run` 覆盖面**：`close` / `run` / `batch` / `trust-extension` / `set-extension-port` 也只输出计划；计划统一包进 `result`，与真实执行同一套 `jq .result` 解析。
- **`atb restart --force`**：daemon 在跑但 token 不匹配时，自动定位并结束占用 18767 的进程后重新拉起，解开「restart 让你去结束进程」的自锁。
- **job 淘汰语义**：任务表达到硬上限强制淘汰 running 任务时留存根（最多 32 条）；`job get/wait` 命中存根返回 `job_evicted`，不再伪装成 `job_not_found`。

### Fixed

- **click 自动降级**：CDP 路径点击失败且错误属于 CDP 层时，自动改走 `mouse-click`（`Input.dispatchMouseEvent`），响应带 `used_fallback` 与 `fallback_reason`。
- **受限站点报错**：命中 Chrome 的 `Cannot access a chrome-extension:// URL of different extension` 时，翻译为 `page_protected` 并给出替代方案（`read` / `scan` / `snapshot` / `mouse-click`），保留 `raw_error`。
- **假成功**：`upload` / `mouse-click` 逐条校验批处理结果；`wait` / `broadcast` / `run` / `batch` / `multi` / `job` 的内层 `ok:false` 上浮为退出码非 0。
- **多智能体隔离**：异步任务在 `tokio::spawn` 前捕获调用方身份，不再落到 `default` 的标签页。
- **超时参数护栏**：`NaN` / `Inf` / 超大值不再触发 panic，统一走 `secs()` / `clamp_timeout_seconds`。
- **会话与状态机**：`prune_sessions` 真正生效；WS 断开清理 refresh 登记；`agent-tabs.json` 原子写并在损坏时隔离；`net reset` 先停后删，规则改动广播到同 profile 的所有标签。
- **扩展自愈**：SW 重启后恢复规则与采集状态；按 tab 串行 attach；发送队列与 socket close 守卫。
- **页面脚本**：`snapshot --js` 支持 `offset` 分页；`net fetch` 不再把 HTTP 404 当命令失败；`wait --text --visible` 生效；`setValue` 事后校验；非法 selector 返回结构化错误。
- **`net reset` 不再把休眠标签算失败**：页面级清理区分 `failed`（明确报错）与 `unconfirmed`（超时/断连），后者只进 `hint`。
- **配置写入原子性**：Windows 改用 `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`，读方不会再撞见文件短暂不存在的空窗。
- **挑战页（0.7.1 之后）**：识别与求解不再向页面写任何 `window.__abc*` 全局；求解器改为按元素证据推进（token、是否离开挑战页、组件是否存在），不再按固定偏移和固定节奏点击；Turnstile 组件在 closed shadow root 内也能被识别；点完不再死等固定秒数；页面存在其他扩展的 `chrome-extension://` iframe 时自动绕过 CDP 限制。
- **`install-skill` 在 Windows 上更新软链接失败**：指向目录的软链以前一律用 `remove_file` 删除，Windows 返回「拒绝访问 (os error 5)」，于是主目录已更新、命令却以失败退出。改为先 `remove_file`、失败再 `remove_dir`，并补一条回归测试（无建链权限的环境自动跳过）。

### Added / Fixed (2026-11-04)

本节修复以下问题：

- **受限页面不再等于不能截图**：新增扩展命令 `captureFullPage`（`chrome.scripting.executeScript`
  逐屏滚动 + `chrome.tabs.captureVisibleTab` + `OffscreenCanvas` 拼接，**全程不 attach 调试器**）。
  `screenshot --full-page` 与 `save-pdf` 在 `chrome.debugger` 被拒时自动回落：前者得到整页长图
  （`via:"captureFullPageStitched"`、`slices`、`truncated`），后者得到单页图片型 PDF
  （`via:"stitched-image-pdf"`、`text_selectable:false`）。新增 `--max-height`（设备像素，默认 20000）。
  `captureVisibleTab` 有 2 次/秒的 Chrome 硬限流，实现里做了 550ms 节流，否则连续滑动必失败。
- **`mouse-click` 合成事件回落**：CDP 被拒时改在页面内派发 pointer/mouse 事件序列，
  返回 `synthetic:true` + `trusted:false`，不再直接失败。
- **阻塞扩展可点名**：新增扩展命令 `blockingExtensions`，受限页面的错误文案会直接写出
  「页面内嵌了扩展 XX 的 iframe，Chrome 拒绝 attach」。
- **幂等缓存跨进程持久化**：`Done` 条目落盘到 `<user_config_dir>/idempotency.jsonl`，
  daemon 重启后 60s 内仍能回放首次响应；`Pending` 不落盘（进程崩在中途没有可信结果）。
- **多浏览器默认目标不再漂移**：断线时把原默认记进 `preferred_default_session_key`（6 小时 TTL），
  同一个标签重连后自动恢复默认；`read` 在「≥2 个浏览器 + 隐式目标」时附带 `multi_browser_hint`，
  `/health.connection` 暴露 `default_session_key` 与 `profiles[].is_default`。
- **`open` 在默认标签被关掉后仍可用**：`open` 前清理本调用方已失效的默认目标，不再被
  `target_lost` 卡住（e2e 实测：`cleanup` 之后 `open` 本地页会直接失败）。
- **`browsers` 不再假空**：补 `wait_for_sessions` + 主动刷新；桥还在但快照未到时返回
  `warning_code:"snapshot_pending"` + `retryable`；`count` 改为浏览器数量，另给 `profile_count`。
- **`download <非 URL>`** 给出 `invalid_argument` + `did_you_mean`（`atb downloads` / `atb download-wait`）。
- **发布链**：部署脚本清理 `*.bak-YYYYMMDD[a-z]?` 历史备份；新增 `scripts/check-lock.mjs`
  （Cargo.lock / package-lock / 依赖集合三方比对），`npm run build` 与 CI 全部加 `--locked`。
- **schema 漂移有门禁**：新增反射测试 `schema_matches_clap_definitions`，用 clap 命令树与静态表
  双向比对命令名与长参数。测试当场发现 **5 个命令共 39 个参数**在表里缺失（`net` 24 个、
  `network`/`console` 各 6 个、`lookup` 2 个、`restart` 1 个），已补齐。
- **端到端受控页面**：`tests/e2e/pages/`（basic / spa / form / iframe-blocked / download）+
  `scripts/e2e.mjs` 六个场景 29 条断言，跑真实 daemon + 扩展，不依赖外网。

### Fixed (2026-11-04)

本节修复以下问题：

- **幂等**：`x-atb-idempotency-key` 现在绑定「agent + method + path + query + body 指纹」。
  同 key 换 body 返回 **409 `idempotency_key_reused`**；同 key 并发只执行一次（in-flight 占位，
  其余请求等待结果回放，超时 504 `idempotency_in_flight_timeout`）；dry-run 与空 body 不入缓存。
- **脚本重复执行**：`exec` 的「SyntaxError 后换 AsyncFunction 重跑」改为**先编译再决定通道**；
  扩展侧用前置探针区分「编译期语法错误」与「用户自己 throw SyntaxError」，后者不再二次执行
  （副作用翻倍）。实测 `window.__n++` 在抛错场景只加 1。
- **restart 必然失败**：daemon 每次启动都会轮换 token 文件，而 CLI 进程内缓存了旧 token，
  于是 `restart` 稳定报「API token 不匹配」。现在对 401 做一次「失效缓存 + 重发」，且
  `start_server()` 成功后立即失效缓存。
- **`scrape` 抓到的是空数据**：`/open` 回包字段是 `targets[]`/`url`（旧代码读 `tabs[]`/`target`），
  `/exec` 的返回值在 `result.js_return`（旧代码读 `result.value`）。两处都修，并兼容旧形状：
  实测 2 个 URL 全部 `ok:true` 且 `data` 非空。
- **截图通道**：`extension_control` 已剥掉 `data` 层，旧代码还去读 `value.data.data_url`，
  导致「非调试器截图」永远匹配不上、静默回落 CDP；页面被别的扩展 iframe 挡住时截图就彻底失败。
  修复后 `via:captureVisibleTab` 可用（实测 67KB / 2083×945 PNG）。整页与元素截图仍走 CDP，
  受限页面会给出「改截可见区域」的可执行提示；`save-pdf` 同样给出替代方案。
- **`mouse-click` 点空**：批处理里先发 `Page.bringToFront`，元素句柄在 `results[3]`，旧代码读
  `results[2]`，拿到的是「滚动」那一步的结果，于是坐标解析失败或点到别处。真实鼠标事件现已验证
  （`mousedown/mouseup/click` 各 1 次）。
- **选择器无匹配的错误文案**：`Runtime.evaluate` 返回 null 时 CDP 只回
  「Either objectId or executionContextId ... must be specified」，现在翻译成
  `target not found: <selector>`（`click` / `fill` / `screenshot --selector` 共用）。
- **应用层心跳**：daemon 对「90s 收不到任何入站帧」的连接会断开，但扩展从不发心跳，空闲页面会被
  周期性掐断。扩展改为每 30s 发 `{"type":"ping"}`，daemon 回 `{"type":"pong"}`；服务端判定改为
  解析而非字符串全等。实测 110s 空闲连接保持稳定。
- **连接代际**：会话带回 `conn_id`，旧连接的迟到回包与断开清理不再误伤新连接的会话。
- **目标丢失**：具名 agent 的专属默认失效直接 `target_lost`（不串到别人的标签），`default` 桶保留旧回退。
- **执行总预算**：`exec` 的等待、锁、派发、回包全链路共用同一个 deadline；锁等待超时返回
  `session_busy_timeout`（可重试），派发前预算不足返回 `request_budget_exhausted`。
- **`--dry-run` 全覆盖**：`exec --async`、`scrape`、`stop`/`restart`、`doctor --fix`、`back`/`reload`、
  `job cancel`、`trust-extension` 均只输出计划；`open --dry-run` 真做目标解析（`ftp://`、缺失文件退出 2）。
- **异步路径预检**：`screenshot`/`upload` 的 `--async` 也走同一套路径绝对化与 preflight。
- **`scrape` 契约**：`outcome_unknown` 不重试并最终退出 4；默认每 URL 一条最终记录（`--record-attempts`
  才逐次写）；关页 guard 覆盖所有退出路径；`--batch/--concurrency` 越界退出 2。
- **脱敏**：`find` 不再回退明文 `value`，`fill` 回显 `value_length`，`simphtml` 对 secret 字段强制遮罩。
- **网络拦截**：XHR 事件不再双触发；无 body 规则时不改写响应体；二进制响应不 decode/encode。
- **等待语义**：`wait` 的非 JS 路径加 trailing 窗口，窗口内的变更不再丢；`open --wait-for` 的条件统一为 `<dom-quiet|selector:S|text:T>`（`dom-quiet` = 资源计数稳定 + DOM 静默，`networkidle` 仅为兼容别名）。
- **引用重定位**：@e 重定位多候选返回 `ref_ambiguous`、无候选 `ref_not_found`，且只在「确定未派发」时重试。
- **批处理校验**：batch 回包校验条数/index 唯一性；裸数组与缺 `index` 的旧回包兼容。
- **后台任务**：`job` 上限（默认 32，`ATB_MAX_RUNNING_JOBS` 可调），硬淘汰前先 abort；`/health.jobs.max_running` 暴露阈值。
- **发布链**：`sync-extension-assets` 真删 ops 孤儿并固定检查顺序；`sync-version` 先校验 `Cargo.lock` 漂移、失败回滚；`deploy-extension` 清理临时文件与残留记录。
- **`@0` / `@0-MAX`**：序号 0 明确拒绝（退出 2），区间跨度用受检算术，不再溢出 panic。
- **进程身份核对**：`restart --force` / `doctor --fix` 强杀前逐个 PID 核对可执行路径与命令行，不符则跳过并给 `skipped_reason`。

### Fixed (2026-11-04)

本节修复以下问题：

- **挑战链路**：`auto_wait_challenge` 的识别重试循环改为受 `--challenge-timeout` 总预算约束（扩展挂死时不再空耗约 99s）；`challenge_auto_solve` 的轮询补上 `challenge_status_usable` 守卫，空载荷不再被报成 `solved:true`。同时：`role=textbox` 写入后读回校验、`reload` 成功才置 `reloaded`、`cdp_call` 缺 `ok` 且载荷非对象时报错、`challenge click --at` 尊重 `--timeout`、切标签失败写入 `diag`、裸 objectId 点击合并为单次往返；双组件场景按面积轮转前 3 个，不再只点第一个。
- **下载**：`download && download-wait` 不再必然假超时 —— 认领在跑条目后按 id 跟踪到终态，另加「10s 内开始的终态条目」窗口（扩展新增 `startTimeMs`/`endTimeMs`），历史旧记录仍不会被当成本次产物。
- **全局 flag**：`--dry-run` 对 `challenge` / `stealth` 真正生效（零 HTTP、只输出计划），对 `net fetch/hook/mock/unroute/reset`、`network`/`console` 的 start/stop、`download`、`broadcast`、`profile-label`、`launch` 明确拒绝（`unsupported_flag`，退出码 2）；`--async` 在 12 条不支持的命令上不再静默同步执行。
- **扩展持久化拦截**：`ws-close` / `keepalive-lost` 不再逐个删除持久化快照条目；重连与冷启动会对快照里的 tab **重新 attach** 再 `Fetch.enable`，DevTools 占用时保留条目下次重试（此前「保留快照」没有真正的恢复路径）。
- **服务端生命周期**：撤销断开的 Close 帧带超时；发送等待期间仍每 500ms 检查撤销；在途请求改为「每条请求进入时刷新起点」，`MAX_INFLIGHT_STALL` 提到 3900s（单条 3600s 长请求不再被误杀）；`open` 的 refresh 失败写明 `refresh_error`/`refresh_error_code`；并发许可获取失败不再无许可继续；batch 的 `JoinError` 带原因；关机清理写入的 `daemon_shutdown` 终态不再被完成回写覆盖。
- **CLI 契约**：`--challenge-timeout` / `--confirm-ms` / `download-wait --timeout,interval` 全部做有限值校验；`net --body`、`broadcast --args` 的非法 JSON 统一 `invalid_json`（退出码 2）；重复 `--header` 报错；`exec` 同时给脚本与 `--file` 报错；`run` 的 `stop_on_error` 冲突报错并回显生效值；`run`/`batch` 的 HTTP 超时按步骤预算求和（上限 24h，不再被 1h 截断）；`close --tab current/window:` 先解析再关；`print_logs` 走 EPIPE 安全输出；`status` 的 `/health` 探测放宽到 3s 并区分 `health_timeout` 与 token 失配。
- **根级健壮性**：HTML 优化入口加 8MB 上限并迭代拆链（超深嵌套不再栈溢出）；`launch` 强杀前逐个 PID 复核身份、等待期进程枚举带 5s 超时；`config` 回滚失败上抛、`.bak` 与主文件同样收紧 ACL；preflight 数值字段补齐并下沉到 `steps[*]`；`ExtTab.id` 非字符串/整数时丢弃该条；panic 输出结构化 JSON 并以退出码 1 结束。
- **快照与网络**：`snapshot` 输出 `ref_scheme`（`operable-only` / `all-named`），JS 侧角色集与 CDP 对齐到 18 个；JS 快照的可操作元素判定前置，深分页不再对前缀元素做布局读；`net --max-log` 两处 clamp；`net log --clear` 失败透出 `cdp_clear_error`。
- **扩展并发与竞态**：持久注入去重串行化；`connectWS` 单飞（不再建两条 WebSocket）；建连即排 keepalive（CONNECTING 卡死可被 15s 超时清掉）；断线期间不再入队 `result`/`error`/`ack`；base64 响应体按 4 字节对齐截断；`batch` 的 `$N` 引用改为结构化精确匹配；标签上报按 URL 过滤 Streamlit 内部端点，不再误伤同标题网页。
- **新增可选参数（此前的不可达项）**：`snapshot --no-skip-generic`、`read --inner-text-limit`、`challenge --confirm-ms`、`net fetch --credentials / --no-parse-json`。

### 扩展 5.3.0（2026-11-04）

扩展文件版本 `5.3.0`，消息协议保持 `4`，可与 0.7.x 的 daemon 配套；旧 daemon 不认识的新帧只会被忽略，不会让命令失败。

- **`challenge` 入口不可枚举**：`page_challenge.js` 整体包进 IIFE，入口 `window.__abcChallenge` 改用 `Object.defineProperty(..., {enumerable:false, configurable:true})` 挂载。页面侧 `Object.keys(window)` 看不到它，`stealth scrub` 也能真正 `delete` 掉；旧的顶层函数声明会留下不可删除、可枚举的全局，是稳定的自动化指纹。
- **open shadow DOM 支持**：`find` / `snapshot` / `read` 递归收集 open shadow root，返回项带 `scope:"shadow"`、`host_selector`、`shadow_path`，选择器写成 `宿主 >>> 相对路径`；回包给出 `scope` / `frame_count` / `shadow_roots` / `shadow_skipped` / `shadow_closed_unreachable`。closed shadow root 明确标注不可达（不伪造结果）。
- **断线重连兜底**：`onclose` 的清理步骤逐个 `try/catch`，`scheduleProbe()` 与 `scheduleFastReconnect()` 放进 `finally`，任一步抛错都不再导致「永久断连」。
- **socket 换代守卫**：`reconnectWS` 先摘掉旧 socket 的 `onopen/onmessage/onclose` 再关闭；`onopen/onmessage` 首层加 `ws !== socket` 守卫，过期连接的迟到帧一律丢弃。
- **命令幂等**：WS 命令按 `id` 记 60 秒终态账本（上限 256 条）。同 id 在跑时只回 `duplicate_inflight`，已完成则直接回放原 payload，超时重试不再重复执行副作用。
- **拼接内存预算**：整页拼接在建画布前按 `宽 × 高 × dpr` 预估像素，超过 `captureMaxPixels`（默认 8000 万，可用 `ATB_CAPTURE_MAX_PIXELS` 调整）返回结构化 `capture_too_large`，不再把 MV3 Service Worker 撑到 OOM。
- **剪贴板 TTL**：popup「复制 cookies」写入剪贴板后按 `clipboardTtlSec`（默认 30 秒）自动清空，清除前尽量 `readText()` 比对，`pagehide` 时再清一次。
- **popup 默认掩码**：Cookie 值默认渲染为 `•••`，必须点「显示值」并二次确认才展示；打开 popup 不再自动拉取 Cookie。
- **网络/console 缓冲上限**：`networkStart` / `consoleStart` 接受 `max_entries`（默认 5000，clamp 100–100000），超出丢最旧；`idempotent:true` 时重复 start 只更新配置并裁剪缓冲，不清空已捕获条目。
- **exec 取消帧**：新增 `cmd:"cancel"` 处理，按 `execId` 释放 daemon 已超时/取消的那次 CDP attach；被 network/console/Fetch 复用的 attach 回 `cancelled:false, reason:"in_use"`，不打断正在用的采集。

## [0.7.1] - 2026-11-03

### Fixed

- **挑战卡死 10 秒内给结论**：Turnstile 静默失败时不再等满超时，返回可诊断的 `reason`（`turnstile_not_rendered` / `window_not_visible` / `verifying_not_completed` 等）。
- **`--background` 打开挑战页时窗口不再最小化**：后台创建标签不会自动激活窗口，挑战页因此无法完成渲染与验证。
- **`hints` 使用最后一轮结论**：此前取首轮 status，提示永远是「通常几秒内自动放行」，与实际情况不符。

## [0.7.0] - 2026-11-03

### Changed

- **全面更名 `agent-browser-cli` → `atb`**：CLI/daemon、Cargo 包名、npm 包名（`@g1728578755/atb`）、帮助文本与文档统一。`AGENT_BROWSER_CLI_*` 环境变量作为兼容别名保留。
- **挑战页判定后切前台**：不再只在非 `--background` 时切；判定为拦截后统一让标签页可见，避免后台标签被浏览器节流。
- **不再自动 reload 挑战页**：reload 会把走到一半的验证打回原点；0.6.0 引入的「到达超时 60% 自动 reload」在 0.7.0 被移除，需要时用 `challenge wait --reload-after` 显式触发。
- **纯 interstitial 不再走 CDP 求解器**：CF managed challenge 没有可点控件，旧实现每轮空跑 CDP。
- **调试横幅缩短到约 1.2 秒**：`chrome.debugger` attach 后立即安排 detach，不再挂满 30 秒。
- **daemon 不再因空闲掉线**：只要还有扩展连着就不退出；扩展侧按 0.3→0.6→1.2→2.5→5 秒退避重连。
- **多浏览器可读身份**：扩展上报 `browser_label`（`品牌 · 系统 · 自定义名 · #短码`），`tabs` / `browsers` / `lookup` 都带这个字段。

### Added

- **`cookies --clear [--url <页面> | --domain <域>]`**：反复测挑战页、切换登录态时清理 Cookie。

## [0.6.0] - 2026-11-02

### Changed

- **命令改名为 `atb`**：Cargo `[[bin]]`、clap `name` / `bin_name`、npm `bin` 同步。
- **环境变量新前缀 `ATB_*`**：`ATB_AGENT` / `ATB_EXIT0` / `ATB_IDLE_TTL` / `ATB_NO_DEBUGGER` / `ATB_BIN` / `ATB_PACKAGE_DIR`；旧前缀继续生效，新名优先。
- **配置目录保持 `~/.atb`**：有意不改。换目录会让新旧 daemon 各用一套配置与端口，同时运行更难排查。
- **CLI ↔ daemon 协议 4 → 5**：升级二进制后必须 `atb restart`；新版 CLI 拒绝连旧 daemon，并给出修复路径。扩展协议仍为 4。
- **扩展升级到 5.0.0（ATB）**：页面操作脚本默认在 ISOLATED world 执行，不再把 `window.__abcRead` 之类的全局写进页面；只有 `net` 与 `stealth` 留在 MAIN world。
- **页面角标默认关闭**：`chrome.storage.local.showBadge` 缺省 `false`；开启后挂在 closed Shadow DOM，宿主 id 为 `atb-status-host`，历史 `#atb-ind` 会被清理。
- **网络钩子状态槽隐蔽化**：改为不可枚举的 `window['__atb_net_<随机>']`，包装后的 `fetch` / `XHR.open` 做 `toString()` 伪装与 `name` / `length` 对齐。
- **扩展部署改为原子替换**：新增 `scripts/deploy-extension.mjs`，逐文件「临时文件 + rename」再逐字节校验。

### Fixed

- **扩展 5.0.1**：`ext_ready` 上报时 `groupTitles` 未定义导致 `ReferenceError`。
- **扩展 5.0.3**：Service Worker 求值阶段 `removeLegacyCspRule()` 的 rejection 无人接管。
- **`open --challenge auto` 卡住不再干等**：等待过程中到达超时的 60%（夹在 3–12 秒）时 reload 一次再继续。
- **旧扩展下审计结果不可信**：扩展低于 5.0.0 时 `stealth` 返回 `extension_ok:false` 与 `fixes`，明确要求重载扩展。
- **挑战等待与 `open` 判定分叉**：`open --challenge` 复用 `challenge wait` 的判定、超时语义与 `need_human` 行为。

## [0.5.0] - 2026-11-02

### Changed

- **命令改名为 `atb2`**：二进制、帮助、文档、npm bin 全部更新；此版本是 `atb` 命名前的过渡。
- **扩展改名 `ATB`，版本 4.0.0**：manifest 名称与描述更新，避免与旧版混装。
- **`status` 增加 `compatibility` 块**：CLI 协议、daemon 协议、扩展协议三方比对。
- **CLI 启动前校验 daemon 协议**：不再尽力解析旧响应，直接给出重启或结束端口占用的修复路径。
- **归属记录落盘**：`~/.atb/agent-tabs.json`，daemon 空闲退出后重启仍能恢复「哪些标签是本工具打开的」。

### Added

- **多智能体隔离**：`--agent` / `AGENT_BROWSER_CLI_AGENT` / 终端标识三层身份；`open` 只改自己身份的默认标签页。
- **`cleanup`**：按「本工具 open 过的会话」收尾，支持 `--dry-run` 与 `--all-agents`，不按 URL 猜。
- **多窗口**：`TabInfo` 上报 `window_id`，`--tab window:<id>` 可批量操作。
- **非接触截图**：可见区域截图走 `chrome.tabs.captureVisibleTab`，不附加调试器。
- **`ATB_NO_DEBUGGER=1`**：彻底禁止 attach，需要 CDP 的命令返回替代方案。

### Fixed

- **`extension_outdated`**：依赖新字段的选择器（window / group / current）在旧扩展上明确报错，不再静默返回空。
- **扩展启动错误记录**：Service Worker 启动异常写入 `chrome.storage.local.lastServiceWorkerError`。
- **`tabs --ids` 缺窗口信息**：统一上报 `window_id`。
- **兼容性报告重复**：按 browser + version + protocol 去重。

## [0.4.0] - 2026-11-02

### Added

- **`--tab` 多目标语法**：`111,222`、`all`、`@1,@3` / `@1-5`、`url:<子串>`、`title:<子串>`、`group:<标签组>`、`current`。单 id 行为不变。
- **`/multi` 并发分发**：一次请求完成「解析目标 → 并发执行（默认最多 8 路）→ 按输入顺序汇总」，结果逐条带 `index` / `tab_id` / `ok` / `elapsed_ms`。
- **`open` 一次多目标**：`open url1 url2 url3` 由 daemon 内部并发打开，只占一个 HTTP 往返。
- **`--dry-run` 与本地预检**：CSS/XPath 括号与引号配平、JS 字符串/注释/括号配平、`--tab` 语法、URL 与本地文件、数值范围、输出路径冲突、`--async` 组合限制。错误带 `error_code` + `hint` + `example`。
- **退出码 2**：参数与预检错误独立成码，脚本可区分「改命令」与「重试」。
- **`tabs --ids`**：只回 `tab_id` / `title` / `url` / `active` / `group`。
- **`close --tab 111,222`**：支持批量关闭；拒绝 `all`，避免误伤用户标签页。
- **截图/PDF 输出占位符**：多目标时 `--out` 支持 `{tab}` / `{index}`，缺占位符在预检阶段拒绝。

### Changed

- **`open` 响应精简**：移除与顶层字段重复的 `metadata.raw`，一次开 6 个标签体积下降约 60%。
- **`tabs` 保证新鲜**：命令执行前主动向扩展拉一次列表（1.5s 超时，失败回落快照），不再依赖可能被 Service Worker 节流的推送。
- **`tabs` 顺序稳定**：统一按 browser → profile → tab id 排序，`@N` 不再随 HashMap 迭代顺序漂移。
- **打开多个标签更快**：6 个标签从约 3.4s 降到约 0.6s；单标签 `ready:true` 通常 50–200ms。
- **多标签操作更快**：6 个标签并发 `exec` 从约 1.0s 降到约 0.2–0.3s。

### Fixed

- **目标不存在的报错**：列出当前可用的 tab id 与标题，并提示 `tabs --ids` / `--tab @1`。
- **`--async` 与多目标**：明确拒绝并说明替代写法（一条 job 只对应一个目标）。
- **动作类 `--async`**：`hover/select/check/uncheck/scroll/focus` 加入支持列表，不再被判 unsupported。
- **多个 daemon 同时监听 18767**：统一走同一份会话表，刷新失败时退到更老的已就绪会话。
- **`exec` / `read` 在新标签偶发失败**：content script 尚未注入时自动退到其它已就绪会话。
- **`open` 的 `ready` 误报**：并发打开时最后创建的标签可能晚一拍出现，现在会重试补齐。

## [0.3.7] - 2026-08-02

0.3.x 的最后一个版本。该版本及更早的逐条记录见 [docs/CHANGELOG_ARCHIVE.md](docs/CHANGELOG_ARCHIVE.md)。

[Unreleased]: https://github.com/1728578755/atb-cli/compare/v0.7.2...HEAD
[0.7.2]: https://github.com/1728578755/atb-cli/compare/v0.7.1...v0.7.2
[0.7.1]: https://github.com/1728578755/atb-cli/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/1728578755/atb-cli/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/1728578755/atb-cli/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/1728578755/atb-cli/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/1728578755/atb-cli/compare/v0.3.7...v0.4.0
