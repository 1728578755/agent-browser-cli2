// DOM 版快照：CDP 不可用（其他扩展 iframe 挡住 debugger）时，用一次页面注入
// 生成与 AX 快照同形的可操作元素列表，每个元素带可直接复用的 CSS 选择器。
// @e 引用在 daemon 侧会映射到这个选择器，后续动作走页面内执行，不再依赖 CDP。
//
// 选择器必须**唯一**：长度上限 + 只在撞上 id 时锚定，得到的是
// 相对路径，在重复结构（卡片/列表/表格行）里指向第一个同构位置 —— @e 引用于是
// 静默点错元素且返回 ok:true。这里逐层验唯一，不唯一就继续往上加祖先。
//
// 已知边界（协议级限制，本轮不修）：CDP 引擎只给可操作角色发 `@e`
// （ref_scheme=operable-only），本脚本给所有「有名字」的结构节点发 `@e`
// （ref_scheme=all-named），非可操作节点（heading / img / label / generic 角色等）
// 也会占号。两个引擎的 `@eN` **不保证指向同一元素**，跨模式复用 `@e` 前必须重新
// snapshot；daemon 用回包里的 ref_scheme 区分口径。
function __abcSnapCssPath(el) {
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
    // 类名只保留在目标自身这层：祖先层用 nth-of-type 定位，不会因 class 变动失效。
    if (parts.length === 0) {
      const cls = (typeof node.className === 'string' ? node.className : '')
        .trim().split(/\s+/).filter(Boolean).slice(0, 2);
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
    if (node && node.nodeType === 1 && node.id) {
      const anchored = '#' + CSS.escape(node.id) + ' > ' + parts.join(' > ');
      if (unique(anchored)) return anchored;
    }
  }
  return parts.join(' > ');
}

/// 口令类字段的值不返回明文：快照会进 CLI 输出 / agent 上下文。
function __abcSnapSecretField(el) {
  if (!el || el.nodeType !== 1) return false;
  const type = String(el.type || '').toLowerCase();
  if (type === 'password') return true;
  const autocomplete = String(el.getAttribute && el.getAttribute('autocomplete') || '').toLowerCase();
  return /current-password|new-password|cc-number|cc-csc|one-time-code/.test(autocomplete);
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

// 可见性判断顺带把 rect 返回给调用方：回包里要用同一个 rect，以前在产出阶段
// 又调一次 getBoundingClientRect（E1-14：同一元素每轮只读一次布局）。
function __abcSnapRect(el) {
  const rect = el.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) return null;
  const style = getComputedStyle(el);
  if (style.visibility === 'hidden' || style.display === 'none' || Number(style.opacity) === 0) return null;
  return rect;
}

function __abcSnapshot(opts) {
  opts = opts || {};
  const maxItems = Math.max(1, Math.min(Number(opts.limit) || 400, 1000));
  // `offset`：跳过前 N 个可操作元素，从第 N+1 个开始返回（0 基，与 CLI 的 --offset 同义）。
  // 旧实现完全忽略 offset、永远从文档开头取 limit 条：daemon 侧只能本地切片，
  // offset≥1000 时页面只给前 1000 条，切完一条不剩（P3 深分页）。
  const offset = Math.max(0, Math.floor(Number(opts.offset) || 0));
  const skipGeneric = opts.skipGeneric !== false;
  const interactiveOnly = opts.interactiveOnly === true;
  // XML / SVG 文档没有 body：旧实现拿到空列表后照样返回 ok:true + 0 条，
  // 调用方于是以为「页面上没有可操作元素」，实际是这份脚本根本没看到内容（P3）。
  // 结构不可用必须明确报错，而不是用一个空成功掩盖。
  if (!document.body) {
    return {
      ok: false,
      error: 'document has no body',
      error_code: 'no_document_body',
      url: location.href,
      title: document.title,
      mode: 'js',
      // JS 侧编号口径，见文件头「已知边界」。
      ref_scheme: 'all-named',
      total: 0,
      truncated: false,
      items: []
    };
  }
  // 与 Rust `is_operable_role` 对齐的 18 个角色（此前只有 11 个，
  // interactiveOnly 会漏掉 listbox / menuitem* / option / spinbutton / treeitem）。
  const OPERABLE = new Set([
    'button', 'link', 'textbox', 'searchbox', 'checkbox', 'radio', 'combobox',
    'listbox', 'menuitem', 'menuitemcheckbox', 'menuitemradio', 'option',
    'slider', 'spinbutton', 'switch', 'tab', 'treeitem', 'file'
  ]);
  const items = [];
  // index 是**全页序号**：分页时也必须连续，`@e` 引用才能跨页稳定、不重号。
  let index = 0;
  let hasMore = false;
  const all = document.body.querySelectorAll('*');
  for (const el of all) {
    const tag = el.tagName.toLowerCase();
    // E1-14：便宜的过滤（tag / role / skipGeneric / interactiveOnly / img-alt）先做，
    // 深分页时为前 offset 条省掉 CSS 选择器与回包对象的构造（name 仍要算，
    // 见下面的「占号 == 产出」约束）。
    if (tag === 'script' || tag === 'style' || tag === 'noscript' || tag === 'svg') continue;
    const role = __abcSnapRole(el);
    if (skipGeneric && role === 'generic') continue;
    if (interactiveOnly && !OPERABLE.has(role)) continue;
    if (role === 'img' && !el.getAttribute('alt')) continue;
    // 序号口径是「只有可见元素占号」，而 @e 的全页编号就靠这个计数。
    // 因此可见性判断不能延后到 offset 之后：前缀里只要有一个隐藏元素
    // （input[type=hidden]、折叠菜单里的 a[href] 都是），盲计数就会让后续
    // @e 整体偏移、跨页复用点错元素。这里省掉的是产出开销（selector/对象构造），
    // 不是序号正确性。
    const rect = __abcSnapRect(el);
    if (!rect) continue;
    // E1-14b：序号只给「会产出」的元素 —— 必须在 index += 1 之前先过 name 判断，
    // 否则「可见但无 name 且非 input」的元素占号却不进 items，index 与 items 数量
    // 脱钩，next_offset = offset + items.length 就会小于真实覆盖位置，翻页会重复
    // 返回同一个 @e（指向同一元素，但调用方会误以为还有新内容，深分页可能空转）。
    // input 例外：即使没有 name 也要占号并产出（保持历史行为，文件型/隐藏控件依赖它）。
    const name = __abcSnapName(el);
    if (!name && tag !== 'input') continue;
    index += 1;
    // 先按 offset 跳过（序号照常累加），再判断本页是否已满。
    if (index <= offset) continue;
    // 本页已满但仍有下一个命中项：说明确实还有下一批（不是「刚好取完」）。
    if (items.length >= maxItems) { hasMore = true; break; }
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
      // 口令类字段只回传占位符：快照会被写进 CLI 输出与 agent 对话上下文。
      if (__abcSnapSecretField(el)) {
        item.value = '•••';
        item.redacted = true;
      } else {
        item.value = el.value.slice(0, 160);
      }
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
    // 与 CDP 引擎（operable-only）区分编号口径。
    ref_scheme: 'all-named',
    // 显式回传分页窗口：daemon 侧据此确认「这一批的起点」而不是靠猜（P3）。
    offset: offset,
    limit: maxItems,
    returned: items.length,
    // 命中总数只在本批就是最后一批时才是确定值，因此单独用 total_known 标注。
    total: hasMore ? null : index,
    total_known: !hasMore,
    has_more: hasMore,
    // next_offset 与「已消费候选数」严格对齐（复核）：序号只在通过全部过滤
    // （含 name 判定，E1-14b）之后才 +1，所以 offset+1 .. offset+items.length 这段
    // 一定是连续产出、没有空洞 —— 下一次从 next_offset+1 开始既不重复也不跳号。
    // 最后一批时 total === offset + items.length，与 items.length 自洽；还有下一批
    // 时 total 为 null（未知），不存在「total 与 items 不一致」的情形，因此不需要
    // 改成 offset+index：index 已是全页绝对序号，再叠 offset 会双重计算。
    next_offset: hasMore ? offset + items.length : null,
    truncated: hasMore,
    items
  };
}
