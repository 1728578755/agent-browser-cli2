# 挑战页识别与自动化痕迹治理

> 适用范围：**自有站点、已获授权的测试环境、可访问性与兼容性回归**。
> 本文描述 `atb` 如何识别挑战页、如何等待放行、以及如何不在页面里留下多余痕迹；不涉及伪造浏览器指纹，也不提供绕过访问控制的方案。

---

## 0. 一句话结论

挑战页处理在工程上是**判定 + 等待 + 最小交互 + 兜底**的状态机问题，不是"破解"问题；痕迹治理的核心不是伪装，而是**别往页面里写不该写的东西**。

本项目驱动的是用户自己的 Chrome（扩展 + WebSocket 桥），不是 Playwright/Puppeteer 拉起的新实例。`navigator.webdriver`、`--enable-automation`、Headless UA、空 `plugins`、`outerWidth=0` 这一整类启动层标记，**从源头上就不存在**。真正需要处理的是两件事：

1. 挑战页要被**准确识别**，识别后要能**等到页面自己放行**，必要时完成一次**最小交互**；
2. 自己注入的东西（全局变量、DOM 节点、被包装的 `fetch`）要**尽量不写进页面**，写了要能**擦干净**。

---

## 1. 判据：站点会观察什么

先列判据，再谈实现。按从易到难分四层：

| 层 | 典型判据 | 本项目现状 | 处理方式 |
| --- | --- | --- | --- |
| **启动层** | `navigator.webdriver`、`--enable-automation`、Headless UA、`plugins.length=0`、`outerWidth=0`、SwiftShader 渲染、`Intl` 时区空、Notification 权限不一致 | 结构性干净（真实浏览器） | 只**审计**，不修（页面脚本改不了） |
| **注入层** | `window.__xxx` 全局、DOM 角标节点、被替换的 `fetch`/`XHR.open`（`toString` 非 native）、跨 world 可见性差异 | 可控 | **隔离 + 隐藏 + 自擦除 + 审计归责** |
| **行为层** | 输入节奏、滚动曲线、焦点切换、`isTrusted` | 部分可控 | 使用 CDP 输入通道产生可信事件；其余靠控制操作频率 |
| **网络/会话层** | TLS/JA3、IP 信誉/ASN、请求头顺序、UA-CH 与 UA 一致性 | 天然真实（真浏览器、真 IP） | 不介入；频繁失败应检查网络出口是否被目标站点限制 |

**关键判断**：绝大多数"被挑战页拦住"的案例，根因在第 1 层（用了 headless/Playwright）或第 4 层（网络出口信誉），而不是第 2 层。投入顺序应当是 **1 → 4 → 2 → 3**。

---

## 2. 识别：多信号打分，而不是布尔判断

**代码落点**：`assets/page_challenge.js` 的 `detect()`、`tokenState()`

单个信号都会误判，所以识别用加权证据：

| 信号 | 说明 | 权重倾向 |
| --- | --- | --- |
| 标题命中 | `TITLE_HINTS`（Just a moment / Attention Required / 安全验证 等） | 中 |
| 挑战 iframe 存在 | `CHALLENGE_FRAME_HINTS`：`challenges.cloudflare.com`、`cdn-cgi/challenge-platform`、`recaptcha/*`、`hcaptcha.com`、`captcha-delivery.com`、`px-captcha`、`awswaf.com`、`arkoselabs.com` | 高 |
| token 字段 | 隐藏的 `cf-turnstile-response` / `g-recaptcha-response` 有值即视为已通过 | 高（一票通过） |
| 硬封禁证据 | `.cf-error-details`、`#cf-error-details`、`[id^="cf-error"]`、页脚 `Cloudflare Ray ID` | 高（判定不可自动处理） |
| 挑战脚本变量 | `window.turnstile`、`_cf_chl_opt`、`__hcaptcha_cfg` | 辅助 |

两个容易出错的地方：

- **普通 403 不是 Cloudflare 硬封禁**。只有出现 CF 专属证据才置 `hard_block:true`；其余返回 `blocked:true, hard_block:false`，并带 `plain_blocked` 诊断位。
- **超时必须 `ok:false`**。返回 `ok:true + cleared:false` 会让调用方以为"等成功了"继续下一步，比超时更危险。现在超时返回 `error:"timeout"` / `error_code:"challenge_wait_timeout"`。

### 2.1 组件可能藏在 closed shadow root 里

托管挑战会把 Turnstile 的 iframe 放进页面的 **closed shadow root**：

```text
DIV#_333e9a51…(display:grid)
  DIV → DIV → DIV
    #document-fragment        ← closed shadow root，页面 JS 进不去
      IFRAME[src=https://challenges.cloudflare.com/cdn-cgi/challenge-platform/…]
      INPUT[type=hidden][name=cf-turnstile-response]
```

页面侧因此会看到一组**自相矛盾**的信号：

| 页面 JS 看到 | 实际情况 |
| --- | --- |
| `document.querySelectorAll('iframe').length === 0` | 有一个 Turnstile iframe |
| `#challenge-stage` 不存在 | 组件正常渲染 |
| `document.elementFromPoint(x, y)` 只返回宿主 DIV | iframe 在其中，页面看不到 |

判据：挑战页 + `window.turnstile` 存在 + 有隐藏 token 字段 + 没有可见 iframe/stage → 组件在 closed shadow root 里。

能"看见"它的只有 CDP：`DOM.getDocument {depth:-1, pierce:true}` 会遍历 shadow root，`Target.getTargets` 也能看到那个 `type:"iframe"` 的 OOPIF 目标。**不要**把「iframe 数为 0」当成「站点没下发组件」，那会误判成 `stalled: turnstile_not_rendered` 并放弃。

---

## 3. 等待：分片注入，而不是一次长等

**代码落点**：`assets/page_challenge.js` 的 `wait` 分支、`src/server/challenge.rs::challenge_value`

| 设计 | 原因 |
| --- | --- |
| 每片只等 4s，由 daemon 反复注入 | 单次长等在页面跳转时会连同执行上下文一起销毁，拿不到结果 |
| 只用 deadline 判终止 | 写死 `injections >= N` 会把用户的 `--timeout 600` 静默截断 |
| `need_human.focusAfterMs` 逐片递减 | 页面侧以"本次注入起点"计时，固定 8000ms 永远到不了 |
| `cleared` 有 250ms 确认窗口 | 避免渲染抖动中被误判为通过 |
| 超时返回 `ok:false` | 见上一节 |

---

## 4. 交互：元素驱动，点一次就不再碰

**代码落点**：`src/server/challenge.rs::challenge_auto_solve`、`src/server/challenge_click.rs`

部分 Turnstile 是显式勾选框，光等不会通过。这时才进入主动点击，规则只有四条，判据全部是**元素或状态**，不是时间：

| 规则 | 判据 | 动作 |
| --- | --- | --- |
| **组件出现** | CDP pierce 能查到挑战 iframe，且宽高大于最小值 | 立即点击，不预热、不等"就绪" |
| **点击被接收** | iframe 消失 / `src` 变化 / 正文出现"正在验证" | 停止一切点击，只等页面结论 |
| **点击被吞** | 3s 内以上都没有 | 允许重点，次数有上限 |
| **通过** | token 字段有值，或已离开挑战页 | 立即返回 |

三个实现细节：

1. **落点用盒模型算，不用固定偏移**。通过 CDP 无障碍树 / 盒模型拿到控件的真实矩形，再取几何中心；不同厂商、不同尺寸、不同缩放都能对上，不需要为每个 vendor 维护偏移表。
2. **用 CDP 输入通道产生事件**。`Input.dispatchMouseEvent` 产生的是 `isTrusted` 事件，与页面里的合成 `dispatchEvent` 不是一回事。项目不做行为建模，也不以"拟人"为目标；按下前的少量 `mouseMoved` 只是为了让事件序列完整（避免"瞬移"导致的控件状态异常）。
3. **`chrome.debugger` 转发 `depth:-1` 的 DOM 树里没有 shadow root**。要用 `getDocument{depth:1}` + `DOM.querySelectorAll('iframe')` 才能稳定命中这个 iframe；自建树遍历在扩展通道下会永远判「组件不存在」。

### 4.1 兜底：把决定权交回调用方

自动求解失败时不硬撑，返回元素数据让上层决定：

```bash
atb challenge widget --tab <tab>        # 组件的 rect / url / click{x,y}
atb challenge click --tab <tab> --at 404,313
atb challenge status --tab <tab>        # 复查 token 是否就绪、是否离开挑战页
```

`challenge widget` 的坐标来自 CDP 盒模型 + 仿射映射（旋转/缩放也能对上），`click --at` 走同一条输入通道。需要人工判断的（图片点选、短信验证）统一返回 `need_human`，由 `challenge focus` 把页面带到前台。

### 4.2 状态机

```text
                    ┌──────────────┐
                    │  navigation  │
                    └──────┬───────┘
                           ↓
              ┌────────────────────────┐
              │ detect（多信号打分）    │
              └────┬───────┬───────┬───┘
        pending    │       │ 无挑战 │ 有挑战
                   ↓       ↓       ↓
              ┌────────┐ ┌──────┐ ┌──────────┐
              │ 继续等 │ │ 放行 │ │ 分类处理 │
              └───┬────┘ └──────┘ └────┬─────┘
                  │                    ├─ 可等待 → wait（分片）
                  │                    ├─ 可点击 → solve（一次）
                  │                    └─ 需人工 → need_human
                  ↓
             deadline 到 → ok:false
```

---

## 5. 痕迹治理：最小注入

### 5.1 世界隔离

**代码落点**：`assets/atb_bridge/background.js` 的 `PAGE_OP_WORLD`

| 通道 | world | 原因 |
| --- | --- | --- |
| `read` / `find` / `snapshot` / `action` / `wait` / `net` | ISOLATED | 页面脚本读不到，互不污染 |
| `challenge` / `stealth` | MAIN | 判定依赖 `window.turnstile` / `_cf_chl_opt`，必须在页面 world |

MAIN world 的代价是入口会存在于页面里，因此入口做了三件事：

1. 定义为**不可枚举**属性：`Object.keys(window)` / `for...in` 看不到；
2. `delete window.__abcChallenge` 返回 `true`，`scrub` 能真正清掉；
3. 挂一个不可枚举的 `__abcOwn` 标记，`pageOp` 只执行带该标记的实现——页面抢先声明同名函数时直接拒绝，不会执行页面提供的实现。

`stealth` 入口同样在 MAIN world，用 `eraseSelfSoon` 把暴露窗口压到 `setTimeout(0)` 级别。

### 5.2 页面角标

`chrome.storage.local.showBadge` 缺省 `false`。开启后使用 closed Shadow DOM + 中性宿主 id `atb-status-host`，站点读不到内部结构。历史版本留下的 `#atb-ind` / `#atb-style` 会被 `scrub` 清理。

### 5.3 尽量减少 CDP attach

Chrome 被 CDP attach 时会显示"已开始调试此浏览器"横幅——这是 Chrome 自己的 UI。本项目的策略是**按命令选择通道**，而不是全局 attach：

| 操作 | 是否 attach |
| --- | --- |
| `open`（未命中挑战页）、`read`、`exec`、`hover`、`select`、`check`、`scroll`、`wait`、`inject`、`cookies` | 否（页面脚本 / 扩展消息） |
| 带 CSS selector 的 `@e` 引用或 `--frame` 触发的 `click` / `fill` | 否（页面内动作） |
| `snapshot --js`、可见区域截图 | 否 |
| `snapshot` 默认、纯 CSS 选择器的 `click` / `fill`、`mouse-click`、`send-keys`、`upload`、`network` / `console` | 是（CDP） |
| 整页截图、元素截图、`save-pdf` | 是；被拒时自动回落到不 attach 的通道 |
| `challenge solve` / `challenge click` | 是（需要 `Input.dispatchMouseEvent`） |

attach 被拒（页面嵌了别的扩展的 iframe，Chrome 报 `Cannot access a chrome-extension:// URL of different extension`）时的回落：

- `snapshot` → DOM 版（`mode:"js"`）
- 可见区域截图 → `captureVisibleTab`
- `screenshot --full-page` → 逐屏滚动 + OffscreenCanvas 拼图（`via:"captureFullPageStitched"`）
- `save-pdf` → 图片型 PDF（`via:"stitched-image-pdf"`，文字不可选中）
- `mouse-click` → 页面内合成事件（`synthetic:true`、`trusted:false`）

设 `ATB_NO_DEBUGGER=1` 可彻底禁止 attach；需要 CDP 的命令会返回替代方案。**代价**：`challenge solve` 的主动点击也会失效，只能走被动等待 + 人工。

### 5.4 审计：评分 + 归责

**代码落点**：`assets/page_stealth.js` 的 `audit()`、`src/server/challenge.rs::stealth_page_call`

审计的价值不在"打个分"，而在**把责任方分清楚**——否则拿到 60 分也不知道该改什么。`findings[].scope` 分三类：

| scope | 含义 | 处理 |
| --- | --- | --- |
| `self` | 本工具写进页面的东西 | `atb stealth scrub` 后复测 |
| `browser` | 浏览器/启动参数层面的标记 | 换成有头真实 Chrome（本项目本来就不该出现） |
| `unknown` | 别的扩展、用户脚本或页面自身 | 逐个排查扩展与注入脚本 |

---

## 6. 参数与配置

### 6.1 打开时处理挑战

```bash
atb open https://example.com                       # 默认 --challenge auto
atb open https://example.com --challenge off       # 完全不检测
atb open https://example.com --challenge wait --challenge-timeout 30
```

| 参数 | 取值 | 默认 | 说明 |
| --- | --- | --- | --- |
| `--challenge` | `auto` / `wait` / `off` | `auto` | 只在标题可疑或为空时探一次 |
| `--challenge-timeout` | 秒 | `15` | 只对 `auto` / `wait` 生效 |

### 6.2 痕迹审计与清理

```bash
atb stealth audit                                  # 默认 15s 超时
atb stealth audit --tab 111 --frame 3 --timeout 20
atb stealth scrub
```

| 参数 | 默认 | 说明 |
| --- | --- | --- |
| `action` | `audit` | `audit`（只读审计）/ `scrub`（清理自身痕迹） |
| `--timeout` | 15 | 页面脚本执行超时（秒） |
| `--tab` / `--frame` / `--browser` / `--profile` | — | 目标选择 |

### 6.3 环境变量

| 变量 | 作用 | 影响 |
| --- | --- | --- |
| `ATB_NO_DEBUGGER=1` | 彻底禁止 CDP attach | 更少横幅；`challenge solve` 主动点击失效 |
| `ATB_IDLE_TTL` | daemon 空闲退出秒数 | 无 |
| `ATB_AGENT` | 智能体身份（会话隔离） | 无 |

---

## 7. 怎么验证

### 7.1 单元层：把脚本放进 `vm` 跑

**代码落点**：`tests/stealth_audit.test.mjs`

不 mock 内部函数，而是造一个"形状像浏览器"的 sandbox，把 `page_stealth.js` 丢进 `vm.runInNewContext` 执行，观察真实回包。sandbox 的几个关键形状缺一样就会误判：

- `window` 与 `globalThis` 必须是同一个对象；
- `canvas.getContext('webgl')` 要能给出 `WEBGL_debug_renderer_info` + `UNMASKED_RENDERER_WEBGL`；
- `fetch` / `XHR.prototype.open` 的"原生"版本用 `bind` 出来的函数（`toString` 恒为 `[native code]`）；
- 断言数组时用 `length` / `join`——结果对象来自另一个 realm，`deepEqual` 会因原型不一致而失败。

覆盖面：干净环境 100 分、入口不可枚举/可删除、`eraseSelfSoon` 真的删掉、本工具痕迹归 `self`、headless 六项归 `browser`、用户脚本归 `unknown`、scrub 顺序、scrub 假干净必须 `ok:false`。

### 7.2 现场层：审计应为 100 分

```bash
atb open https://example.com
atb stealth audit
# 期望：score=100, level=clean, findings=[], extension_ok=true
```

不为 100 时按 `scope` 分流（见 5.4）。

### 7.3 验收矩阵

| 场景 | 命令 | 期望 |
| --- | --- | --- |
| 正常页（无挑战） | `atb open https://example.com` | 不多花往返，无 `challenge` 字段 |
| JS interstitial | `atb challenge wait` | 数秒内 `cleared:true`，`via` 有值 |
| Turnstile 显式勾选 | `atb challenge solve` | `clicks>0` 且 `passed:true` |
| 需要人工的滑块 | `atb challenge solve --no-focus` | `need_human` 早于总超时返回 |
| 硬封禁页 | `atb challenge status` | `blocking:true, auto:false`，提示转人工 |
| 后台标签打开 | `atb open ... --background` | 不抢焦点；必要时先激活再等 |
| 挑战中导航 | `challenge wait` 期间页面跳转 | 不因执行上下文销毁误判失败 |

### 7.4 回归层

```bash
cargo test
node --test tests/stealth_audit.test.mjs
node --test tests/asset_parity.test.mjs
```

`asset_parity` 容易被忽略但很关键：`assets/page_*.js` 和 `assets/atb_bridge/ops/` 是两份拷贝，改了一份没同步，线上跑的还是旧的。

---

## 8. 已知边界

| 边界 | 原因 | 应对 |
| --- | --- | --- |
| **过不了 hard block（1020/1015）** | IP / ASN 级封禁，与浏览器无关 | 换网络出口，或按站点规则申请白名单 |
| **过不了图片点选 / 滑块** | 需要视觉理解与人类操作 | `challenge focus` 转人工 |
| **行为侧、IP 侧不在覆盖范围** | 本项目不模拟人类行为，也不改动网络层 | 控制请求频率；遵守目标站点的使用条款 |
| **后台标签会节流** | Chrome 对后台 tab 的 timer 降频 | 等待前用 `tabs/switch`（`allowFocus:false`）激活 |
| **closed shadow root 内探会失效** | Turnstile 内部结构随版本变化 | 有盒模型兜底；`diag` 里能看到每一步失败原因 |
| **"过了"不等于"进去了"** | 有些站点点完还要等二次跳转 | `solve` 只给一半预算做主动求解，剩下留给被动等待 |
| **审计只覆盖页面这一层** | 站点还有行为侧、IP 侧、账号侧判据 | 审计 100 分不等于不会被风控 |

两条工程约束：

- **`challenge` op 在 MAIN world 执行**，因此入口 `__abcChallenge` 会在页面里存在。它被定义为不可枚举 + 可配置：`Object.keys(window)` 看不到，`delete` 能成功，`stealth audit` 不会把它报成 `own_globals`，`scrub` 也能清掉。扩展低于 5.0.0 时入口是可枚举、不可删除的顶层函数声明，`stealth audit` 会报 `own_globals`。
- **`stealth` 必须在 MAIN world**，因此它自己的入口（`__abcStealth`）会在页面里短暂存在。用 `eraseSelfSoon` 压到 `setTimeout(0)` 级别，已经是能做到的最小暴露窗口。

---

## 9. 代码位置索引

| 能力 | 文件 | 关键位置 |
| --- | --- | --- |
| 挑战识别、等待、token 判定（MAIN world） | `assets/page_challenge.js` | `detect()`、`wait`、`tokenState()`、入口 `__abcChallenge` |
| 挑战 CLI | `src/cli/cmd_action.rs` | `ChallengeArgs`、`open --challenge` 参数透传（`src/cli/cmd_open.rs`） |
| 挑战主流程（status/wait/solve/focus） | `src/server/challenge.rs` | `challenge_value` |
| 主动求解（点击） | `src/server/challenge.rs` | `challenge_auto_solve`（元素驱动）、`challenge_click_point` |
| iframe 几何定位 | `src/server/challenge_click.rs` | `challenge_frames`、`CHALLENGE_FRAME_HINTS`（pierce DOM） |
| 别的扩展挡住 CDP | `src/server/challenge_probe.rs` | `strip_foreign_extension_frames`、`is_foreign_extension_frame_error` |
| iframe 内探控件 | `src/server/challenge_probe.rs` | `collect_iframe_ids`、`iframe_doc_url` |
| 点击通道 | `src/server/challenge_click.rs` | `challenge_click_at`、`click_event` |
| open 后自动等待 | `src/server/challenge_probe.rs` | `should_probe_challenge`、`auto_wait_challenge`；调用点 `src/server/open.rs` |
| 读页后被拦时附挑战信息 | `src/server/challenge_probe.rs` | `attach_challenge_if_blocked` |
| 审计 / 清理 | `assets/page_stealth.js` | `audit()`、`scrub()`、入口挂载 |
| stealth 调用通道（MAIN world） | `src/server/challenge.rs` | `stealth_page_call`、`stealth_value` 的 `extension_ok` |
| 世界选择（ISOLATED/MAIN） | `assets/atb_bridge/background.js` | `PAGE_OP_WORLD`（`challenge: 'MAIN'`） |
| 挑战判定单元测试 | `tests/challenge_detect.test.mjs` | closed shadow root / 未下发 / 普通 widget / 普通页面四种形状 |
| 痕迹审计单元测试 | `tests/stealth_audit.test.mjs` | 全文 |
