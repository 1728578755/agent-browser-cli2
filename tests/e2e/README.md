# 端到端受控页面（tests/e2e）

真实站点回归不稳定：改版、风控、CDN 抖动、地域差异都会让结果随机变红，而这条链路
真正要守的是 **CLI ↔ daemon ↔ 浏览器扩展** 的契约。所以页面全部本地化、可控、可断言，
站点只是一个常量。

```
tests/e2e/
├── README.md            # 本文件
├── pages/               # 受控页面（由 scripts/e2e.mjs 起的静态服务托管）
│   ├── basic.html       # 标题 / 输入框 / 按钮（点击计数写进 #count）/ 链接
│   ├── spa.html         # 点按钮 → history.pushState → 300ms 后异步渲染 #list
│   ├── form.html        # <form> + 账号 + password 字段（验证脱敏）
│   ├── iframe-blocked.html  # 内嵌 chrome-extension:// 假 id 的 iframe（受限页面路径）
│   ├── download.html    # <a download> 触发真实下载
│   └── files/sample.txt # download 场景的下载目标
└── ../../scripts/e2e.mjs    # 跑测试的脚本（Node，无第三方依赖）
```

## 前提

1. **已构建二进制**：`cargo build --release`（默认用 `target/release/atb[.exe]`）。
   也可以用环境变量指向别的构建产物：`ATB_E2E_BIN=/path/to/atb`。
2. **daemon 就绪**：`atb status` 返回 `running: true`。没跑就 `atb restart`，
   或先随便跑一条命令让它自动拉起。
3. **浏览器装了本扩展**：Chrome/Edge 里加载 `assets/atb_bridge`，且扩展连上了 daemon
   （`atb status` 的 `connection.extension_connected` 为 true）。
4. 端口 `18767` 没有被残留的旧 daemon 占用；被占用且 token 失配时用 `atb restart --force`。

前置条件不满足时脚本 **exit 2** 并打印上面这份清单——这是「环境没准备好」，不是用例失败。

## 怎么跑

```bash
node scripts/e2e.mjs                  # 全部场景
node scripts/e2e.mjs --only=scrape    # 只跑名字包含 scrape 的场景
node scripts/e2e.mjs --keep           # 保留 os.tmpdir() 下的临时产物（排障）
ATB_E2E_BIN=./target/debug/atb node scripts/e2e.mjs
```

脚本自己起静态服务（`127.0.0.1` 随机端口），不会占用固定端口；临时文件都写在
`os.tmpdir()/atb-e2e-*`，结束时删除（`--keep` 除外）。

退出码：`0` 全 PASS（SKIP 不算失败） / `1` 有 FAIL / `2` 前置条件不满足。

## 场景

| 场景 | 断言 |
| --- | --- |
| `basic-flow` | `open` → `read`（`result.page.title`）→ `fill #name` → `click #btn` → `exec` 读 `#count` 为 `1`；并复查 `read` 里出现了填进去的文本 |
| `find-redaction` | `find #pass`（`type=password`）命中 1 个且 `items[0].redacted === true`、`value === "•••"`；`find #name`（文本框）**不**脱敏（负向对照） |
| `screenshot` | `screenshot --out` 落盘、非空、前 8 字节是 PNG 魔数 `89504e47 0d0a1a0a` |
| `spa-wait` | 点击后 `wait --selector '#list li' --timeout 5` 成功，`location.pathname === "/spa/detail"`，列表渲染 3 条 |
| `scrape` | `scrape --urls <本地 URL> --out tmp.jsonl` 的 `success:1`、`failed:0`，jsonl 一行且 `data.title` 非空 |
| `download` | 点 `<a download>` → `download-wait --timeout 15` 得到 `state:"complete"`；拿不到下载事件时标 `SKIP`（不算失败） |

关于脱敏场景的字段名：脱敏只对密码类输入生效（`type=password` 或
`autocomplete=current-password` 等），所以断言打在 `form.html` 的 `#pass` 上；
`#name` 保留为普通文本框并作为负向对照，用来证明脱敏不是「对所有字段都打码」。

## 为什么不用真实站点

- **可断言**：页面结构、DOM id、点击后的状态都由本仓库维护，断言能精确到字段；
  真实站点一改版，断言就得跟着改，最后退化成「只要不报错就算过」。
- **可离线**：不依赖外网、DNS、证书、地域，CI 与本地结果一致。
- **可复现失败**：`iframe-blocked.html`、`download.html` 这类边角路径在真实站点上
  几乎无法稳定命中，本地页面可以随时复现。
- **不打扰别人**：不对第三方站点发请求，避免把回归流量打到别人机器上。
