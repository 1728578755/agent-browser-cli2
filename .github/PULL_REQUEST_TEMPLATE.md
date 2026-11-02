# 变更说明

<!-- 一句话说明这个 PR 解决什么问题；关联的 issue 请用 Fixes #123 语法 -->

## 改动内容

- 

## 验证方式

<!-- 贴关键命令与结果；涉及行为变更时说明覆盖了哪些场景 -->

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
node tests/*.test.mjs
npm run check:extension
```

## 检查清单

- [ ] 改的是 `assets/page_*.js`，不是 `assets/tmwd_cdp_bridge/ops/`（`ops/` 是自动副本，禁止手改）
- [ ] 跑过 `npm run check:extension`，资源一致性通过
- [ ] 已在 `CHANGELOG.md` 的 `[Unreleased]` 下补充用户可见变更
- [ ] `README.md` 与 `README_EN.md` 已同步（涉及命令、参数、输出结构时）
- [ ] 没有夹带无关重构；行为变更已说明兼容性影响
