# 自动过 Cloudflare 挑战 & 防止站点检测自动化：实现方案

> 本文只描述实现思路与代码落点，**不修改任何代码**。
> 本文记录挑战页识别/求解与痕迹审计的实现、取舍与已知边界。

---

## 0. 一句话结论

「自动过 Cloudflare 挑战」在工程上不是一个"破解"问题，而是一个**判定 + 等待 + 最小交互 + 兜底**的状态机问题；「防检测」的核心不是伪造指纹，而是**别在页面里留下不该留的东西**。

这个项目的架构天然占了很大便宜：它驱动的是**用户自己的真实 Chrome**（扩展 + WebSocket 桥），不是 Playwright/Puppeteer 拉起的新实例。所以 `navigator.webdriver`、`--enable-automation`、`HeadlessChrome` UA、空 plugins、`outerWidth=0` 这一整类最致命的标记，**从源头上就不存在**。真正要处理的只剩两件事：

1. 挑战页要能被**准确识别**，识别后要能**等到它自己放行**，需要点时能**点得像人**；
2. 自己注入的东西（全局变量、DOM 节点、被包装的 `fetch`）要**尽量不写进页面**，写了要能**擦干净**。

---

## 1. 威胁模型：站点到底在看什么

反检测不能凭感觉堆特性，得先列出对手的判据。按从易到难分四层：

| 层 | 判据 | 本项目现状 | 处理方式 |
| --- | --- | --- | --- |
| **启动层** | `navigator.webdriver`、`--enable-automation`、Headless UA、`plugins.length=0`、`outerWidth=0`、SwiftShader 渲染、`Intl` 时区空、Notification 权限不一致 | 结构性干净（真实浏览器） | 只**审计**，不修（页面脚本修不了） |
| **注入层** | `window.__xxx` 全局、DOM 角标节点、被替换的 `fetch`/`XHR.open`（`toString` 非 native）、跨 world 可见性差异 | 可控 | **隔离 + 隐藏 + 自擦除 + 审计归责** |
| **行为层** | 鼠标轨迹熵、点击精度（是否每次都正中中心）、输入节奏、滚动曲线、焦点切换、`isTrusted` | 部分可控 | 点击带轨迹；其余靠"别做太机械"的策略 |
| **网络/会话层** | TLS/JA3 指纹、IP 信誉/ASN、`cf_clearance` 有效期、请求头顺序、UA-CH 与 UA 一致性、Cookie 年龄 | 天然真实（真浏览器真 IP） | 会话预热 + Cookie 复用 |

**关键判断**：绝大多数"被 Cloudflare 拦住"的案例，根因在第 1 层（用了 headless/Playwright）或第 4 层（IP 太脏、没有历史 Cookie），而不是第 2 层。所以反检测的投入顺序应该是 **1 → 4 → 2 → 3**，而不是反过来死磕指纹。

---

## 2.2.0 先记住这条：组件可能藏在 closed shadow root 里（2026-11-03 修正）

这一条推翻了本方案早期的一个前提，必须写在最前面。

CF 的托管挑战（managed challenge）现在把 Turnstile 的 iframe 放进页面里的 **closed shadow root**：

```text
DIV#_333e9a51…(display:grid)
  DIV
    DIV
      DIV
        #document-fragment        ← closed shadow root，页面 JS 进不去
          IFRAME[src=https://challenges.cloudflare.com/cdn-cgi/challenge-platform/…]
          INPUT[type=hidden][name=cf-turnstile-response]
```

页面侧因此会看到一组**自相矛盾**的信号：

| 页面 JS 看到 | 实际情况 |
| --- | --- |
| `document.querySelectorAll('iframe').length === 0` | 有一个 300×65 的 Turnstile iframe |
| `#challenge-stage` 不存在 | 组件正常渲染，勾选框就在那儿 |
| `document.elementFromPoint(x, y)` 只返回宿主 DIV（896×68） | iframe 在其中，页面看不到 |
| `_cf_chl_opt.cType === 'interactive'` 只在首帧有效 | 编排脚本跑完后键名被混淆成随机串，稳态读不到 |

> 判据：**CF 挑战页 + `window.turnstile` 存在 + 页面上有隐藏的 `cf-turnstile-response` 字段 + 没有可见 iframe/stage** → 组件在 closed shadow root 里，`interactive:true`，必须点击。
>
> 早期实现把「iframe 数为 0」当成「站点没下发组件」，于是判 `stalled: turnstile_not_rendered` 并放弃——这是错的。同一台机器上，页面截图里勾选框明明就在那儿。

真正能"看见"它的只有 CDP：`DOM.getDocument {depth:-1, pierce:true}` 会遍历 shadow root，`Chrome://inspect` 的 `Target.getTargets` 也能看到那个 `type:"iframe"` 的 OOPIF 目标。



## 2. 自动过 Cloudflare 挑战

### 2.1 先分类：哪些能自动过，哪些不自动处理

这一步做错，后面全是无用功。Cloudflare 的拦截产品线差异很大：

| 类型 | 典型特征 | 能否自动 | 策略 |
| --- | --- | --- | --- |
| **JS Interstitial**（`Just a moment...`） | 标题 + `#cf-challenge-running` / `#challenge-stage`，`cdn-cgi/challenge-platform` 脚本 | ✅ 通常 3–8 秒自动放行 | 只需**等待**，不要乱点 |
| **Managed Challenge（隐形 Turnstile）** | 同上，但内部跑 Turnstile 打分 | ⚠️ 多数能，取决于浏览器环境评分 | 等待为主；环境干净基本能过 |
| **Turnstile 显式勾选框** | `challenges.cloudflare.com` iframe，300×65 | ✅ 能 | 等待 + **带轨迹点击** |
| **图片点选 / 滑块**（reCAPTCHA 图片、阿里、极验、腾讯） | 需要视觉理解 | 尚未实现（未来工作） | 当前不自动处理；`challenge focus` 把标签带到前台，由使用者在页面上自行完成 |
| **Hard Block**（1020 / 1015 / `Access denied`） | 纯文本拦截页，无 JS | 尚未实现（未来工作） | IP 或 ASN 级封禁，与浏览器无关，页面侧无解；换网络/换号 |
| **需要手机/邮箱验证的 Managed Challenge** | 表单 | 尚未实现（未来工作） | 当前不自动处理；`challenge focus` 把标签带到前台，由使用者在页面上自行完成 |

所以实现上必须有 `auto`（可等）/ `interactive`（需交互）/ `blocking`（阻断）三个正交标志位，而不是一个 `detected: true/false`。

### 2.2 检测：多信号打分，而不是布尔判断

**代码落点**：`assets/page_challenge.js`（全部，**MAIN world**）；调用方 `src/server/challenge.rs::challenge_page_call`；探测入口在 `src/server/challenge_probe.rs`（`should_probe_challenge` / `attach_challenge_if_blocked`）

核心是 `detect()` 函数（`page_challenge.js:157`）。它按"证据强度"打分，取最高分作为结论：

```js
function hit(id, score, nextKind, isBlocking, isInteractive, isAuto, why) {
  if (score <= best) return;   // 只认最强证据，避免多信号互相污染
  best = score; vendor = id; kind = nextKind;
  blocking = isBlocking; interactive = isInteractive; auto = isAuto;
  signals.push(why);
}
```

信号与权重（按实际代码）：

| 分 | 信号 | 说明 |
| --- | --- | --- |
| 12 | 标题正则 | `just a moment` / `attention required` / `checking your browser` / `请稍候` / `正在验证` |
| 11 | CF 挑战 DOM | `#cf-challenge-running`、`#challenge-form`、`#challenge-stage`、`#cf-please-wait` |
| 10 | 整页风控 | PerimeterX（`#px-captcha`）、Akamai（`#sec-if-cpt-container`） |
| 9 | CF 脚本 / 全局 | `script[src*=cdn-cgi/challenge-platform]`、`window._cf_chl_opt` |
| 9 | 滑块类 | 阿里 `#nc_1_n1z`、极验 `#gt_slider`、腾讯 `#tcaptcha_transform_dy`、顶象、数美 |
| 8 | Turnstile 组件 | `.cf-turnstile` |
| 7 | reCAPTCHA / hCaptcha | `.g-recaptcha` / `.h-captcha` |
| 6 | 正文兜底 | `verify you are human`、`确认您是真人` |
| 5 / 4 | 通用兜底 | `img[src*=captcha]` / `input[name*=captcha]` |

三个容易被忽略但必须实现的细节：

**(1) `pending` 三态，而不是二态。**
导航刚提交时页面是空白文档——既没有挑战标记，也不代表已放行。只判 `detected` 会把"还没加载"当成"已通过"：

```js
const pending = document.readyState === 'loading';
```

`wait` 循环里遇到 `pending` 必须 `clearSince = 0` 继续等（`page_challenge.js:360`）。

**(2) `token_ready` 是最硬的通过信号。**
各家验证码完成后都会往隐藏字段写令牌，这比"iframe 消失了"可靠得多：

```js
const TOKEN_SELECTORS = [
  '[name="cf-turnstile-response"]', '[name="g-recaptcha-response"]',
  '[name="h-captcha-response"]', '#g-recaptcha-response',
  'textarea[name="g-recaptcha-response"]', 'textarea[name="h-captcha-response"]'
];
```

再加一条通用兜底：任意 `input[type=hidden]`，名字同时含 `captcha|turnstile|recaptcha|hcaptcha` 与 `token|response|answer`，且 `value.length > 0`。

**(3) 正文兜底必须限代价。**
最初的写法是 `document.body.textContent.slice(0, 2000)`——切片发生在**整页序列化之后**。26 万节点的页面上，250ms 一次的轮询等于每秒把整棵树拼成字符串，页面直接卡死。正确做法是 `TreeWalker` 边走边停 + 三重门控（`page_challenge.js:121-155`）：

```js
const TEXT_SCAN_LIMIT = 2000, TEXT_SCAN_INTERVAL_MS = 1000, TEXT_SCAN_MAX_NODES = 20000;
let bodyTextMatched = false;   // 命中后粘性：否则被节流跳过的那次会把拦截页误判为已放行
```

### 2.3 等待：分片注入，而不是一次长等

**代码落点**：`src/server/challenge.rs::challenge_value`（`wait` 分支，分片循环在 `challenge_wait_result`）、`assets/page_challenge.js`（页面侧 `wait`）

不能"注入一次，页面里 `setInterval` 等 120 秒"。原因有三个：扩展消息有超时；通过挑战后页面会**导航**，执行上下文直接销毁；后台标签的定时器会被 Chrome 节流。

所以 daemon 侧做成**分片轮询**（`challenge_wait_result`）：

```
每次注入最多 4s  →  页面内 250ms 轮询 + MutationObserver  →  返回 cleared/need_human/pending
   ↑                                                                    ↓
   └──────────────── 未清除且未到 deadline 就再注入一片 ←────────────────┘
```

几个必须踩对的点：

- **只用 deadline 判终止**。旧实现写死 `injections >= 80`（=320s），把 `--timeout 600` 静默截断成 320s，而且超时后既不报错也不说明原因。
- **`need_human` 的 `focusAfterMs` 必须递减**。页面侧计时的基准是"本次注入的起点"，而单片只等 4s；固定下发 8000ms 时 `at - started` 永远到不了，`need_human` 从不返回，需要人工的页面会一路静默等到总超时。
- **`cleared` 要有确认窗口**。`confirmMs = 250ms`，避免"渲染抖动中短暂看不到标记"被误判为通过。
- **超时必须 `ok:false`**。以前返回 `ok:true + cleared:false`，CLI 退出码 0，智能体以为等成功就继续下一步——这比超时更危险。

### 2.4 主动求解：几何定位 + 可信点击

**代码落点**：`src/server/challenge.rs::challenge_auto_solve`、`src/server/challenge_click.rs::challenge_frames` / `challenge_click_at`、`src/server/challenge_probe.rs::collect_iframe_ids`（勾选框探测）

有些 Turnstile 是显式勾选框，**光等永远过不去**。这时要走主动点击。

#### 第一步：找到验证码 iframe 的屏幕几何

iframe 内部是跨域文档 + closed shadow root，页面侧 `querySelector` 看不到勾选框。必须走 CDP 三级调用（`challenge_frames`）：

```
Page.getFrameTree          → 遍历 frameTree，URL 命中 CHALLENGE_FRAME_HINTS 的 frame
DOM.getFrameOwner          → backendNodeId
DOM.getBoxModel            → model.content 四角 quad
                           → width/height 由 quad 反算（已含 CSS transform）
```

```rust
const CHALLENGE_FRAME_HINTS: [&str; 9] = [
    "challenges.cloudflare.com", "cdn-cgi/challenge-platform",
    "recaptcha/api2", "recaptcha/enterprise", "hcaptcha.com",
    "captcha-delivery.com", "px-captcha", "awswaf.com", "arkoselabs.com",
];
```

注意 `DOM.getFrameOwner` 在部分 Chrome 版本上要求 `DOM` 域已启用，所以前面要先空跑一次 `DOM.enable`（对页面不可见）。

#### 第二步：在 iframe 内部找勾选框

```rust
Page.createIsolatedWorld { frameId, worldName: "atb_challenge_probe", grantUniveralAccess: true }
Runtime.evaluate { contextId, expression: PROBE }
```

`PROBE` 在**该 frame 自己的 document** 里做 BFS 遍历 shadow root，找 `input[type="checkbox"], .cb-lb, #challenge-stage, label[for]` 的中心点：

```js
const stack = [document];
while (stack.length) {
  const root = stack.pop();
  const el = root.querySelector('input[type="checkbox"], .cb-lb, #challenge-stage, label[for]');
  if (el) { const r = el.getBoundingClientRect(); if (r.width > 0 && r.height > 0) return {x: r.x + r.width/2, y: r.y + r.height/2}; }
  for (const node of root.querySelectorAll('*')) if (node.shadowRoot) stack.push(node.shadowRoot);
}
```

拿不到就退回几何兜底（`challenge_auto_solve:7426`）：

- 紧凑型 Turnstile（`h <= 96`，典型 300×65）：勾选框在约 `(30, h/2)`
- 整页挑战 iframe：组件居中，勾选框在 `((w-300)/2 + 30, (h-65)/2 + 32)`，再补中心点与左侧点

#### 第三步：带轨迹的可信点击

**不要一步跳到坐标按下**。Turnstile 会统计 `mousemove` 的熵与时间分布，"瞬移 + 点击"的命中率明显更低（`challenge_click_at:7332`）：

```rust
const STEPS: u32 = 5;
for step in 1..=STEPS {
    let t = step / STEPS;
    let eased = 1.0 - (1.0 - t).powi(2);           // ease-out：接近目标时减速
    let px = start_x + (x - start_x) * eased + (t * 3.0).sin() * 1.5;  // 轻微抖动
    let py = start_y + (y - start_y) * eased + (t * 2.0).cos() * 1.2;
    cdp_call(..., "Input.dispatchMouseEvent", json!({ "type": "mouseMoved", "x": px, "y": py, ... }))
}
// 最后 mousePressed / mouseReleased，button=left, clickCount=1, pointerType=mouse
```

起点取 `(x-90, y-60)`，模拟从右下方向目标靠近的手部轨迹。

#### 第四步：点完看「元素证据」，而不是等时间

点下去之后 Turnstile 会跑一段 PoW（主线程能被卡 5–10 秒），然后三种结局之一：

| 元素证据 | 含义 | 动作 |
| --- | --- | --- |
| `token_ready:true`（`cf-turnstile-response` 有值） | 通过 | 立即返回 `solved` |
| `detected:false`（离开挑战页） | 通过（跳转完成） | 立即返回 `solved` |
| shadow root 里的 iframe 消失 | 点击被接收，正在跳转 | **只等，绝不再点** |
| 正文出现「正在验证您是否是真人／Verifying you are human」 | 点击被接收 | **只等，绝不再点** |
| 以上都没有，组件原样还在 | 那一下没点中 / 组件还没就绪 | 5s 后允许再点一次（最多 2 次） |

旧实现的两个反面教训都在这里：

- **不能按固定秒数等**：`sleep(12s)` 既可能等太久（页面早就跳了），也可能在「马上要出 token」时提前换点。
- **验证中途再点一下会把流程打回原点**：连点三次的实测结果是 24s 全白跑。所以「正在验证」必须被识别成证据，一旦出现就锁住点击。

#### 预算分配

```
solve:  主动求解预算 = min(总超时 × 50%, 30s)，剩余时间留给被动等待兜底
auto / click: 一次性动作，预算 = 全部超时，结果立刻返回（供上层判断）
```

为什么要留一半：有些站点点完之后还要等它自己跳转，主动求解成功不代表页面已经进去。

### 2.4.5 重写（2026-11-03 晚）：元素驱动，点一次就不再碰

上一节的「第二步/第三步」用的是固定偏移 + 固定节奏，实测命中率极低，已整体替换。
现在的实现只有四条规则，全部可观测：

| 规则 | 判据（全部是元素/状态，不是时间） | 动作 |
| --- | --- | --- |
| **组件出现** | CDP `pierce` 能查到 `challenges.cloudflare.com` 的 iframe，且宽高 > 100×24 | 立刻点，不预热、不等「就绪」 |
| **点击被接收** | iframe 元素消失 / `src` 换成新的 / 正文变「正在验证」 | 停止一切点击，只等页面结论 |
| **点击被吞** | 3s 内以上都没有 | 允许重点（最多 6 次；被吞的点击不留状态） |
| **通过** | `cf-turnstile-response` 有值 或 已离开挑战页 | 立刻返回 |

三个实测出来的关键细节（都踩过坑）：

1. **落点是勾选框方块，不是那行文案。** 用 CDP 无障碍树读到 `checkbox[请验证您是真人]`
   的盒子是 `9,21,130x24`，真正的方块在 `(9,21)-(33,45)`，中心 `(21,33)` —— 即宽度的 7%。
   之前按 `(30, h/2)` 点，压在 label 上：label 有时转发、有时直接吞掉。
2. **鼠标必须从组件外面滑进来。** 起点放在组件内部时，页面照样进「正在验证」，
   但 Turnstile 服务端始终不给结果；从左侧 40px 外进入（触发 mouseenter）才过。
3. **`chrome.debugger` 转发 `depth:-1` 的 DOM 树里没有 shadow root。** 只有
   `getDocument{depth:1}` + `DOM.querySelectorAll('iframe')` 才稳定命中这个 iframe；
   自建树遍历在扩展通道下会永远判「组件不存在」。

#### 兜底：让智能体自己点（不转人工）

自动求解失败时，调用方可以两步自己完成，全部基于元素数据：

```bash
atb challenge widget --tab <tab>        # 返回组件的 rect / url / click{x,y}（含 local 坐标）
atb challenge click --tab <tab> --at 404,313
atb challenge status --tab <tab>        # 复查 token_ready / 是否离开挑战页
```

`challenge widget` 的坐标来自 CDP 盒模型 + 仿射映射（旋转/缩放也能对上），
`click --at` 走的是同一条可信鼠标事件通道，不是页面里的合成点击。

### 2.4.6 最关键的一条：**不要往挑战页里写多余的东西**

这一条是整轮排查的终点，也是「人工能过、脚本过不去」的真正原因。

挑战页是 CF 全程监控的页面，页面的 MAIN world 里跑着它的编排脚本。旧实现为了识别
挑战，用 `eval` 把 `page_challenge.js` 注入 MAIN world，脚本里有一行：

```js
window.__abcChlSeenAt = Date.now();   // 记录「第一次判定为拦截」的时刻
```

于是挑战一开始，页面上凭空多了一个全局变量。点击照样能被接收（页面照样显示
「正在验证您是否是真人」），但 Turnstile 服务端永远不给 token、也不跳转。

对照实验（同一台机器、同一个浏览器 profile、同一个点击坐标）：

| 页面状态 | 结果 |
| --- | --- |
| 不注入额外全局，直接 CDP 点击 | **1.4s 通过** |
| 注入过 `window.__abcChlSeenAt` 后再点击 | 进「正在验证」，永不返回 |

**为什么 `challenge` 仍在 MAIN world**：判定必须读站点自己挂在页面 `window` 上的
全局——`window.turnstile`、`window._cf_chl_opt`、hCaptcha 的 `__hcaptcha_cfg`。
`PAGE_OP_WORLD.challenge = 'MAIN'` 是有意为之：ISOLATED world 有独立的 `window`，
读这些全局永远是 `undefined`，回退路径会稳定报「没有挑战」。所以这里的目标不是
「完全不可见」，而是「只留一个必须留的入口，且它不构成可枚举/可删除的痕迹」。

**修法**（已实现）：

1. `page_challenge.js` 整文件包进 IIFE，**不再使用顶层 `function __abcChallenge(){}`**
   ——classic script 里的顶层函数声明会创建不可删除（`DONT_DELETE`）且可枚举的全局绑定。
2. 入口改用
   `Object.defineProperty(window, '__abcChallenge', { value, writable:true, enumerable:false, configurable:true })`：
   `Object.keys(window)` / `for...in` 看不到，`delete window.__abcChallenge` 返回 `true`，
   `stealth scrub` 不会永远把 `remaining` 报成非空。
3. 入口同时挂一个不可枚举的 `__abcOwn` 标记；`pageOp` 只执行带该标记的实现，
   页面抢先声明同名 `__abcChallenge` 时直接拒绝，不会执行页面提供的实现。
4. 脚本里**不再写任何其他 `window.*` 全局**，时间戳改由 daemon 侧计；
5. 识别判据优先用 DOM（隐藏的 `cf-turnstile-response` 字段 + 无可见 iframe/stage），
   页面全局只作为辅助证据，不再单点依赖。
6. `wait` 的 `ok` 跟 `cleared` 走：超时 / `need_human` 回 `ok:false`（`error:"timeout"` /
   `error_code:"challenge_wait_timeout"`），不再是「`ok:true` + `cleared:false`」；daemon 仍按
   `cleared` 判定，并保留 `passed` 语义。
7. `hard_block` 只在 **CF 专属证据**成立时为 true（`.cf-error-details` / `#cf-error-details` /
   `[id^="cf-error"]`，或页脚 `Cloudflare Ray ID`）；普通 403 是 `blocked:true, hard_block:false`，
   另有 `plain_blocked` 诊断位，不再被当成 CF 硬拦截。

修完的实测（`v2ray.v2ray66.top/files`，每次都先清 CF cookie）：

```
run1 cleared:true 6.7s   run2 cleared:true   run3 cleared:true 6.6s   run4 cleared:true
早一轮：cleared:true 7.5s / 12.9s / 8.3s，失败 1 次（站点偶发）
nowsecure.nl 回归 cleared:true 4.7s；example.com 不误判
```

这条原则值得写进任何「自动化 + 被监控页面」的项目里：**你能读它，它也能读你。**
注入进去的每一个全局、每一个函数、每一次 eval，都是对方指纹的一部分。

### 2.5 完整状态机

```
                    ┌──────────────┐
                    │  navigation  │
                    └──────┬───────┘
                           ↓
              ┌────────────────────────┐
              │ detect（多信号打分）    │
              └────┬───────┬───────┬───┘
        pending    │       │ 无挑战 │ 有挑战
        继续等 ←───┘       ↓       ↓
                    ┌──────────┐  ┌──────────────────────┐
                    │ cleared  │  │ blocking? / interactive? │
                    └──────────┘  └───┬──────────────┬───┘
                                      │ 可等(auto)    │ 需交互
                                      ↓              ↓
                              ┌─────────────┐  ┌──────────────────┐
                              │ 分片 wait   │  │ auto_solve(点击) │
                              │ 250ms 轮询  │  │ 几何定位+轨迹点击 │
                              └──┬───────┬──┘  └────┬─────────┬───┘
                           token │       │ 超时      │ 成功     │ 失败
                                 ↓       ↓           ↓         ↓
                            ┌────────┐ ┌─────────┐ ┌──────┐ ┌──────────┐
                            │ passed │ │ reload  │ │passed│ │ need_human│
                            └────────┘ │ 一次?   │ └──────┘ │ focus 前台 │
                                       └────┬────┘          └─────┬─────┘
                                            │ 仍失败              │ 人完成后
                                            ↓                     ↓
                                     ┌────────────────┐    （回到 wait）
                                     │ ok:false       │
                                     │ challenge_timeout │
                                     └────────────────┘
```

### 2.6 `open` 时的自动路径（默认 `--challenge auto`）

**代码落点**：`src/server/open.rs`（`/open` 主流程）、`src/server/challenge_probe.rs::should_probe_challenge` / `auto_wait_challenge`

不是每次 `open` 都去问一遍"你是不是挑战页"——那要多一次页面往返，高频操作下成本明显。先用**最便宜的判据**过滤：

```rust
fn should_probe_challenge(url: &str, title: &str) -> bool {
    if !(url.starts_with("http://") || url.starts_with("https://")) { return false; }
    let title = title.trim().to_lowercase();
    if title.is_empty() { return true; }   // 挑战页在响应头到达前就是空标题，值得探一次
    const HINTS: [&str; 11] = ["just a moment", "attention required", "checking your browser",
        "verifying you are human", "one more step", "请稍候", "正在检查", "正在验证",
        "安全验证", "人机验证", "access denied"];
    HINTS.iter().any(|hint| title.contains(hint))
}
```

命中后的完成判定有个坑（`auto_wait_challenge`）：

```rust
// 「没被拦」不等于「已经放行」：页面挂着一个没勾的 Turnstile 时 blocking=false，
// 但提交前必须过它。只看 blocking 会在这种页面上返回 cleared:true，
// 调用方以为进去了，后续命令全打在挑战页上。
if !blocking && (!interactive || token_ready) { return cleared: true }
```

另外：后台标签的定时器会被节流，挑战脚本跑不完。所以等待前要用 `tabs/switch` 把标签**激活**（`allowFocus: false` 不抢系统焦点）——但 `--background` 打开时保持后台语义，不做这一步。

---

## 3. 防止站点检测到自动化

### 3.1 设计原则

| 做法 | 本项目态度 | 理由 |
| --- | --- | --- |
| 改 `canvas` / `WebGL` 输出加噪声 | 尚未实现（未来工作） | 且伪造本身会与真实 GPU / 字体 / 时区产生一致性硬伤 |
| 改 UA / 硬件参数 | 尚未实现（未来工作） | 与 Client Hints、`navigator.platform`、屏幕尺寸、字体列表必须成套自洽，单改一项就是硬伤 |
| 减少自身注入到页面 | ✅ 核心 | 这是唯一"只减不加"的操作，零副作用 |
| 擦除已写入的痕迹 | ✅ 核心 | 同上，且可验证 |
| 不附加调试器 | ✅ 结构性优势 | CDP attach 有横幅、有可检测面 |
| 点击带人类轨迹 | ✅ 做 | 只用于验证码组件的点击轨迹，不做全站行为模拟 |

> 本表的「尚未实现（未来工作）」为当前发布状态；「保留接口」只表示后续版本可能以显式配置提供，不构成承诺。


### 3.2 最重要的一条：世界隔离

**代码落点**：`assets/atb_bridge/background.js` 的 `PAGE_OP_WORLD`

Chrome 扩展的 `chrome.scripting.executeScript` 可以指定 `world: 'ISOLATED' | 'MAIN'`。这是整个反检测方案里**性价比最高**的一招：

| world | 站点可见性 | 适用 |
| --- | --- | --- |
| `ISOLATED` | 扩展私有世界，`window.__abcRead` 对站点**完全不可见** | 纯读/操作类 |
| `MAIN` | 就是页面自己的 `window`，写什么站点都能读到 | 必须碰页面原生对象 |

```js
const PAGE_OP_WORLD = {
  read: 'ISOLATED', find: 'ISOLATED', snapshot: 'ISOLATED',
  action: 'ISOLATED', wait: 'ISOLATED',
  challenge: 'MAIN',  // 判定要读 window.turnstile / _cf_chl_opt / __hcaptcha_cfg
  net: 'MAIN',        // 要包装页面自己的 fetch / XHR.prototype.open
  stealth: 'MAIN'     // 审计的正是"站点视角"，在 ISOLATED 里看不到自己在 MAIN 的残留
};
```

**只要把默认值从 MAIN 改成 ISOLATED，一类指纹就整类消失了**——而且不需要任何伪装。这是 5.0.0 最大的一项改进。`challenge` 是唯一保留在 MAIN world 的读/操作类脚本，原因见 §2.4.6：它的判定依赖站点全局，进 ISOLATED 就永远读不到；代价用「入口不可枚举 + 可配置」抵消（§3.3）。


### 3.3 入口不可枚举 + 自擦除

**代码落点**：`assets/page_stealth.js:436`、`page_net.js` 同款

如果在扩展注入的 classic script 里写顶层 `function __abcStealth(){}`，会创建 **non-configurable** 的全局绑定（V8 的 `DONT_DELETE` 位）：

- `delete window.__abcStealth` 恒返回 `false` —— 审计函数自己成了**永远擦不掉的指纹**；
- 函数声明是**可枚举**的，`Object.keys(window)` 一行就能列出来。

正确写法（`page_stealth.js:436`）：

```js
Object.defineProperty(window, '__abcStealth', {
  value: __abcStealth,
  writable: true,
  enumerable: false,    // Object.keys / for...in 看不到
  configurable: true    // 才删得掉
});
```

再配一个自擦除：

```js
function eraseSelfSoon() {
  setTimeout(function () { delete window.__abcStealth; }, 0);
}
```

下次调用由扩展重新注入，代价是几毫秒的 `executeScript`。

这里有个反直觉的点：入口**仍然必须挂在 `window` 上**（daemon 侧是裸标识符 `return __abcStealth({...})`，靠全局属性解析），只是要让它不可枚举、可删除、用完即走。`challenge` 的 `__abcChallenge` 用的是同一套挂载方式（不做自擦除，因为它在 `wait` 期间要被反复调用）。

### 3.4 网络钩子隐藏

**代码落点**：`assets/page_net.js`（5.0 起）、审计侧 `page_stealth.js:69-116`

`net` 要包装页面的 `fetch` / `XMLHttpRequest.prototype.open`，这是必须写进 MAIN world 的"必要之恶"。降低可见性的三招：

1. **状态槽隐藏**：不再写 `window.__abcNetState`，改成 `Object.defineProperty` 挂 `window['__atb_net_<8位随机>']`，`enumerable: false`。
2. **`toString` 伪装 + `name` / `length` 对齐**：包装函数让 `Function.prototype.toString.call(fn)` 仍返回 `[native code]` 形态。
3. **审计侧三检**，避免误判与误删：

```js
const STATE_SLOT = /^__atb_net_[a-z0-9]{8}$/;   // 必须加锚点
// 名字形状 + 不可枚举 + 值结构（installed:boolean / seq:number / log[] / routes[]）三检全中才认
```

为什么必须三检：页面随手写个 `__atb_net_fake`，只按前缀匹配的实现会把它当成本工具槽位——`audit` 误报 `own_net_hook`；`scrub` 更糟，会**删掉页面的属性**还报 `restored: ['state-only']`（毁数据）。

还有一条容易踩的：**读 `window[name]` 会触发页面的 getter**。页面可以放一个"求值即计数"的 getter，我们每跑一次审计就在帮他标记自动化。正确读法是只看属性描述符的 `value` 槽（`dataValue`，`page_stealth.js:94`），访问器一律当"不存在"。

### 3.5 页面角标：默认关闭 + closed shadow DOM

`chrome.storage.local.showBadge` 缺省 `false`，只有 popup 里能开。开启后也是 closed Shadow DOM + 中性宿主 id `atb-status-host`，站点读不到内部结构。历史版本留下的 `#atb-ind` / `#atb-style` 会被 `scrub` 清理。

### 3.6 不附加调试器（结构性优势）

**代码落点**：`README.md:190`、`ATB_NO_DEBUGGER`

Chrome 被 CDP attach 时会弹"已开始调试此浏览器"横幅，这一条本身就是极强的自动化信号（部分检测直接读这个状态）。

本项目的分工：

| 操作 | 是否 attach |
| --- | --- |
| `open`（未命中挑战页）、`read`、`exec`、`hover`、`select`、`check`、`scroll`、`wait`、`inject`、`cookies` | ❌ 不 attach（走页面脚本 / 扩展消息） |
| 带 CSS selector 的 `@e` 引用或 `--frame` 触发的 `click` / `fill` | ❌ 不 attach（页面内动作） |
| `snapshot --js` | ❌ 不 attach（DOM 快照） |
| 可见区域截图 | ❌ 不 attach（`chrome.tabs.captureVisibleTab`，扩展 4.0+） |
| `snapshot` 默认、纯 CSS 选择器的 `click` / `fill`、`mouse-click`、`send-keys`、`upload`、`network` / `console` | ✅ attach（CDP） |
| 整页截图、元素截图、`save-pdf` | ✅ attach；**被拒时自动回落**到不 attach 的通道（见下） |
| `net mock --engine cdp`、显式 CDP | ✅ attach |
| `challenge auto_solve` / `challenge click` 点击 | ✅ attach（需要 `Input.dispatchMouseEvent`） |

**attach 被拒时**（页面嵌了别的扩展的 iframe，Chrome 报 `Cannot access a chrome-extension:// URL of different extension`）：
`snapshot` → DOM 版（`mode:"js"`）；可见区域截图 → `captureVisibleTab`；`screenshot --full-page` → 逐屏滚动 + OffscreenCanvas 拼图（`via:"captureFullPageStitched"`）；
`save-pdf` → 图片型 PDF（`via:"stitched-image-pdf"`，文字不可选中）；`mouse-click` → 页面内合成事件（`synthetic:true`、`trusted:false`）。
这几条通道都不产生调试横幅。仍需要调试器的只剩 `network` / `console` / `upload` / `send-keys` / 元素截图 / 后台标签页截图。

设 `ATB_NO_DEBUGGER=1` 可彻底禁止 attach，需要 CDP 的命令会返回替代方案。**注意**：这意味着 `challenge solve` 的主动点击也会失效，只能走被动等待 + 人工。这是一个明确的取舍，要在使用时知道。

### 3.6.1 把横幅彻底关掉：`--silent-debugger-extension-api`

上面那张表讲的是「尽量减少 attach」。但有些命令**必须** attach（挑战点击要用
`Input.dispatchMouseEvent`），而横幅是 Chrome 自己的 UI，运行时没有 API 能摘。

唯一开关是启动参数 `--silent-debugger-extension-api`（Chromium 里叫
`kSilentDebuggerExtensionAPI`）。Chrome 156 仍支持——二进制里能查到该字符串，
实测也有效：

| 启动方式 | 同一次 `chrome.debugger.attach` 后窗口顶部 |
| --- | --- |
| 普通启动 | 出现「「ATB」已开始调试此浏览器 [取消]」 |
| 加 `--silent-debugger-extension-api` | **没有信息条**，页面内容不被挤压 |

判据不能靠 `window.innerHeight`：实测两种情况下 innerHeight 都是 707，横幅是画在
内容区之上的一层，不改变视口尺寸。只能看窗口截图。

因为参数只在进程启动时读，工具里做成了 `atb launch`：

```bash
atb launch --dry-run     # 输出 args / would_close，不碰进程
atb launch --restart     # 温和关闭同 profile 的实例，带参数重开（默认恢复标签页）
```

两个容易踩的点，都已处理：

1. **不给 `--restart` 就绝不关窗口。** 已有实例在跑时，再启动一次只会把 URL 转发
   过去、命令行参数静默失效——所以直接返回 `browser_already_running`，
   免得用户以为「加了参数就好了」。
2. **浏览器必须由 daemon 拉起。** 由一次性命令 spawn 时，很多运行环境（含智能体框架）
   会把命令放进带「关闭即杀」的作业对象，命令一结束整棵进程树被回收，浏览器跟着消失。
   即使加了 `CREATE_BREAKAWAY_FROM_JOB` 也未必能脱离。放到 daemon 里 spawn 才是
   稳定的（daemon 本身就是脱离作业对象启动的）。

### 3.7 审计：评分 + 归责

**代码落点**：`assets/page_stealth.js` 的 `audit()`、`src/server/challenge.rs::stealth_page_call`

审计的价值不在"打个分"，而在**把责任方分清楚**——否则拿到 60 分也不知道该改什么。

评分：`score = 100 - Σ(high:25 / medium:10 / low:4)`，`level = clean(≥90) / low(≥70) / medium(≥40) / high`。

`scope` 三分：

| scope | 含义 | 例子 | 能否就地修 |
| --- | --- | --- | --- |
| `self` | 本工具留下的 | `own_globals`、`own_dom_badge`、`own_net_hook` | ✅ `stealth scrub` |
| `browser` | 浏览器启动方式造成 | `navigator.webdriver`、`HeadlessChrome` UA、plugins 为空、`outerWidth=0`、SwiftShader、时区空 | ❌ 页面脚本改不了 |
| `unknown` | 别人的痕迹 | `__playwright` / `__puppeteer` / `cdc_` / `_phantom`、非本工具改写的 `fetch`、`chrome.runtime` 暴露 | ❌ 得去掉对应框架 |

`self_fixable` = `scope:"self"` 的条数，直接告诉调用方"这一堆里有几条是 scrub 能解决的"。

还有一个版本门禁：`extension_ok`（`src/server/challenge.rs` 的 `stealth_value`）。扩展低于 `5.0.0` 时，审计可能显示"干净"但页面操作仍在往 MAIN world 写全局——**结论不可信**，必须先覆盖扩展目录 + 在 `chrome://extensions` 点「重新加载」（覆盖文件 ≠ 重载）。

### 3.8 scrub：顺序不能反，且不能"假干净"

**代码落点**：`assets/page_stealth.js:367 scrub`

固定顺序（`page_stealth.js:372-401`）：

```
1. 调 __abcNet({action:'reset'})   → 把 fetch / XHR.open 还原成原生
2. delete 注入的全局（OWN_GLOBALS）
3. remove DOM 角标节点
4. 复查 remaining（含随机的网络状态槽）
```

**为什么必须先 reset 再删全局**：`reset` 需要 `__abcNet` 这个入口还活着。反过来做，删了 `__abcNet` 之后包装还在页面上，反而更糟——还原能力丢了，痕迹还在。

**为什么要复查**：网络状态槽的名字是随机的，不在 `OWN_GLOBALS` 里。只删 `__abcNet` 不删槽位的话，页面照样能用 `getOwnPropertyNames` 看到它。这时报 `ok:true` 就是**假干净**。所以：

```js
return {
  ok: leftovers.length === 0 && leftoverNodes.length === 0,
  ...
  note: '仍有痕迹未清除（可能被页面以不可配置属性固定），刷新页面即可恢复干净状态'
};
```

daemon 侧再把 `ok:false` 压到外层（`src/server/challenge.rs` 的 `stealth_value`），CLI 才会给非 0 退出码。

**什么时候该跑 scrub**：确认页面里还残留本工具注入的东西，且这个页面接下来要给站点看（提交表单前、风控校验前）。
**什么时候不该跑**：日常读页面不必跑——`scrub` 会把还在用的 `net hook` 一起停掉，抓包会断。仍要抓包又不想碰页面函数时，用 `net mock --engine cdp`。

---

## 4. 参数与配置项（全部可用，无硬编码默认值覆盖用户意图）

### 4.1 打开时自动处理挑战

```bash
atb open https://example.com                       # 默认 --challenge auto
atb open https://example.com --challenge off       # 完全不检测（最快，但可能落在拦截页）
atb open https://example.com --challenge wait --challenge-timeout 30
```

| 参数 | 取值 | 默认 | 说明 |
| --- | --- | --- | --- |
| `--challenge` | `auto` / `wait` / `off` | `auto` | 只在标题可疑或为空时才探一次 |
| `--challenge-timeout` | 秒 | `15` | 只对 `auto` / `wait` 生效 |

### 4.2 禁止需要手动处理挑战


### 4.3 反检测

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

### 4.4 环境变量

| 变量 | 作用 | 与反检测的关系 |
| --- | --- | --- |
| `ATB_NO_DEBUGGER=1` | 彻底禁止 CDP attach | ✅ 更干净，但 `challenge solve` 主动点击失效 |
| `ATB_IDLE_TTL` | daemon 空闲退出秒数 | 无 |
| `ATB_AGENT` | 智能体身份（会话隔离） | 无 |

---

## 5. 怎么验证它真的有效

### 5.1 单元层：不读源码猜逻辑，而是把脚本放进 `vm` 跑

**代码落点**：`tests/stealth_audit.test.mjs`

这个测试的思路值得复用：不 mock 我们的函数，而是造一个"形状像浏览器"的 sandbox，把 `page_stealth.js` 丢进 `vm.runInNewContext` 执行，观察真实回包。sandbox 的几个关键形状缺一样就会误判：

- `window` 与 `globalThis` 必须是同一个对象；
- `canvas.getContext('webgl')` 要能给出 `WEBGL_debug_renderer_info` + `UNMASKED_RENDERER_WEBGL`，否则 `webgl_renderer` 是空串；
- `fetch` / `XHR.prototype.open` 的"原生"版本用 `bind` 出来的函数（`toString` 恒为 `[native code]`），不需要替换 `Function.prototype`；
- 断言数组时用 `length` / `join`——结果对象来自另一个 realm，`assert.deepEqual` 会因 `[[Prototype]]` 不一致而失败。

覆盖面（约 15 个用例）：干净环境 100 分、入口不可枚举/可删除、`eraseSelfSoon` 真的删掉、本工具痕迹归 `self`、headless 六项归 `browser`、用户脚本归 `unknown`、scrub 顺序、scrub 假干净必须 `ok:false`、新版 `__atb_net_*` 槽位识别、四种假槽位不误删、getter 不被触发。

### 5.2 现场层：审计应该 100 分

```bash
atb open https://example.com
atb stealth audit
# 期望：score=100, level=clean, findings=[], extension_ok=true
```

不为 100 时按 `scope` 分流：`self` → `stealth scrub` 后复测；`browser` → 换成有头真实 Chrome（本项目本来就不该出现这类）；`unknown` → 查是不是装了别的扩展/油猴脚本。

### 5.3 挑战层：验收矩阵

| 场景 | 命令 | 期望 |
| --- | --- | --- |
| 正常页（无挑战） | `atb open https://example.com` | 不多花往返，无 `challenge` 字段 |
| JS interstitial | `atb challenge wait` | 3–8s 内 `cleared:true`，`via` 有值 |
| Turnstile 显式勾选 | `atb challenge solve` | `clicks>0` 且 `passed:true` |
| 滑块（尚未实现自动求解） | `atb challenge solve --no-focus` | `need_human` 早于总超时返回 |
| 硬封禁页 | `atb challenge status` | `blocking:true, auto:false`，页面侧无解，需更换网络出口 |
| 后台标签打开 | `atb open ... --background` | 不抢焦点；必要时先激活再等 |
| 挑战中导航 | `challenge wait` 期间页面跳转 | 不因执行上下文销毁误判失败 |

### 5.4 回归层

```bash
cargo test                          # 含 challenge_envelope / should_probe_challenge 等单测
node --test tests/stealth_audit.test.mjs
node --test tests/asset_parity.test.mjs   # 确认 assets 与扩展目录里的副本一致
```

`asset_parity` 这条容易被忽略但很关键：`assets/page_*.js` 和 `assets/atb_bridge/ops/` 里可能是两份拷贝，改了一份没同步，线上跑的还是旧的。

---

## 6. 已知边界与不承诺的东西

诚实地列出来，比事后解释便宜：

| 边界 | 原因 | 应对 |
| --- | --- | --- |
| **hard block（1020/1015）当前不自动处理** | IP / ASN 级封禁，与浏览器无关，页面侧无解 | 换网络出口、换账号 |
| **图片点选 / 滑块尚未实现自动求解** | 需要视觉理解与人类操作 | `challenge focus` 把标签带到前台，由使用者在页面上自行完成 |
| **行为侧、IP 侧管不了** | 鼠标轨迹只覆盖验证码点击那一下 | 控制请求频率、避免批量同一路径 |
| **后台标签会节流** | Chrome 对后台 tab 的 timer 降频 | 等待前用 `tabs/switch`（`allowFocus:false`）激活 |
| **closed shadow root 内探会失效** | Turnstile 内部结构随版本变 | 有几何兜底；`diag` 里能看到每一步失败原因 |
| **不确定"过了"就是"进去了"** | 有些站点点完还要等二次跳转 | `solve` 只给一半预算做主动求解，剩下留给被动等待 |
| **审计只覆盖"页面这一层"** | 站点还有行为侧、IP 侧、账号侧判据 | 审计 100 分不等于不会被风控 |

另外两条工程约束：

- **`challenge` op 在 MAIN world 执行**（判定依赖 `window.turnstile` / `_cf_chl_opt` / `__hcaptcha_cfg`），因此入口 `__abcChallenge` 会在页面里存在。它被定义为**不可枚举 + 可配置**：`Object.keys(window)` 看不到，`delete` 能成功，`stealth audit` 不会把它报成 `own_globals`，`scrub` 也能清掉。扩展低于 5.0.0 时入口是可枚举、不可删除的顶层函数声明，`stealth audit` 会报 `own_globals`。
- **`stealth` 必须在 MAIN world**，因此它自己的入口（`__abcStealth`）会在页面里短暂存在。用 `eraseSelfSoon` 压到 `setTimeout(0)` 级别，已经是能做到的最小暴露窗口。

---

## 7. 如果要继续增强，优先级建议

按"收益 / 成本"排序，不是按"听起来酷"排序：

| 优先级 | 方向 | 具体做法 | 收益 |
| --- | --- | --- | --- |
| P0 | **会话预热** | 首次访问目标站前，先访问同域普通页拿 Cookie、建立 TLS 会话 | 比任何指纹伪装都有效，Managed Challenge 评分直接上一档 |
| P0 | **`cf_clearance` 生命周期管理** | 记录 clearance cookie 的签发时间与绑定 UA/IP，快过期前刷新而不是硬闯 | 减少重复挑战次数 |
| P1 | **挑战指纹库外置** | 把 `TITLE_HINTS` / `CHALLENGE_FRAME_HINTS` 抽成可配置 JSON，支持按站点覆盖 | 新厂商上线不用改代码 |
| P1 | **点击策略按 vendor 分支** | Turnstile / reCAPTCHA / hCaptcha 的勾选框偏移不同，现在是一套几何兜底通吃 | 提高 solve 成功率 |
| P2 | **`diag` 结构化上报** | 把 `challenge_frames` 的三级 CDP 失败原因聚合成一个 `error_code` | 排障从"瞎猜"变"看码" |
| P2 | **行为轨迹参数化** | 步数、抖动幅度、间隔从固定值改成可选 profile（`fast` / `natural`） | 让用户按场景取舍速度与拟人度 |
| P3 | **审计增加"一致性"检查** | UA vs UA-CH vs `navigator.platform` vs 屏幕尺寸 vs 时区是否自洽 | 抓出"伪装留下的一致性硬伤"（当前刻意不做伪装，此项为将来留口） |
| 未来工作 | **图片点选 / 滑块 / 手机邮箱验证的自动求解** | 需要视觉理解与真实人类操作，当前不自动处理；实现前走 `challenge focus` 人工通道，由使用者在页面上自行完成 | 覆盖剩余挑战类型 |
| 未来工作 | **hard block 的页面侧处理** | IP / ASN 级封禁与浏览器无关，页面侧无解；即便立项也依赖更换网络出口，不是纯软件可解 | — |

---

## 8. 代码位置索引

| 能力 | 文件 | 关键位置 |
| --- | --- | --- |
| 挑战/验证码识别、等待、token 判定（MAIN world） | `assets/page_challenge.js` | `detect()`、`wait`、`tokenState()`、入口 `__abcChallenge`（不可枚举 + 可配置） |
| 挑战 CLI | `src/cli/cmd_action.rs` | `ChallengeArgs`、`open --challenge` 的参数透传（`src/cli/cmd_open.rs`） |
| 挑战主流程（status/wait/solve/focus） | `src/server/challenge.rs` | `challenge_value` |
| 主动求解（点击） | `src/server/challenge.rs` | `challenge_auto_solve`（元素驱动：等组件→点→看证据）、`challenge_click_point` |
| iframe 几何定位 | `src/server/challenge_click.rs` | `challenge_frames`、`CHALLENGE_FRAME_HINTS`（pierce DOM，穿 closed shadow root）|
| 别的扩展挡住 CDP | `src/server/challenge_probe.rs` | `strip_foreign_extension_frames`、`is_foreign_extension_frame_error` |
| iframe 内探勾选框 | `src/server/challenge_probe.rs` | `collect_iframe_ids`、`iframe_doc_url` |
| 带轨迹点击 | `src/server/challenge_click.rs` | `challenge_click_at`、`click_event` |
| open 后自动等待 | `src/server/challenge_probe.rs` | `should_probe_challenge`、`auto_wait_challenge`；调用点 `src/server/open.rs` |
| 读页后被拦时附挑战信息 | `src/server/challenge_probe.rs` | `attach_challenge_if_blocked` |
| 审计 / 清理 | `assets/page_stealth.js` | `audit()`、`scrub()`、入口挂载 |
| stealth 调用通道（MAIN world） | `src/server/challenge.rs` | `stealth_page_call`、`stealth_value` 的 `extension_ok` |
| 世界选择（ISOLATED/MAIN） | `assets/atb_bridge/background.js` | `PAGE_OP_WORLD`（`challenge: 'MAIN'`） |
| 挑战判定单元测试 | `tests/challenge_detect.test.mjs` | 全文（closed shadow root / 未下发 / 普通 widget / 普通页面四种形状）|
| 反检测单元测试 | `tests/stealth_audit.test.mjs` | 全文 |

---


