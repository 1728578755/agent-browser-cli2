// 元素查找：按 CSS 选择器或可见文本定位，返回可操作信息（含简短 CSS 路径）。
// 不点击、不修改，供智能体先"看清有哪些元素"再动手。
//
// CSS 路径必须**能唯一定位**：
// 只按 depth <= 4 往上走、且只有撞上 id 才停，会得到一个相对选择器
// （`div.body > ul > li:nth-of-type(2) > a`），它在文档任何同构位置都成立，
// 而 querySelector 返回文档序第一个匹配 —— 快照 @e37 指向第 3 张卡片的按钮，
// 后续 click 却命中第 1 张卡片的同位按钮，还返回 ok:true。
// 现在每加一层就用 querySelectorAll 验一次「唯一」，不唯一就继续往上加，
// 直到 documentElement（带 nth-of-type 的全路径必然唯一）。
function __abcCssPath(el) {
  if (!el || el.nodeType !== 1) return '';
  const unique = (path) => {
    try { return document.querySelectorAll(path).length === 1; } catch (_) { return false; }
  };
  if (el.id) {
    const byId = '#' + CSS.escape(el.id);
    if (unique(byId)) return byId;
  }
  const parts = [];
  let node = el;
  let guard = 0;
  while (node && node.nodeType === 1 && guard < 64) {
    guard += 1;
    let part = node.tagName.toLowerCase();
    // 类名只加在**目标自身**这一层：每层都带类名会让选择器在页面微调 class
    // （hover 态、A/B 变体）后整体失效，而祖先层用 nth-of-type 已经足够定位。
    if (parts.length === 0) {
      const cls = (node.className && typeof node.className === 'string')
        ? node.className.trim().split(/\s+/).filter(Boolean).slice(0, 2)
        : [];
      if (cls.length) part += '.' + cls.map(c => CSS.escape(c)).join('.');
    }
    const parent = node.parentElement;
    if (parent) {
      const sameTag = Array.from(parent.children).filter(c => c.tagName === node.tagName);
      if (sameTag.length > 1) part += ':nth-of-type(' + (sameTag.indexOf(node) + 1) + ')';
    }
    parts.unshift(part);
    const path = parts.join(' > ');
    if (unique(path)) return path;
    node = parent;
    // 祖先带 id：优先用它当锚点（更短、更稳）。重复 id 的脏页面上不唯一，
    // 就照常继续往上，别把 #id 塞进 parts 污染后面的判断。
    if (node && node.nodeType === 1 && node.id) {
      const anchored = '#' + CSS.escape(node.id) + ' > ' + parts.join(' > ');
      if (unique(anchored)) return anchored;
    }
  }
  return parts.join(' > ');
}

// 命名前缀与 page_action.js 的选择器保持一致：两份脚本注入同一个 world，
// 同名全局会被后注入者覆盖。语义相同也各留一份、各叫各的名。
function __abcFindVisible(el) {
  if (!el || el.nodeType !== 1) return false;
  const rect = el.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) return false;
  const style = getComputedStyle(el);
  if (style.visibility === 'hidden' || style.display === 'none' || Number(style.opacity) === 0) return false;
  return true;
}

/// `text=` 定位读文本的口径：小页面用 innerText（准），超大页面退 textContent
/// （不触发布局）。阈值与 page_wait.js / page_action.js 对齐。
const __ABC_FIND_TEXT_INNER_LIMIT = 20000;

/// 口令类字段的值一律不返回明文。
/// read/find/snapshot 都会把输入框的值带回 daemon → CLI 输出 → agent 上下文 →
/// 日志文件。用户 fill 过登录表单后再 read（LLM 代理的标准工作流）就会把
/// 明文口令写进对话历史，属于必须堵住的泄漏面。
function __abcSecretField(el) {
  if (!el || el.nodeType !== 1) return false;
  const type = String(el.type || '').toLowerCase();
  if (type === 'password') return true;
  const autocomplete = String(el.getAttribute && el.getAttribute('autocomplete') || '').toLowerCase();
  return /current-password|new-password|cc-number|cc-csc|one-time-code/.test(autocomplete);
}

function __abcMaskValue(el, value) {
  const raw = typeof value === 'string' ? value : '';
  if (!raw) return '';
  if (!__abcSecretField(el)) return raw;
  return '•••';
}

function __abcFind(spec) {
  spec = spec || {};
  const limit = Math.max(1, Math.min(Number(spec.limit) || 50, 200));
  const visibleOnly = spec.visible !== false;
  let candidates = [];

  // 非法 selector / xpath 会抛 DOMException，旧实现让它直接冒到通道层：
  // 调用方只能看到 "pageOp failed"，分不清是选择器写错了还是注入失败。
  // 口径与 page_read.js / page_wait.js 的 `invalid selector:` 保持一致。
  try {
    if (spec.selector) {
      candidates = Array.from(document.querySelectorAll(spec.selector));
    } else if (spec.text) {
      const needle = String(spec.text).toLowerCase();
      const exact = !!spec.exact;
      const all = document.querySelectorAll('body *');
      const hits = [];
      const ancestors = new Set();
      let nodeCount = null;
      for (const el of all) {
        // 只看近叶子节点，避免整段容器都命中同一句文本。
        if (el.children.length > 3) continue;
        if (nodeCount === null) {
          try { nodeCount = document.getElementsByTagName('*').length; } catch (_) { nodeCount = 0; }
        }
        const text = nodeCount <= __ABC_FIND_TEXT_INNER_LIMIT
          ? (el.innerText || el.textContent || '')
          : (el.textContent || '');
        const t = text.trim();
        if (!t) continue;
        const lower = t.toLowerCase();
        const hit = exact ? lower === needle : lower.includes(needle);
        if (!hit) continue;
        hits.push(el);
        // 命中节点的祖先一律排除，最终只留"最内层"的匹配元素。
        let parent = el.parentElement;
        while (parent && !ancestors.has(parent)) {
          ancestors.add(parent);
          parent = parent.parentElement;
        }
        if (hits.length >= limit * 8) break;
      }
      candidates = hits.filter(el => !ancestors.has(el));
    } else if (spec.xpath) {
      const snapshot = document.evaluate(spec.xpath, document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
      for (let i = 0; i < snapshot.snapshotLength; i += 1) candidates.push(snapshot.snapshotItem(i));
    }
  } catch (e) {
    const xpath = !!spec.xpath;
    const raw = String((xpath ? spec.xpath : spec.selector) || '');
    return {
      ok: false,
      target: raw,
      error: 'invalid ' + (xpath ? 'xpath' : 'selector') + ': ' + raw + ' (' + ((e && e.message) || e) + ')',
      error_code: xpath ? 'invalid_xpath' : 'invalid_selector'
    };
  }

  const items = [];
  let matched = 0;
  for (const el of candidates) {
    if (!el || el.nodeType !== 1) continue;
    if (visibleOnly && !__abcFindVisible(el)) continue;
    matched += 1;
    if (items.length >= limit) continue;
    const rect = el.getBoundingClientRect();
    // 派生文本与默认属性同样算敏感面：
    // 密码框 innerText 为空，旧写法 `el.innerText || el.value` 会正好回退到明文
    // value，等于把 __abcMaskValue 白做了；aria-label / placeholder 也常被站点
    // 写成"请输入密码/卡号后四位"，同样是半明文。secret 字段一律按 '•••' 处理。
    const secret = __abcSecretField(el);
    const redacted = secret;
    const maskDerived = (v) => (secret ? (v ? '•••' : null) : v);
    const text = secret ? '•••' : String(el.innerText || el.value || '').trim().slice(0, 200);
    items.push({
      tag: el.tagName.toLowerCase(),
      text,
      value: typeof el.value === 'string' ? __abcMaskValue(el, el.value.slice(0, 200)) : null,
      href: el.href || null,
      name: el.name || null,
      type: el.type || null,
      role: el.getAttribute('role') || null,
      ariaLabel: maskDerived(el.getAttribute('aria-label') || null),
      placeholder: maskDerived(el.placeholder || null),
      disabled: !!el.disabled,
      checked: typeof el.checked === 'boolean' ? el.checked : null,
      redacted: redacted || undefined,
      rect: {
        x: Math.round(rect.x), y: Math.round(rect.y),
        w: Math.round(rect.width), h: Math.round(rect.height)
      },
      selector: __abcCssPath(el)
    });
  }

  return { ok: true, matched, returned: items.length, items };
}
