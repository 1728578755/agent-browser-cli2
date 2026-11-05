// 元素查找：按 CSS 选择器或可见文本定位，返回可操作信息（含简短 CSS 路径）。
// 不点击、不修改，供智能体先"看清有哪些元素"再动手。
//
// ---- shadow DOM / iframe 边界----
// 遍历口径：本文件把 **open** shadow root 一起扫进来（spec.shadowDom = auto|off，
// 默认 auto）。closed shadow root 在页面 JS 里完全不可达（host.shadowRoot === null），
// 只能靠 CDP 的 pierce 查询，回包用 shadow_mode / shadow_skipped 如实标注，
// 不假装「页面上没有元素」（旧实现返回 ok:true, matched:0，属于静默假成功）。
//
// 选择器：shadow 内元素无法用 document.querySelector 定位，回包给
//   selector: "<host 文档路径> >>> <shadow 内相对路径>"
// 外加 pierce:true / host_selector / shadow_path；page_action.js 的 __abcResolve
// 认这种写法（逐层进 shadow root）。`>>>` 不是合法 CSS，不会误命中主文档元素。
//
// iframe：跨文档穿透不在本脚本能力内（需要逐 frame 注入或 CDP），
// 回包用 frame_count + scope:"main-frame-only" 标明「还有内容没扫」，不静默假成功。
//
// CSS 路径必须**能唯一定位**：
// 只按 depth <= 4 往上走、且只有撞上 id 才停，会得到一个相对选择器
// （`div.body > ul > li:nth-of-type(2) > a`），它在文档任何同构位置都成立，
// 而 querySelector 返回文档序第一个匹配 —— 快照 @e37 指向第 3 张卡片的按钮，
// 后续 click 却命中第 1 张卡片的同位按钮，还返回 ok:true。
// 现在每加一层就用 querySelectorAll 验一次「唯一」，不唯一就继续往上加，
// 直到 documentElement（带 nth-of-type 的全路径必然唯一）。
// 唯一性判定必须在**元素所属的 root** 内做：shadow 里的 `button.ok` 在主文档里
// 可能同名，用 document 的计数去判会把相对路径误判成全局唯一。

/// shadow 遍历上限：组件库站点常见几十个 shadow root，超过 200 个基本是异常页面，
/// 再扫下去只是把一帧的预算烧光。
const __ABC_FIND_SHADOW_MAX_ROOTS = 200;
const __ABC_FIND_SHADOW_MAX_DEPTH = 8;

/// `spec.shadowDom`：auto（默认，扫 open shadow root）/ off（只扫主文档）。
/// 未识别的值按 auto 处理——默认行为必须是「尽量看见」而不是「假装没有」。
function __abcFindShadowMode(spec) {
  const raw = String((spec && spec.shadowDom) || '').toLowerCase();
  return raw === 'off' ? 'off' : 'auto';
}

/// 元素所属的 root：Document（nodeType 9）或 ShadowRoot（nodeType 11）。
function __abcFindRootOf(el) {
  try {
    const root = el && el.getRootNode ? el.getRootNode() : null;
    return root && (root.nodeType === 9 || root.nodeType === 11) ? root : document;
  } catch (_) {
    return document;
  }
}

/// 收集页面里所有 open shadow root（按宿主文档序），并统计「看得见但没扫」的数量。
/// skipped 只可能来自两种情形：mode=off，或超过 roots/depth 上限。
function __abcFindShadowRoots(mode) {
  const out = [];
  let skipped = 0;
  if (mode === 'off') {
    // off 时也要数一遍宿主：否则回包无法区分「页面没有 shadow」与「我们没看」。
    try {
      for (const el of document.querySelectorAll('*')) {
        if (el.shadowRoot) skipped += 1;
      }
    } catch (_) {}
    return { roots: out, skipped: skipped };
  }
  const queue = [{ root: document, depth: 0 }];
  while (queue.length) {
    const item = queue.shift();
    let hosts = [];
    try { hosts = item.root.querySelectorAll('*'); } catch (_) { hosts = []; }
    for (const el of hosts) {
      let shadow = null;
      try { shadow = el.shadowRoot; } catch (_) { shadow = null; }
      if (!shadow) continue; // closed root 在这里同样是 null，页面 JS 不可达
      if (item.depth >= __ABC_FIND_SHADOW_MAX_DEPTH || out.length >= __ABC_FIND_SHADOW_MAX_ROOTS) {
        skipped += 1;
        continue;
      }
      out.push(shadow);
      queue.push({ root: shadow, depth: item.depth + 1 });
    }
  }
  return { roots: out, skipped: skipped };
}

/// 扫描范围列表：主文档在前，open shadow root 依次在后。
function __abcFindScopes(mode) {
  const found = __abcFindShadowRoots(mode);
  return {
    scopes: [document].concat(found.roots),
    shadowSkipped: found.skipped,
    shadowRoots: found.roots.length
  };
}

/// iframe / frame 数量：这些文档的内容本脚本看不到，必须如实回报。
function __abcFindFrameCount() {
  try { return document.querySelectorAll('iframe,frame').length; } catch (_) { return 0; }
}

/// 在指定 root 内生成唯一 CSS 路径（root 为 Document 或 ShadowRoot）。
function __abcFindCssPathIn(el, root) {
  if (!el || el.nodeType !== 1) return '';
  if (!root || typeof root.querySelectorAll !== 'function') root = document;
  const unique = (path) => {
    try { return root.querySelectorAll(path).length === 1; } catch (_) { return false; }
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

/// 完整路径：主文档元素返回单段；shadow 内元素返回「最近可 pierce 的宿主锚点 + 相对路径」。
function __abcFindPath(el) {
  if (!el || el.nodeType !== 1) return { selector: '', scope: 'main' };
  const chain = [];
  let node = el;
  let guard = 0;
  while (node && node.nodeType === 1 && guard < __ABC_FIND_SHADOW_MAX_DEPTH) {
    guard += 1;
    const root = __abcFindRootOf(node);
    chain.unshift(__abcFindCssPathIn(node, root));
    // Document 没有 host；ShadowRoot 有。到顶就停。
    if (!root || root.nodeType !== 11 || !root.host) break;
    node = root.host;
  }
  if (chain.length <= 1) return { selector: chain[0] || '', scope: 'main' };
  return {
    selector: chain.join(' >>> '),
    host_selector: chain[0],
    shadow_path: chain.slice(1),
    scope: 'shadow',
    pierce: true
  };
}

/// 兼容旧调用点（其它脚本/测试只认字符串路径）。
function __abcCssPath(el) {
  return __abcFindPath(el).selector;
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

/// 派生文本（placeholder / aria-label / 输入框的无障碍名）的掩码口径。
/// find / read / snapshot 三处必须一致：
/// 空值保持 null（调用方据此判断"字段没写"），secret 字段的非空值一律 '•••'。
function __abcMaskDerived(el, value) {
  const raw = value === undefined || value === null ? '' : String(value);
  if (!__abcSecretField(el)) return raw || null;
  return raw ? '•••' : null;
}

/// `host >>> inner` 解析：逐层进 open shadow root（与 page_action.js 的 __abcResolve
/// 同一套写法）。find 自己产出的 selector 必须能被 find 再吃回去，否则
/// 「find → 拿 selector → 再 find 校验」这条常见回路会报 invalid_selector。
function __abcFindPierce(selector) {
  const parts = String(selector || '').split('>>>').map((item) => item.trim()).filter(Boolean);
  if (parts.length < 2) return null;
  let el = document.querySelector(parts[0]);
  for (let i = 1; i < parts.length; i += 1) {
    if (!el) return null;
    let shadow = null;
    try { shadow = el.shadowRoot; } catch (_) { shadow = null; }
    if (!shadow) return null; // closed shadow root：页面 JS 不可达
    el = shadow.querySelector(parts[i]);
  }
  return el || null;
}

function __abcFind(spec) {
  spec = spec || {};
  const limit = Math.max(1, Math.min(Number(spec.limit) || 50, 200));
  const visibleOnly = spec.visible !== false;
  const shadowMode = __abcFindShadowMode(spec);
  const scopeInfo = __abcFindScopes(shadowMode);
  let candidates = [];

  // 非法 selector / xpath 会抛 DOMException，旧实现让它直接冒到通道层：
  // 调用方只能看到 "pageOp failed"，分不清是选择器写错了还是注入失败。
  // 口径与 page_read.js / page_wait.js 的 `invalid selector:` 保持一致。
  try {
    if (spec.selector) {
      const rawSelector = String(spec.selector);
      if (rawSelector.indexOf('>>>') >= 0) {
        // 自己产出的 pierce 选择器（"host >>> inner"）：直接逐层进 shadow root。
        const hit = __abcFindPierce(rawSelector);
        if (hit) candidates = [hit];
      } else {
        // 每个 scope 各查一次：shadow 内元素不会被 document.querySelectorAll 命中。
        for (const scope of scopeInfo.scopes) {
          const hit = scope.querySelectorAll(rawSelector);
          for (const el of hit) candidates.push(el);
        }
      }
    } else if (spec.text) {
      const needle = String(spec.text).toLowerCase();
      const exact = !!spec.exact;
      const hits = [];
      const ancestors = new Set();
      let nodeCount = null;
      for (const scope of scopeInfo.scopes) {
        const all = scope.querySelectorAll('*');
        for (const el of all) {
          // 只看近叶子节点，避免整段容器都命中同一句文本。
          if (el.children.length > 3) continue;
          if (nodeCount === null) {
            try { nodeCount = document.getElementsByTagName('*').length; } catch (_) { nodeCount = 0; }
          }
          const text = nodeCount <= __ABC_FIND_TEXT_INNER_LIMIT
            ? (el.innerText || el.textContent || '')
            : (el.textContent || '');
          const t = String(text).trim();
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
      }
      candidates = hits.filter(el => !ancestors.has(el));
    } else if (spec.xpath) {
      // XPath 没有跨 shadow 的作用域概念，只能在主文档里求值；
      // 回包用 scope / shadow_skipped 说明这一点，不假装扫过了。
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
    const text = secret ? '•••' : String(el.innerText || el.value || '').trim().slice(0, 200);
    const path = __abcFindPath(el);
    const item = {
      tag: el.tagName.toLowerCase(),
      text,
      value: typeof el.value === 'string' ? __abcMaskValue(el, el.value.slice(0, 200)) : null,
      href: el.href || null,
      name: el.name || null,
      type: el.type || null,
      role: el.getAttribute('role') || null,
      ariaLabel: __abcMaskDerived(el, el.getAttribute('aria-label')),
      placeholder: __abcMaskDerived(el, el.placeholder),
      disabled: !!el.disabled,
      checked: typeof el.checked === 'boolean' ? el.checked : null,
      redacted: secret || undefined,
      rect: {
        x: Math.round(rect.x), y: Math.round(rect.y),
        w: Math.round(rect.width), h: Math.round(rect.height)
      },
      selector: path.selector,
      // 作用域：main = 主文档；shadow = open shadow root 内（选择器需要 pierce）。
      scope: path.scope
    };
    if (path.pierce) {
      item.pierce = true;
      item.host_selector = path.host_selector;
      item.shadow_path = path.shadow_path;
    }
    items.push(item);
  }

  const out = {
    ok: true,
    matched,
    returned: items.length,
    items,
    // 扫描覆盖范围：只要页面上还有 iframe，就明确告诉调用方「这些文档没扫」。
    // 旧实现无条件当作「整页都查过了」，于是 iframe 里的控件被当成不存在。
    scope: scopeInfo.shadowRoots > 0 ? 'main-frame+shadow' : 'main-frame-only',
    frame_count: __abcFindFrameCount(),
    shadow_mode: shadowMode,
    shadow_roots: scopeInfo.shadowRoots,
    shadow_skipped: scopeInfo.shadowSkipped,
    // closed shadow root 无法枚举（host.shadowRoot 恒为 null）。只有在页面上确实见到了
    // open shadow root 时才标注「可能有 closed 的」：普通页面也标 true 会让调用方
    // 每次都判「覆盖不完整」，属于噪音而不是信息。
    shadow_closed_unreachable: shadowMode === 'auto' && scopeInfo.shadowRoots > 0
  };
  return out;
}
