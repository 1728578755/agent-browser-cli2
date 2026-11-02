# 安全政策 / Security Policy

**项目：** agent-browser-cli（CLI `0.3.8`）+ Chrome 扩展 `Agent Browser CLI Bridge`（Manifest V3，版本 `3.4`）
**最后更新：** 2026-11-02

本文件说明：支持哪些版本、如何私密报告漏洞、我们承诺的响应时限、当前安全设计与**已知局限**。关于数据流向与权限，请同时阅读 [PRIVACY.md](./PRIVACY.md)。

---

## 1. 支持版本

| 版本 | 状态 | 说明 |
| --- | --- | --- |
| `0.3.x` | ✅ 支持 | 当前维护线，安全修复优先进入最新补丁版本 |
| `0.2.x` 及更早 | ❌ 不再保证 | 不接收安全修复；请先升级再报告 |
| 扩展 `3.4`（随 CLI 0.3.8 发布） | ✅ 支持 | daemon 检测到扩展版本低于随包版本时会请求浏览器重载 |
| 更早的扩展构建 | ❌ 不再保证 | 请删除 `assets/tmwd_cdp_bridge` 并以最新包重新加载 |

升级方式与安装一致（见 README）：替换 CLI 二进制 / npm 包，删除旧扩展目录后在 `chrome://extensions` 重新加载。安全修复不会回移到旧版本。

---

## 2. 私密报告渠道

**请不要为安全漏洞开公开 issue、PR 或讨论帖。**

请使用 GitHub Security Advisories 的私密报告入口：

> <https://github.com/1728578755/agent-browser-cli2/security/advisories/new>

该入口只在维护者与报告者之间可见，支持后续的私下讨论、补丁协作与 CVE 申请。若入口不可用，可先开一个**不含任何技术细节**的 issue，仅说明「需要私密安全联系方式」，维护者会跟进建立私密通道。

报告中请尽量包含：受影响版本、组件（CLI / daemon / 扩展）、复现所需的前置条件（例如是否配置了 `ws-trust.json`、是否处于空闲窗口）、影响面判断，以及你希望的处理方式。**不需要**附带可直接运行的攻击脚本；描述清楚原理与条件即可，必要时我们再单独沟通细节。

---

## 3. 响应时限

以下是目标时限（best effort，非法律承诺）：

| 阶段 | 目标 |
| --- | --- |
| 确认收到报告 | **72 小时内** |
| 初步评估（是否受影响、严重级别、是否需要临时缓解） | 7 天内 |
| 修复或缓解方案 | **30 天内**发布；复杂问题会先给缓解措施并同步进度 |
| 公开披露 | 修复发布后协调进行，通常不超过 90 天；如已被主动利用会提前 |

我们会在整个过程中同步进展；若你希望匿名致谢，请在报告中说明。

---

## 4. 安全设计

### 4.1 网络暴露面

- daemon 的 HTTP API（`127.0.0.1:18767`）与扩展 WebSocket（`127.0.0.1:18765`）都**只绑定回环地址**，不监听 `0.0.0.0`，局域网内其它主机无法直连。
- 两者都是**明文**（`http://` / `ws://`）。这是刻意的取舍：链路不出本机，TLS 只会增加自签证书与信任配置的成本。代价见 5.4。
- HTTP API 会检查 `Origin` 头：只接受 `http://127.0.0.1:18767` 与 `http://localhost:18767`，其它浏览器来源一律 `403 origin_not_allowed`，避免任意网页用浏览器当跳板访问本机 API（CSRF / DNS rebinding 方向）。
- WebSocket 握手要求 `Origin` 为 Chrome 扩展来源（`chrome-extension://`），没有该头的浏览器页面无法直接连入。但 `Origin` 是客户端自报的请求头，本机进程可以伪造它，因此这道检查只用于挡住「网页里的脚本直连本机端口」，**不构成对本地进程的身份认证**；本地进程的防线是白名单与 token（见 5.1）。

### 4.2 API token

- 每个 daemon 启动时生成新的随机 token（`Uuid::new_v4().simple()`，32 位十六进制），先监听端口、再写入 `~/.agent-browser-cli/api-token`，因此旧 token 不会在重启后被复用。
- 除 `/ping`（仅返回存活与端口绑定状态，不含业务数据）外，所有路由都经过同一层中间件校验 `x-agent-browser-token`。
- 比较使用**恒定时间**实现（长度不同才提前返回，长度本身固定且不是秘密），降低同机进程通过响应时间逐字节猜测 token 的可能。
- Unix 上 token 文件以 `0600` 创建；Windows 上依赖用户目录的默认 ACL。
- token 在 daemon 退出后**保留在磁盘**，由下一个 daemon 在绑定端口后轮换，避免「退出中的 daemon 删掉新 daemon 的 token」这类竞态。

### 4.3 token 指纹（连接登记表）

已建立的 WebSocket 连接在内存登记表中只保存 token 的**指纹**（`DefaultHasher` 的 64 位结果），不保存明文。用途是回答「这条连接当初带的 token 与现在配置的是否同一个」，从而在信任配置收紧后撤销老连接。

该指纹**不承担密码学职责**：不用于存储凭据、不用于跨进程比较、不防碰撞。它只在同一进程内做相等判断，登记表本身不会让明文 token 出现在日志或调试输出里。

### 4.4 配置的原子写入与 fail-closed

- `config.json`、`api-token`、`net-hooks.json`、`ws-trust.json` 的写入统一走「同目录临时文件 → `write_all` → `sync_all` → 原子替换」。
- Unix 上 `rename` 直接覆盖；Windows 上 `rename` 不能覆盖，因此采用「备份 → 替换 → 删备份」，替换失败会把备份还原，保证目标文件不会永久缺失。
- 读取时若目标不存在则回退读 `.bak`。这条规则对 `ws-trust.json` 尤其重要：否则「主文件暂时不存在、备份里还有白名单」会被误判成「从没配过」，白名单静默退化为「任何扩展可连」（fail-open）。
- `ws-trust.json` 解析失败或内容为空时视为**损坏**：daemon 拒绝**所有** WebSocket 握手（`503`）并打印修复指引，**不会**回退到兼容模式。修复方式是删除该文件，或运行 `agent-browser-cli trust-extension --clear` 重建。
- 握手判定顺序固定为「Origin → 白名单 → token」。信任配置变更通过 `/reload-trust` 生效：每条连接每 500ms 轮询一次撤销标志，命中即关闭，无需重启 daemon。
- 插件端口绑定失败不会被静默吞掉：错误记入 `ws_bind_error`，`doctor` 与 `/ping` 都会如实报告「daemon 在跑但插件端口没监听」，避免把「端口被别的程序占着」误判成正常。
- 同一用户下只允许一个 daemon：CLI 用 `~/.agent-browser-cli/daemon.lock` 的文件排他锁串行化「检查 → 拉起」，拿到锁后再用 `/ping` 二次确认，避免两个 daemon 同时抢 18765。

### 4.5 扩展侧

- 扩展不从网络加载任何代码，也不执行远程脚本；全部逻辑随包分发。
- WS token 由用户在弹窗中手动填写，保存在 `chrome.storage.local`，**明文存储**（Chrome 扩展存储没有可用的系统级加密容器）。弹窗只显示「是否已设置」，不回显原文。
- 旧版本曾在打开弹窗时自动把 Cookie 写入剪贴板；该行为已移除，现在必须显式点击「复制 cookies」，且默认跳过 `httpOnly` / `secure` 条目。

---

## 5. 已知局限（Known limitations）

以下不是「待修 bug 列表」，而是**当前设计接受的风险**。请在使用前确认你能接受，并按 6 节的建议加固。

### 5.1 默认「装扩展即可连」，且扩展不验证 daemon 身份

开箱状态下 `ws-trust.json` 不存在 = 兼容模式：任何 Chrome 扩展都能连上 daemon，任何本机进程也都能作为客户端接入 18767（只要拿到 token）。

`agent-browser-cli trust-extension <id>` / `--token <value>` 收紧的是**「谁可以连 daemon」**：白名单校验扩展 ID，token 校验握手时 `?token=` 的值。它**不能**回答反方向的问题 —— 扩展在连接时**不校验对面是不是真正的 daemon**：没有证书、没有挑战应答、没有签名，扩展只看「`127.0.0.1:18765` 上有没有人接受 WebSocket」。

因此：配置 token 能防「别的扩展连你的 daemon」，**防不住「你的扩展连上别的进程」**。

### 5.2 daemon 空闲退出后的端口空窗期

daemon 空闲 5 分钟自动退出，18765 随之释放。在这段空窗期（以及任何 daemon 未运行的时刻），**本机任意进程都可以抢先监听 18765**。扩展会按自己的重试节奏连上去，并把它当作 daemon，执行对方下发的命令 —— 包括读取任意站点的 Cookie、调用任意 CDP method、注入脚本、截图。

也就是说，这是一个**反向冒充**问题：攻击面不在 daemon 的鉴权，而在扩展缺少对服务端的身份校验。当前版本没有机制能完全消除它，只能缩短暴露窗口并降低被利用的价值。

### 5.3 缓解措施（推荐全部启用）

| 措施 | 作用 |
| --- | --- |
| `agent-browser-cli trust-extension <扩展ID>` | 白名单：daemon 只接受指定扩展 ID 的连接，减少「别的扩展借用你的 daemon」 |
| `agent-browser-cli trust-extension --token <值>`，并在扩展弹窗填入同一值 | 握手 token：未携带或错误的连接被 `401` 拒绝；对 5.2 的反向冒充无效 |
| 调短 `AGENT_BROWSER_CLI_IDLE_TTL`（例如 `60`） | 缩小端口无人监听的窗口 |
| 不用时执行 `agent-browser-cli stop` | 主动结束 daemon；窗口期从「空闲 5 分钟后」提前到「你决定停止时」 |
| 只在专用 Chrome Profile 中使用 | 限制被波及的登录态范围，敏感账号不放在自动化 Profile 里 |
| 定期 `agent-browser-cli doctor` | 检查插件端口是否被非预期进程占用、daemon 是否处于降级状态 |

`AGENT_BROWSER_CLI_IDLE_TTL` 接受秒数；`0` 或非法值会回落到默认 300 秒。

### 5.4 明文 HTTP / WebSocket 仅监听回环

18767 与 18765 都是明文协议。数据不出本机网卡，但**同机的其它进程**（包括同一用户下的任意程序）可以在具备相应权限时读取回环流量、或直接访问这两个端口。API 侧的防线是随机 token + 回环绑定，而不是传输加密。

同理，本机同用户下的恶意进程可以读取 `~/.agent-browser-cli/api-token`、`ws-trust.json`、`daemon.log`，以及 Chrome 的扩展存储。**本项目的威胁模型不覆盖「攻击者已能以你的用户身份在本机执行代码」这一前提** —— 在这种前提下，浏览器里已登录的会话本来就已经暴露。

### 5.5 其它边界

- `debugger` 权限可读取并改写请求/响应体；`downloads` 权限可清空全部下载历史；`management` 权限可禁用其它扩展。这些都是产品功能，被滥用时的后果由「谁能连上 daemon」决定。
- `inject --persist` 的脚本长期保存在扩展存储中，并在每次导航后重放；忘记清理会让旧脚本持续生效。
- 日志无自动轮转，可能包含 URL 与偶发的原始报文片段；共享日志前请先检查。

---

## 6. 安全使用建议

1. 从官方 Release 获取 CLI 与扩展包，核对来源；不要加载来路不明的 `tmwd_cdp_bridge` 目录。
2. 首次使用就配置 `trust-extension`（白名单 + token），不要停留在兼容模式。
3. 为自动化单独建一个 Chrome Profile，不要在日常主力 Profile（含邮箱、网盘、后台、支付）上跑不受信任的 Agent 流程。
4. 不使用时 `agent-browser-cli stop`；需要长期驻留时把空闲 TTL 调短。
5. 谨慎使用 `net hook --capture-bodies`、`cookies`、`screenshot`：这些命令的输出通常会进入 Agent 上下文并可能发给模型服务商。
6. 发现异常（扩展频繁重连、`doctor` 报告端口异常、出现未执行过的操作）时，立即 `stop`，检查 `~/.agent-browser-cli/logs/daemon.log`；重启 daemon 会轮换 API token，握手 token 需要时用 `agent-browser-cli trust-extension --token <新值>` 重设。
7. 卸载时按 PRIVACY.md 4.4 节清理 `~/.agent-browser-cli` 与扩展。

---

## 7. 不在范围内

以下通常不作为安全漏洞处理，但欢迎讨论：

- 需要「攻击者已能以你的用户身份在本机执行任意代码」才能成立的问题（见 5.4）。
- 用户主动把 Cookie、截图或页面内容粘贴给第三方服务导致的数据外泄。
- 目标站点自身的漏洞，或使用本工具违反站点条款 / 当地法律的情形。
- 未启用 `trust-extension` 时「任意扩展可连」的默认行为 —— 这是已知设计，见 5.1。

---

# Security Policy (English)

**Supported versions:** CLI `0.3.x` and extension `3.4` are supported. `0.2.x` and earlier, and older extension builds, receive no security fixes.

**Reporting:** Use GitHub Security Advisories at <https://github.com/1728578755/agent-browser-cli2/security/advisories/new>. Do not open a public issue containing exploit details; if the advisory form is unavailable, open an issue that only asks for a private contact channel.

**Response targets:** acknowledgement within 72 hours, initial assessment within 7 days, fix or mitigation within 30 days, coordinated disclosure normally within 90 days.

**Design.** The daemon binds only to loopback (`127.0.0.1:18767` for HTTP, `127.0.0.1:18765` for the extension WebSocket). Every HTTP route except `/ping` requires the `x-agent-browser-token` header; the token is regenerated on each daemon start, written with mode `0600` on Unix, and compared in constant time. Browser `Origin` values are restricted to the loopback API origins, and the WebSocket handshake requires a Chrome extension origin. Established connections are tracked by a 64-bit **token fingerprint** rather than the plaintext token; the fingerprint is used only for in-process equality checks and carries no cryptographic weight. Configuration files are written atomically (temp file, `fsync`, atomic replace, with a `.bak` fallback on read), and a corrupt `ws-trust.json` is **fail-closed**: all handshakes are rejected with `503` instead of silently degrading to "any extension may connect". Trust changes are applied to live connections via `/reload-trust` within 500 ms.

**Known limitations.** (1) Out of the box the bridge is "install and connect", and `trust-extension` only authenticates *who may connect to the daemon*; the extension never authenticates the daemon, so it cannot tell a real daemon from another local process. (2) Because the daemon exits after five idle minutes, port 18765 is frequently unoccupied, and another local process can bind it first; the extension will then execute that process's commands, including reading cookies from any site and invoking arbitrary CDP methods. (3) Mitigations: use the extension allow-list and handshake token, shorten `AGENT_BROWSER_CLI_IDLE_TTL`, and run `agent-browser-cli stop` when idle. (4) HTTP and WebSocket traffic is plaintext and only loopback-bound; the threat model does not cover an attacker who already executes code as your OS user.

**Out of scope:** issues requiring prior code execution as the same user, user-initiated sharing of cookies or screenshots with third parties, and the documented default behaviour when `trust-extension` is not configured.
