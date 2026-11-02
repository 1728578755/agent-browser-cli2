# 手工测试夹具

这些 HTML 文件供真机手工验收使用，不被自动化测试直接读取。

| 文件 | 用途 |
|---|---|
| `form.html` | 表单填写、`fill` / `select` / `check` 与提交行为 |
| `frames.html` | iframe 枚举、`exec --frame` / `read --frame` |
| `read_hidden.html` | `read --include-hidden` 的可见 / 隐藏内容对比 |
| `read_hidden_2.html` | 隐藏内容的第二种布局（`aria-hidden` / 裁剪） |

用法示例：

```bash
agent-browser-cli open tests/fixtures/form.html
agent-browser-cli read --include-hidden
```
