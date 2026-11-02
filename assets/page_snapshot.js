// DOM 版快照：CDP 不可用（其他扩展 iframe 挡住 debugger）时，用一次页面注入
// 生成与 AX 快照同形的可操作元素列表，每个元素带可直接复用的 CSS 选择器。
// @e 引用在 daemon 侧会映射到这个选择器，后续动作走页面内执行，不再依赖 CDP。
function __abcSnapCssPath(el) {
  if (!el || el.nodeType !== 1) return '';
  if (el.id) return '#' + CSS.escape(el.id);
  const parts = [];
  let node = el;
  let depth = 0;
  while (node && node.nodeType === 1 && depth < 5) {
    let part = node.tagName.toLowerCase();
    const cls = (typeof node.className === 'string' ? node.className : '')
      .trim().split(/\s+/).filter(Boolean).slice(0, 2);
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

function __abcSnapRole(el) {
  const explicit = (el.getAttribute('role') || '').toLowerCase();
  if (explicit) return explicit;
  const tag = el.tagName.toLowerCase();
  if (tag === 'a') return el.hasAttribute('href') ? 'link' : 'generic';
  if (tag === 'button') return 'button';
  if (tag === 'select') return 'combobox';
  if (tag === 'textarea') return 'textbox';
  if (tag === 'img') return 'img';
  if (/^h[1-6]$/.test(tag)) return 'heading';
  if (tag === 'input') {
    const type = (el.type || 'text').toLowerCase();
    if (type === 'checkbox') return 'checkbox';
    if (type === 'radio') return 'radio';
    if (type === 'submit' || type === 'button' || type === 'reset') return 'button';
    if (type === 'search') return 'searchbox';
    if (type === 'file') return 'file';
    return 'textbox';
  }
  if (tag === 'label') return 'label';
  return 'generic';
}

function __abcSnapName(el) {
  const attr = (name) => (el.getAttribute(name) || '').trim();
  const aria = attr('aria-label');
  if (aria) return aria;
  const tag = el.tagName.toLowerCase();
  if (tag === 'input' || tag === 'textarea' || tag === 'select') {
    const placeholder = attr('placeholder');
    if (placeholder) return placeholder;
    if (el.type === 'submit' || el.type === 'button') return el.value || '';
    const labelled = el.id ? document.querySelector('label[for="' + CSS.escape(el.id) + '"]') : null;
    const labelText = labelled ? (labelled.innerText || '').trim() : '';
    if (labelText) return labelText;
    const wrapped = el.closest('label');
    if (wrapped) return (wrapped.innerText || '').trim();
    return attr('name');
  }
  if (tag === 'img') return attr('alt') || attr('title');
  const text = (el.innerText || el.textContent || '').trim().replace(/\s+/g, ' ');
  return text.slice(0, 160);
}

function __abcSnapVisible(el) {
  const rect = el.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) return false;
  const style = getComputedStyle(el);
  return style.visibility !== 'hidden' && style.display !== 'none' && Number(style.opacity) !== 0;
}

function __abcSnapshot(opts) {
  opts = opts || {};
  const maxItems = Math.max(1, Math.min(Number(opts.limit) || 400, 1000));
  const skipGeneric = opts.skipGeneric !== false;
  const interactiveOnly = opts.interactiveOnly === true;
  const OPERABLE = new Set(['button', 'link', 'textbox', 'checkbox', 'radio', 'combobox', 'searchbox', 'file', 'slider', 'switch', 'tab']);
  const items = [];
  let index = 0;
  const all = document.body ? document.body.querySelectorAll('*') : [];
  for (const el of all) {
    if (items.length >= maxItems) break;
    const tag = el.tagName.toLowerCase();
    if (tag === 'script' || tag === 'style' || tag === 'noscript' || tag === 'svg') continue;
    const role = __abcSnapRole(el);
    if (skipGeneric && role === 'generic') continue;
    if (interactiveOnly && !OPERABLE.has(role)) continue;
    if (role === 'img' && !el.getAttribute('alt')) continue;
    if (!__abcSnapVisible(el)) continue;
    index += 1;
    const name = __abcSnapName(el);
    if (!name && tag !== 'input') continue;
    const rect = el.getBoundingClientRect();
    const item = {
      ref: '@e' + index,
      role,
      name: String(name || '').slice(0, 160),
      tag,
      selector: __abcSnapCssPath(el),
      rect: {
        x: Math.round(rect.x), y: Math.round(rect.y),
        w: Math.round(rect.width), h: Math.round(rect.height)
      }
    };
    if (el.disabled) item.disabled = true;
    if (typeof el.checked === 'boolean' && (el.type === 'checkbox' || el.type === 'radio')) item.checked = el.checked;
    if (typeof el.value === 'string' && el.value && (tag === 'input' || tag === 'textarea' || tag === 'select')) {
      item.value = el.value.slice(0, 160);
    }
    if (tag === 'select') {
      item.options = Array.from(el.options).slice(0, 20).map(o => ({ value: o.value, text: (o.text || '').trim() }));
    }
    if (/^h[1-6]$/.test(tag)) item.level = Number(tag.slice(1));
    if (tag === 'a' && el.href) item.href = el.href;
    items.push(item);
  }
  return {
    ok: true,
    url: location.href,
    title: document.title,
    mode: 'js',
    total: items.length,
    truncated: items.length >= maxItems,
    items
  };
}
