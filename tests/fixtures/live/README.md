# 页面脚本真机夹具

这组静态页面用来**真机**验证 `assets/page_*.js` 的行为（单测里没有 DOM，只能锁数据流）。
每个文件对应一个具体的「静默失效」场景，跑法：

```bash
# 1) 起一个本地静态服务（端口随意，下文用 8765）
cd tests/fixtures/live && python -m http.server 8765 --bind 127.0.0.1

# 2) 后台打开，避免打扰当前标签页
atb open "http://127.0.0.1:8765/cards.html" --agent fixture --background

# 3) 按下面的表逐项断言
```

| 文件 | 验证点 | 期望 |
|---|---|---|
| `cards.html` | 重复结构下的 CSS 路径唯一性（JS-01） | `find --text "Buy now"` 返回 `li:nth-of-type(1..3) > button.buy`；点第 3 个后 `window.__clicked` 为 `[2]` |
| `covered.html` | 目标被全屏遮罩盖住（JS-06） | `click` 返回 `blocked:true` + 顶层 `warnings[0].code=action_target_covered`；`mouse-click` 同样带告警 |
| `password.html` | 口令/卡号脱敏（JS-10） | `read`/`find` 里 `pw`、`cc` 的 `value` 为 `•••` 且 `redacted:true`；`scan` 的 HTML 不含明文 |
| `ad.html` | 固定浮层广告剔除（JS-04） | `scan` 输出不含 `banner-336x140`。前提：`main` 铺满视口且带背景色（浮层要与之重叠） |
| `deep.html` | 超深 DOM 不炸（JS-19） | `scan` 正常返回，头注 `truncated: DOM deeper than 512 levels` |
| `title.html` | 标题像拦截页但内容正常（JS-11） | `challenge status` 返回 `detected:false` / `blocking:false` / `suspected:true` |
| `react-check.html` | React 受控 checkbox（JS-05） | `check '#root input'` 后 `window.__state === true` 且 `window.__changes === 1` |

`vendor/` 里是 React 18 UMD（仅夹具使用，不进 npm 包），这样 `react-check.html` 不依赖外网。

清理：`atb cleanup --agent fixture --dry-run` 看一眼，再去掉 `--dry-run` 关掉本智能体开的标签页。
