# 参与贡献

欢迎提交缺陷报告、功能建议和代码。提问或开 PR 之前，请先搜索已有 issue，避免重复。

## 开发环境

| 依赖 | 要求 | 说明 |
|---|---|---|
| Rust | stable（edition 2021） | 需要 `rustfmt` 与 `clippy` 组件 |
| Node.js | >= 18 | 扩展脚本同步、Node 侧契约测试 |
| Chrome / Edge | 近两年版本 | 只有手工验证需要；自动化测试用假扩展 |

```bash
git clone https://github.com/1728578755/agent-browser-cli
cd agent-browser-cli
cargo build
```

跨平台均可开发；CI 在 Linux、macOS、Windows 三套环境跑 Rust 检查。

## 构建

```bash
cargo build              # debug 二进制
cargo build --release    # 发布用二进制
npm run build            # 等价于 cargo build --release
```

## 测试矩阵

提交 PR 前请在本地跑完与改动相关的检查；改到共享代码时跑全量。

| 命令 | 检查内容 | 备注 |
|---|---|---|
| `cargo fmt --all -- --check` | 代码格式 | 不要提交未格式化的改动 |
| `cargo clippy --all-targets --all-features -- -D warnings` | 静态检查 | `-D warnings` 是硬性要求，告警即失败 |
| `cargo test --all-targets --all-features` | Rust 单元 / 集成测试 | |
| `node tests/*.test.mjs` | Node 侧契约测试 | Git Bash / macOS / Linux 可直接用通配符 |
| `npm run check:extension` | `assets/page_*.js` 与 `ops/` 副本逐字节一致 | 只比对，不写盘 |
| `node tests/mock_extension.mjs --self-test` | 端到端自检（真 daemon + 假扩展） | 占用 18766 / 18767 端口，必须串行跑 |

Windows PowerShell 下执行 Node 测试：

```powershell
Get-ChildItem tests/*.test.mjs | ForEach-Object { node $_.FullName }
```

涉及真实浏览器的用例（`tests/live_*.mjs`）需要本地已装扩展，不属于 CI 必跑项。

## 三条硬约定

### 1. 页面脚本只有一个来源

页面侧脚本的源文件是 `assets/page_*.js`，扩展实际注入的是 `assets/tmwd_cdp_bridge/ops/` 下的副本。

- 只改 `assets/page_*.js`，然后执行 `npm run sync:extension` 生成副本。
- **不要手改 `assets/tmwd_cdp_bridge/ops/`**：它由脚本生成，下次同步会被覆盖，而且两边不一致会让严格 CSP 页面跑到旧代码。
- `npm run check:extension` 会在 CI 中守卫这条约定；`prepack` 也会在打包前自动同步一次。

### 2. 版本以 `package.json` 为唯一真相源

`package.json` 的 `version` 是版本号来源。发版或调整版本时执行：

```bash
npm run version
```

该命令会同步 `package-lock.json`、`Cargo.toml` 与 `README.md` / `README_EN.md` 中的版本展示，并把改动加入暂存区。不要手工分别修改这些文件，否则版本漂移会让 `npm ci` 直接失败。

### 3. 扩展资源保持一致

`assets/tmwd_cdp_bridge/` 下的 `manifest.json`、`background.js`、`content.js`、`popup.js` 与 `ops/` 副本必须与源码同批提交。改动页面脚本却漏掉同步、或只更新其中一处，都会导致「本机可用、干净安装不可用」。

## 提交信息

采用 `type(scope): summary` 形式，summary 用一句话说明改了什么：

```text
fix(net): 持久化规则刷新后不再丢失
feat(wait): 用页面内 MutationObserver 替代定时器轮询
docs: 补充异步任务的重试语义
```

常用 type：`feat`、`fix`、`docs`、`refactor`、`perf`、`test`、`chore`。scope 可省略，涉及具体命令时优先用命令名。

一个提交只做一件事。格式化、重命名这类纯机械改动尽量单独提交，方便 review 和回滚。

## CHANGELOG 约定

用户可见的变更都要写进 `CHANGELOG.md` 的 `## [Unreleased]` 段，按 `Added` / `Changed` / `Fixed` / `Removed` 分类：

- 描述「原来的现象 + 现在的行为」，而不是罗列改了哪些函数。
- 会破坏兼容性的变更（退出码、输出结构、参数语义）必须在条目里显式标注。
- 纯内部重构、测试补充、注释修正可以不写。
- 发版时由维护者把 `[Unreleased]` 归档到具体版本号下。

## PR 检查清单

提交 PR 时请逐项确认：

- [ ] 改动范围与 issue / 描述一致，没有夹带无关重构
- [ ] 新增或修改的行为有对应测试；修 bug 时优先补一个能复现的用例
- [ ] `cargo fmt` / `cargo clippy -D warnings` / `cargo test` 全部通过
- [ ] `node tests/*.test.mjs` 通过
- [ ] 改过 `assets/page_*.js` 时跑过 `npm run sync:extension`，且 `npm run check:extension` 通过
- [ ] 没有手改 `assets/tmwd_cdp_bridge/ops/`
- [ ] `CHANGELOG.md` 已更新（用户可见变更）
- [ ] 涉及命令、参数、输出结构时，`README.md` 与 `README_EN.md` 已同步
- [ ] 涉及版本号时使用 `npm run version`，没有手工散改

维护者可能会要求拆分过大的 PR，或在合并前 squash 提交。
