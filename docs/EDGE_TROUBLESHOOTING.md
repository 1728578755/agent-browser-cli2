# Edge / Chrome 扩展加载失败排查

目标现象：

```text
ATB（atb_bridge）
问题 已找到 1 个
Service worker registration failed. Status code: 2
```

`Status code: 2` 是 Chromium 的「Service Worker 注册失败」，它**不告诉你原因**。
按下面的顺序逐项排除，绝大多数是前三条。

---

## 0. 先对比版本号（30 秒定位「改完没生效」）

`edge://extensions` 里本扩展卡片上显示的版本号，必须等于磁盘上 `manifest.json` 的版本。

```bash
node -e "console.log(require('<你的扩展目录>/manifest.json').version)"
```

- **两边一致** → 浏览器跑的是当前代码，继续往下排查；
- **卡片上更旧**（例如磁盘 5.3.0、卡片 5.0.1）→ 浏览器从没重载过，`Service worker registration failed`
  多半是上一次替换文件时读到了半截 `background.js`。这种情况**改代码没用**，点一次「重新加载」即可。

### 为什么会出现「半截文件」

Chrome / Edge 重新注册 Service Worker 时会重新读 `background.js`。如果用 `cp` / 解压覆盖的方式更新目录，
而浏览器恰好在这一刻重载，就会读到写了一半的文件，注册直接失败；失败状态会一直挂在扩展页上，
后续不会自动重试，看起来就像「代码坏了」。

统一用原子部署脚本，不再有这个问题：

```bash
node scripts/deploy-extension.mjs --dry-run   # 看计划
node scripts/deploy-extension.mjs             # 逐文件「临时文件 + rename」，写完逐字节校验
```

判断是不是这一类问题的旁证：扩展页显示旧版本、`atb status` 里看不到这个浏览器（SW 没起来就不会连 daemon）、
但用干净 profile 启动同一个目录能正常加载（`CDP Bridge installed`）。

---

## 1. 确认加载的是哪个目录（最常见）

必须选择**包含 `manifest.json` 的那一层**。

```text
正确： .../assets/atb_bridge/          ← 这里有 manifest.json
错误： .../assets/                          ← 没有 manifest.json
错误： .../atb-extension/atb_bridge/    ← 解压工具多建了一层子目录
```

检查方法：把要加载的目录打开，第一眼应该能看到 `manifest.json`、`background.js`、`content.js`。

命令行确认：

```bash
ls assets/atb_bridge/manifest.json assets/atb_bridge/background.js
```

如果是从 `chrome-extensions.zip` 解压的：zip 根目录直接就是扩展文件，`manifest.json` 就在解压目录里，没有再套一层 `atb_bridge`。若解压工具自动多建了子目录，往下选到直接含 `manifest.json` 的那一层。

---

## 2. 确认 `background.js` 真的存在且没被改名

manifest 里写的是：

```json
"background": { "service_worker": "background.js" }
```

如果目录里叫 `background.js.txt`（Windows 隐藏扩展名导致）、`Background.js`（大小写敏感环境），
注册就会失败。用 `dir` / `ls -l` 看真实文件名。

---

## 3. 确认浏览器版本

`manifest.json` 里声明了 `"minimum_chrome_version": "120"`。Edge 是基于 Chromium 的，
但**版本号不一定对得上**：

- Edge 120+ → 等价 Chromium 120，可用；
- 更老的 Edge（例如 117/118）→ 缺少 `chrome.tabs.group`、`captureVisibleTab` 的某些行为，
  启动期调用可能直接抛错，表现就是 SW 注册失败。

查看：`edge://version`，第一行 `Microsoft Edge 1xx.x.x.x`。
低于 120 请升级浏览器，或改用 Chrome。

---

## 4. 看扩展页的「错误」详情

`edge://extensions` → 找到本扩展 → 点「错误」/「Errors」按钮：

| 详情 | 含义 | 处理 |
| --- | --- | --- |
| `Manifest file is missing or unreadable` | 目录选错 / JSON 损坏 | 回到第 1 步 |
| `Could not load background script 'background.js'` | 文件缺失 | 回到第 2 步 |
| `Service worker registration failed` + 一段 JS 堆栈 | 启动期脚本抛错 | 升级扩展（4.0+ 已把启动期调用包了防御），或把堆栈发出来 |
| `Cannot load extension with file or directory name _metadata` | 解压出 Mac 垃圾文件 | 删掉 `__MACOSX` / `_metadata` 后重载 |

扩展 4.0+ 会把最近一次启动错误写进 `chrome.storage.local.lastServiceWorkerError`，
在扩展的 popup 里能看到一行「最近启动错误」。

---

## 5. 企业策略 / 未打包扩展被禁

学校或公司管理的 Edge 常禁用开发者模式：

- `edge://extensions` 里「开发人员模式」开关是灰的；
- 或提示「此扩展程序已被管理员阻止」。

查看策略：`edge://policy`，搜索 `ExtensionInstallBlocklist` / `ExtensionDevelopmentMode`。
这种情况需要管理员放开，或用个人设备测试。

---

## 6. 重复加载 / ID 冲突

同一个扩展目录被加载了两次（不同路径各一份），会互相抢 WebSocket 连接，表现为
daemon 里出现两个 `browser_id`、命令随机打到没有响应的那个。

处理：`edge://extensions` 里只保留一份，删掉多余的。

---

## 7. 加载成功后仍然连不上 daemon

按顺序看：

```bash
atb status        # summary / compatibility / next_steps
atb doctor        # 逐项自检
atb logs --tail 100
```

### 扩展 5.3.0 的行为变化

- **open shadow DOM**：`find` / `snapshot` / `read` 现在会递归 open shadow root，返回项多出 `scope` / `host_selector` / `shadow_path` 字段。旧版扩展不返回这些字段，daemon 仍按旧形状解析，不会报错，但拿不到 shadow 内容——这才是「明明页面上有按钮却 `find` 不到」的常见原因，先确认扩展卡片是 5.3.0。
- **exec 取消帧**：daemon 在命令超时/取消时会向扩展发 `{cmd:"cancel", execId}`。**旧 daemon 不认识这个帧也无所谓**：扩展只是不提前释放那次 CDP attach，等 idle 定时器回收即可，命令结果与错误语义都不受影响。反过来，5.3.0 扩展配旧 daemon 时也不会因为缺少 cancel 帧而失败。
- `extension_not_connected`：端口不一致（默认 18765）→ `set-extension-port` 或改扩展 popup 里的端口；
- `compatibility.ok:false` 且 `issues[].kind=extension_outdated`：浏览器里跑的是旧版扩展文件，
  覆盖目录后重新加载；
- 刚重启 daemon：扩展重连最长约 30-60 秒（MV3 alarm 下限），稍等再试。

---

## 8. 仍然失败时，请提供这些信息

1. `edge://version` 第一行；
2. `edge://extensions` 里该扩展的「错误」详情全文；
3. 加载的目录路径（`dir` 输出前 10 行）；
4. `atb status --pretty` 输出；
5. daemon 日志最后 50 行（`atb logs --tail 50`）。
