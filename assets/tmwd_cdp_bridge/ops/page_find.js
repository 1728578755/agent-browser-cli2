// 元素查找：按 CSS 选择器或可见文本定位，返回可操作信息（含简短 CSS 路径）。
// 不点击、不修改，供智能体先"看清有哪些元素"再动手。
function __abcCssPath(el) {
  if (!el || el.nodeType !== 1) return '';
  if (el.id) return '#' + CSS.escape(el.id);
  const parts = [];
  let node = el;
  let depth = 0;
  while (node && node.nodeType === 1 && depth < 4) {
    let part = node.tagName.toLowerCase();
    const cls = (node.className && typeof node.className === 'string')
      ? node.className.trim().split(/\s+/).filter(Boolean).slice(0, 2)
      : [];
    if (cls.length) part += '.' + cls.map(c => CSS.escape(c)).join('.');
    const parent = node.parentElement;
    if (parent) {
      const sameTag = Array.from(parent.children).filter(c => c.tagName === node.tagName);
      if (sameTag.length > 1) part += ':nth-of-type(' + (sameTag.indexOf(node) + 1) + ')';
    }
    parts.unshift(part);
    node = parent;
    depth += 1;
    if (node && node.id) { parts.unshift('#' + CSS.escape(node.id)); break; }
  }
  return parts.join(' > ');
}

function __abcVisible(el) {
  if (!el || el.nodeType !== 1) return false;
  const rect = el.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) return false;
  const style = getComputedStyle(el);
  if (style.visibility === 'hidden' || style.display === 'none' || Number(style.opacity) === 0) return false;
  return true;
}

function __abcFind(spec) {
  spec = spec || {};
  const limit = Math.max(1, Math.min(Number(spec.limit) || 50, 200));
  const visibleOnly = spec.visible !== false;
  let candidates = [];

  if (spec.selector) {
    candidates = Array.from(document.querySelectorAll(spec.selector));
  } else if (spec.text) {
    const needle = String(spec.text).toLowerCase();
    const exact = !!spec.exact;
    const all = document.querySelectorAll('body *');
    const hits = [];
    const ancestors = new Set();
    for (const el of all) {
      // 只看近叶子节点，避免整段容器都命中同一句文本。
      if (el.children.length > 3) continue;
      const t = (el.innerText || el.textContent || '').trim();
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

  const items = [];
  let matched = 0;
  for (const el of candidates) {
    if (!el || el.nodeType !== 1) continue;
    if (visibleOnly && !__abcVisible(el)) continue;
    matched += 1;
    if (items.length >= limit) continue;
    const rect = el.getBoundingClientRect();
    items.push({
      tag: el.tagName.toLowerCase(),
      text: (el.innerText || el.value || '').trim().slice(0, 200),
      value: typeof el.value === 'string' ? el.value.slice(0, 200) : null,
      href: el.href || null,
      name: el.name || null,
      type: el.type || null,
      role: el.getAttribute('role') || null,
      ariaLabel: el.getAttribute('aria-label') || null,
      placeholder: el.placeholder || null,
      disabled: !!el.disabled,
      checked: typeof el.checked === 'boolean' ? el.checked : null,
      rect: {
        x: Math.round(rect.x), y: Math.round(rect.y),
        w: Math.round(rect.width), h: Math.round(rect.height)
      },
      selector: __abcCssPath(el)
    });
  }

  return { ok: true, matched, returned: items.length, items };
}
