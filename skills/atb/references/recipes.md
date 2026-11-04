# 场景配方

每个配方可直接照抄，只需替换 URL、选择器、文件名。参数含义见 [commands.md](./commands.md)，失败时见 [troubleshooting.md](./troubleshooting.md)。

## 1. 读取已登录页面

场景：站点已在用户 Chrome 里登录，要读后台数据，不重放登录流程。

```bash
atb open https://example.com/dashboard --background --wait-for 'selector:main'
atb read --max-chars 6000
atb cookies
atb find --text '订单号' --limit 10
```

注意：

- 登录态来自用户现有 Profile 与 Cookie；atb 不代替用户登录，也不应持有账号密码。
- `read` 默认带表单结构和计数；只要正文时加 `--no-forms` 省 token。
- 页面很长时先 `scan --text-only` 或 `read --selector`，不要整页 dump。
- 不要用 `cookies --clear` 清理真实账号的登录态，除非用户明确要求。

## 2. 表单填写与提交

场景：后台表单、搜索框、筛选器。

```bash
atb open https://example.com/form --wait-for 'selector:form'
atb snapshot --interactive-only
atb fill @e3 '张三'
atb fill @e5 '13800000000'
atb select @e7 --label '华东'
atb check @e9
atb click @e12 --dry-run
atb click @e12
atb wait --text '提交成功' --timeout 15
atb read --selector '.result'
```

注意：

- `fill` 默认覆盖；追加用 `--append`，清空用 `--clear`。
- `check` 只负责勾选；已勾选时返回 `unchanged:true`，不会反选。
- 提交按钮点不动先看 `warnings`：`action_target_covered` 表示被弹窗遮住，先关弹窗或改用 `mouse-click`。

## 3. 前端调试：console 与网络

场景：本地开发页点了没反应，要定位报错和失败请求。

```bash
atb open http://localhost:3000 --background
atb console start
atb network start
atb click '#submit'
atb console list --level error --limit 50
atb network list --filter /api/ --limit 50
```

注意：

- `console start` / `network start` 要在触发操作**之前**执行，否则抓不到。
- `console list` 的 `--level` 支持 `error` 等日志级别；`--limit` 默认 100。
- 想改请求或响应体用 `net hook` + `net mock`，不是 `network`。

## 4. 批量抓取多个 URL

场景：几十到几百个详情页，统一抽取字段。

```bash
printf '%s\n' https://example.com/1 https://example.com/2 > urls.txt
atb scrape --urls urls.txt --out out.jsonl --batch 4 --concurrency 4 --retry 3 --timeout 30
atb scrape --urls urls.txt --js-file grab.js --out out.jsonl --group-title research
```

注意：

- 输出是 JSONL：一行一个 URL，含 `{url, tab_id, ok, data, error, elapsed_ms}`。
- `--js-file` 的脚本需 `return` 一个可 JSON 序列化的值；缺省返回 `{title, url, text}`。
- 失败按 3s / 5s / 7s 退避重试，`--retry` 是轮数，不是总次数。
- `--concurrency` 默认 4；没有强制上限，但并发越高标签与内存压力越大，重页面建议 4–6。

## 5. 多标签并发

场景：同一操作要打在多个已打开标签上。

```bash
atb read --tab url:docs --max-chars 2000
atb exec --tab all 'return document.title'
atb screenshot --tab all --out 'shots/{index}-{tab}.png'
atb close --tab 111,222
```

注意：

- 一条命令内部并发，比 shell 循环快一个数量级。
- 多目标写文件时 `--out` 必须含 `{tab}` 或 `{index}`，否则预检直接拒绝（退出码 2）。
- 结果按输入顺序返回；单项失败不影响其它项。

## 6. 多智能体并行

场景：两个智能体同时在同一台机器的 Chrome 上工作，互不抢页面。

```bash
export ATB_AGENT=reviewer
atb open https://a.example.com --background
atb read
# 另一个终端
ATB_AGENT=writer atb open https://b.example.com --background
```

注意：

- 身份优先级：`--agent` → `ATB_AGENT` → 终端会话标识 → `default`。
- `open` 只改本智能体的默认标签页；裸命令不会跑到别人的页面上。
- 收尾用 `atb cleanup --dry-run` 再 `atb cleanup`；`--all-agents` 会清掉别人的页面，慎用。

## 7. 挑战页：自动求解与智能体兜底

场景：页面出现 Cloudflare 之类的验证页，先让 atb 自动求解；失败时智能体自己点，不要直接交给人。

```bash
atb open https://protected.example.com --challenge auto --challenge-timeout 20
atb challenge status
atb challenge solve --timeout 120
atb challenge widget
atb challenge click --at 320,410
atb challenge status
atb challenge focus   # 只有图片/短信等必须人做的验证
```

注意：

- `open` 默认 `--challenge auto`：命中后先主动求解（定位组件、带轨迹点击），再等 token/跳转；不是只等待。`challenge solve` 手动跑同一路径，`challenge auto` 只求解一次，`challenge wait` 才是纯等待。
- 自动求解失败先看 `challenge.auto_solve.reason`：`no-widget-frame` 表示组件没渲染；`click-no-effect` 表示点了没反应；`verifying_not_completed` 表示停在「正在验证」；`widget-removed-no-nav` 表示组件消失但没跳转。
- 兜底顺序是 `widget` 取坐标 → `click --at` 自己点 → `status` 复查；只有图片点选、短信/邮箱验证这类需要人判读的场景才 `focus`。
- 不要循环狂点：连续失败会浪费预算，也会让站点更警惕。不保证通过，也不破解访问控制。

## 8. 下载文件

场景：点击导出后拿到文件，或直接下载已知 URL。

```bash
atb download https://example.com/report.pdf --filename report.pdf --dir atb-downloads
atb download-wait --id <downloadId> --timeout 120
```

注意：

- `--dir` 是 Chrome 默认下载目录下的子目录，不是任意绝对路径。
- `download-wait` 省略 `--id` 时等最近一条下载。
- 下载由浏览器执行；磁盘满、权限不足或站点要求二次确认时会停在 `interrupted`。

## 9. iframe 内操作

场景：表单或按钮在 iframe 里，直接选择器打不到。

```bash
atb frames
atb find 'button.submit' --frame 3
atb fill '#email' 'a@b.com' --frame 3
atb click '#submit' --frame 3
```

注意：

- `frame_id` 会随导航变化，操作前重新 `frames`。
- 跨域 iframe 只能通过 `--frame` 走扩展侧执行，页面内 JS 拿不到它的 DOM。
- `exec --all-frames` 会返回每个 frame 的结果，适合做全页扫描。

## 10. 网络 mock 调试

场景：后端接口未就绪，前端要验证空态、错误态或慢请求。

```bash
atb net mock '/api/orders' --status 200 --body '{"items":[]}' --header 'Content-Type: application/json'
atb reload
atb net routes
atb net reset
```

注意：

- 默认 `--engine page` 只包装页面 `fetch` / XHR，拦不到 `img`、`script` 和二进制响应。
- 需要拦静态资源或 `blob` 响应用 `--engine cdp`，代价是 attach 调试器，浏览器会出现调试提示。
- 只改请求用 `--request-header` / `--request-body`；只改真实响应用 `--response-status` / `--response-body`。

## 11. 截图与 PDF

场景：留证、生成报告、检查视觉回归。

```bash
atb screenshot --out shot.png
atb screenshot --selector '#chart' --out chart.png
atb screenshot --full-page --format jpeg --quality 80 --out full.jpg
atb save-pdf --out page.pdf --paper a4 --landscape
```

注意：

- 可见区域截图走 `captureVisibleTab`，不 attach 调试器。
- 整页截图、元素截图、`save-pdf` **优先**走调试器；页面嵌了别的扩展的 iframe（`page_protected`）时自动回落：
  整页截图变逐屏拼接（`via:"captureFullPageStitched"`），`save-pdf` 变图片型 PDF（`text_selectable:false`），
  元素截图没有回落通道（会报 `page_protected`）。设 `ATB_NO_DEBUGGER=1` 时同理。
- 长页面用 `--max-height <设备像素>` 限高（默认 20000），超出会截断并标 `truncated:true`；也可以直接选 `--format jpeg --quality 80` 控制体积。
- 多标签截图必须带 `{tab}` 或 `{index}` 占位符，写法见配方 5。
- 元素截图前先 `wait --visible --selector '#chart'`，避免截到未渲染区域。
- 拿到结果先看 `via` / `synthetic`：`captureVisibleTab` 是可见区域、`cdp` 是元素/整页真截图、`stitched*` 是拼接产物。

## 12. 持久注入

场景：页面每次导航都要带同一段初始化脚本（埋点、开关、工具函数）。

```bash
atb inject 'window.__ATB_FLAG__ = true' --persist flag --match 'https://example.com/*'
atb reload
atb exec 'return window.__ATB_FLAG__'
atb inject --remove flag
```

注意：

- `--persist <name>` 命名后，每次页面导航自动重放；`--list` 查看，`--remove` 删除。
- `--match` 默认 `<all_urls>`，尽量收窄，避免污染无关站点。
- `--world isolated` 里的变量页面脚本看不到；要影响页面逻辑就用默认 `main`。
