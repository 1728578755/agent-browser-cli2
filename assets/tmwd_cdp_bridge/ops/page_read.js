// 页面结构化读取：一次注入返回正文、标题、元信息、表单与链接统计。
// 只做只读操作，不触发页面副作用。
//
// 性能约定（大页面）：
// - 文本、计数、标题、链接、表单字段都在一次遍历里完成，不做 5~6 次 querySelectorAll；
// - 元素数超过 visibleInnerTextLimit 时不再用 innerText（需要整页布局），
//   改用 TreeWalker 近似可见性，并在 text_mode 里标记 "approx"；
// - 全程受 budgetMs / maxNodes 约束，超限时返回 partial:true，而不是把页面卡死。
function __abcRead(opts) {
  opts = opts || {};
  const doc = document;
  const root = opts.selector ? doc.querySelector(opts.selector) : doc.body;
  if (!root) return { ok: false, error: 'selector not found: ' + opts.selector };

  const now = () => (typeof performance !== 'undefined' && performance.now ? performance.now() : Date.now());
  const started = now();
  // 参数约定：用户显式给的值一律照办，只有「缺省 / 非法 / <= 0」才落默认。
  // 以前 maxNodes / innerTextLimit 写的是「小于 1000 就当默认」，于是 --max-nodes 200
  // 反而拿到 60000 上限——用户想省 token，结果被静默放大，属于典型的「假成功」。
  const maxChars = opts.maxChars > 0 ? Math.floor(opts.maxChars) : 20000;
  // budgetMs 是唯一保留下限的：低于 10ms 这一趟基本读不到内容，等于返回空串。
  // 但下限不是静默的——被调整时在返回体里给 clamped / requested。
  const MIN_BUDGET_MS = 10;
  const requestedBudgetMs = Math.floor(Number(opts.budgetMs));
  const budgetClamped = requestedBudgetMs > 0 && requestedBudgetMs < MIN_BUDGET_MS;
  const budgetMs = requestedBudgetMs > 0 ? Math.max(requestedBudgetMs, MIN_BUDGET_MS) : 2000;
  const requestedMaxNodes = Math.floor(Number(opts.maxNodes));
  const maxNodes = requestedMaxNodes > 0 ? requestedMaxNodes : 60000;
  const requestedInnerTextLimit = Math.floor(Number(opts.innerTextLimit));
  const innerTextLimit = requestedInnerTextLimit > 0 ? requestedInnerTextLimit : 30000;
  const includeHidden = opts.includeHidden === true || opts.mode === 'all';
  const deadline = started + budgetMs;

  const BLOCK_TAGS = new Set([
    'ADDRESS', 'ARTICLE', 'ASIDE', 'BLOCKQUOTE', 'BR', 'DD', 'DETAILS', 'DIV', 'DL', 'DT',
    'FIELDSET', 'FIGCAPTION', 'FIGURE', 'FOOTER', 'FORM', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6',
    'HEADER', 'HR', 'LI', 'MAIN', 'NAV', 'OL', 'P', 'PRE', 'SECTION', 'SUMMARY', 'TABLE',
    'TBODY', 'TD', 'TFOOT', 'TH', 'THEAD', 'TR', 'UL'
  ]);
  const SKIP_TAGS = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'HEAD', 'META', 'LINK', 'TITLE']);

  function normalize(text) {
    return String(text || '')
      .replace(/\r\n?/g, '\n')
      .replace(/[ \t\f\v\u00a0]+/g, ' ')
      .replace(/ ?\n ?/g, '\n')
      .replace(/\n{3,}/g, '\n\n')
      .trim();
  }

  function clipped(text, max) {
    const value = String(text || '').replace(/\s+/g, ' ').trim();
    return value.length > max ? value.slice(0, max) : value;
  }

  // 走一次 TreeWalker 收集文本；includeAll=false 时粗略跳过明显隐藏的子树。
  function collectText(includeAll) {
    const chunks = [];
    let length = 0;
    let visited = 0;
    let truncated = false;
    const walker = doc.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT, {
      acceptNode(node) {
        if (node.nodeType === 3) return NodeFilter.FILTER_ACCEPT;
        const tag = node.tagName;
        if (SKIP_TAGS.has(tag)) return NodeFilter.FILTER_REJECT;
        if (!includeAll) {
          if (node.hidden === true) return NodeFilter.FILTER_REJECT;
          if (node.getAttribute('aria-hidden') === 'true') return NodeFilter.FILTER_REJECT;
          const inline = node.getAttribute('style');
          if (inline && /display\s*:\s*none|visibility\s*:\s*hidden/i.test(inline)) {
            return NodeFilter.FILTER_REJECT;
          }
        }
        return NodeFilter.FILTER_ACCEPT;
      }
    });
    let node = walker.nextNode();
    while (node) {
      visited += 1;
      if (visited > maxNodes) { truncated = true; break; }
      if ((visited & 511) === 0 && now() > deadline) { truncated = true; break; }
      if (node.nodeType === 1) {
        if (BLOCK_TAGS.has(node.tagName) && chunks.length) chunks.push('\n');
      } else {
        const raw = node.nodeValue;
        if (raw) {
          const value = raw.replace(/[ \t\f\v\u00a0]+/g, ' ');
          if (value.trim() || (chunks.length && !/\n$/.test(chunks[chunks.length - 1]))) {
            chunks.push(value);
            length += value.length;
          }
        }
      }
      if (length >= maxChars * 3) { truncated = true; break; }
      node = walker.nextNode();
    }
    return { text: normalize(chunks.join('')), truncated, visited };
  }

  const nodeCount = (root.getElementsByTagName ? root.getElementsByTagName('*').length : 0) + 1;
  let text = '';
  let textMode = includeHidden ? 'all' : 'visible';
  let textTruncated = false;
  let visited = 0;

  if (includeHidden) {
    const got = collectText(true);
    text = got.text;
    textTruncated = got.truncated;
    visited = got.visited;
  } else if (nodeCount <= innerTextLimit) {
    // 小页面：innerText 最准（会排除 CSS 隐藏内容），一次布局可以接受。
    text = normalize(root.innerText || root.textContent || '');
  } else {
    const got = collectText(false);
    text = got.text;
    textTruncated = got.truncated;
    visited = got.visited;
    textMode = 'visible-approx';
  }

  // 单次查询覆盖计数 / 标题 / 链接 / 表单，避免多次全量遍历。
  const SELECTOR = 'a[href],img,input,textarea,select,button,form,h1,h2,h3,[role="button"]';
  const counts = { links: 0, images: 0, inputs: 0, buttons: 0, forms: 0 };
  const headings = [];
  const wantLinks = opts.links === true;
  const wantForms = opts.forms !== false;
  const links = [];
  const forms = [];
  let scanned = 0;
  let partial = textTruncated || nodeCount > maxNodes;

  const elements = root.querySelectorAll(SELECTOR);
  for (const el of elements) {
    scanned += 1;
    if (scanned > maxNodes) { partial = true; break; }
    if ((scanned & 255) === 0 && now() > deadline) { partial = true; break; }
    const tag = el.tagName;
    if (tag === 'A') {
      counts.links += 1;
      if (wantLinks && links.length < 100) {
        links.push({ text: clipped(el.textContent, 120), href: el.href });
      }
    } else if (tag === 'IMG') {
      counts.images += 1;
    } else if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') {
      counts.inputs += 1;
    } else if (tag === 'BUTTON' || el.getAttribute('role') === 'button') {
      counts.buttons += 1;
    } else if (tag === 'FORM') {
      counts.forms += 1;
      if (wantForms && forms.length < 20) {
        const fields = [];
        for (const f of el.querySelectorAll('input,textarea,select')) {
          if (fields.length >= 50) break;
          fields.push({
            name: f.name || '',
            type: (f.type || f.tagName).toLowerCase(),
            value: typeof f.value === 'string' ? f.value.slice(0, 200) : '',
            placeholder: f.placeholder || '',
            required: !!f.required
          });
        }
        forms.push({ action: el.action || '', method: (el.method || 'get').toUpperCase(), fields });
      }
    } else if (tag === 'H1' || tag === 'H2' || tag === 'H3') {
      if (headings.length < 50) {
        const value = clipped(el.textContent, 200);
        if (value) headings.push({ tag: tag.toLowerCase(), text: value });
      }
    }
  }

  const meta = {};
  for (const m of doc.querySelectorAll('meta[name],meta[property]')) {
    const key = m.getAttribute('name') || m.getAttribute('property');
    const value = m.getAttribute('content');
    if (key && value && !(key in meta)) meta[key] = String(value).slice(0, 300);
  }

  const out = {
    ok: true,
    url: location.href,
    title: doc.title,
    lang: doc.documentElement.lang || '',
    description: meta.description || meta['og:description'] || '',
    textLength: text.length,
    text: text.slice(0, maxChars),
    truncated: text.length > maxChars || textTruncated,
    text_mode: textMode,
    partial,
    headings,
    counts,
    stats: {
      node_count: nodeCount,
      scanned,
      visited,
      elapsed_ms: Math.round((now() - started) * 100) / 100,
      budget_ms: budgetMs,
      max_nodes: maxNodes
    }
  };

  if (wantLinks) out.links = links;
  if (wantForms) out.forms = forms;
  // 只在实际调整过用户显式值时才出现，避免「改了数还不说」。
  if (budgetClamped) {
    out.clamped = true;
    out.requested = { budget_ms: requestedBudgetMs };
  }
  return out;
}
