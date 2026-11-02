// 页面内等待：
//   selector / text —— MutationObserver 监听 DOM 变化，命中即返回；
//   js             —— 谓词只编译一次，之后在页面里反复求值：DOM 变化由
//                     MutationObserver 立即触发，非 DOM 条件（时间、全局变量、
//                     网络回调）由定时器兜底，daemon 只注入一次。
// 后台标签页里 MutationObserver 不会被节流，这是它相对 setTimeout 轮询的关键优势；
// 定时器仅作兜底，被节流到 1s 也不影响 DOM 类条件的响应速度。
function __abcWait(opts) {
  opts = opts || {};
  const now = () => (typeof performance !== 'undefined' && performance.now ? performance.now() : Date.now());
  const started = now();
  const timeoutMs = opts.timeoutMs > 0 ? Math.floor(opts.timeoutMs) : 10000;
  const selector = opts.selector ? String(opts.selector) : '';
  const text = opts.text ? String(opts.text) : '';
  const jsCode = opts.js ? String(opts.js) : '';
  const visible = opts.visible === true;
  const minIntervalMs = opts.minIntervalMs >= 0 ? Math.floor(opts.minIntervalMs) : 50;

  let observer = null;
  let timer = null;
  let ticker = null;
  let done = false;
  let checks = 0;
  let lastCheckAt = 0;

  function checkSelector() {
    let el = null;
    try {
      el = document.querySelector(selector);
    } catch (e) {
      return { error: 'invalid selector: ' + selector };
    }
    if (!el) return { matched: false };
    if (!visible) return { matched: true, node: el };
    const rect = el.getBoundingClientRect();
    let style = null;
    try {
      style = getComputedStyle(el);
    } catch (_) {}
    const shown = rect.width > 0 && rect.height > 0 &&
      (!style || (style.visibility !== 'hidden' && style.display !== 'none'));
    return { matched: shown, node: el };
  }

  // --text 的取文本口径：
  //   innerText    —— 尊重可见性，语义最准，但读取会**强制整页布局**；
  //   textContent  —— 不触发布局，但会把 display:none 与 <script>/<style> 里的
  //                   文本也算进来，口径更宽。
  // 26 万节点的 DOM 上单次 innerText 可达秒级，而命中不了时每 minIntervalMs
  // （默认 50ms）就要巡检一次，超时时间会被布局开销整个吃光，表现成「等很久还是
  // 超时」。所以只在超大 DOM 降级为 textContent，并把实际口径回传（text_match_mode）。
  // 阈值取 20000：常见内容站点的元素总数只在数千量级；2 万以上基本都是长列表 /
  // 大表格这类「整页布局本身就很贵」的页面，此时放宽口径的代价远小于布局开销。
  const TEXT_INNER_LIMIT = 20000;
  let domNodeCount = null;
  let textMatchMode = null;

  /// 只量一次：阈值仅用于选口径，一次 wait 期间页面规模基本稳定；
  /// 每次巡检都遍历一遍 DOM 反而固定给大页面再加一笔开销。
  function countNodes() {
    if (domNodeCount !== null) return domNodeCount;
    try {
      domNodeCount = document.getElementsByTagName('*').length;
    } catch (_) {
      domNodeCount = 0;
    }
    return domNodeCount;
  }

  function checkText() {
    const body = document.body;
    if (!body) return { matched: false };
    const degraded = countNodes() > TEXT_INNER_LIMIT;
    textMatchMode = degraded ? 'textContent' : 'innerText';
    const content = degraded
      ? String(body.textContent || '')
      : String(body.innerText || body.textContent || '');
    return { matched: content.indexOf(text) >= 0 };
  }

  function evaluate() {
    if (selector) return checkSelector();
    if (text) return checkText();
    return { matched: false, error: 'wait: 需要 selector / text / js' };
  }

  // 谓词编译：三种写法都要支持——语句级 return、裸表达式、多语句 + await。
  // 编译只做一次；放在 Promise 外面，CSP 页面（禁 unsafe-eval）会同步抛错，
  // 上层据此走 CDP 回退。
  function compilePredicate(code) {
    const trimmed = String(code || '').trim();
    if (!trimmed) throw new Error('wait: js 为空');
    const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
    if (/^return\b/.test(trimmed)) return new AsyncFunction(trimmed);
    try {
      return new AsyncFunction('return (' + trimmed + '\n);');
    } catch (_) {}
    return new AsyncFunction(trimmed);
  }

  const predicate = jsCode ? compilePredicate(jsCode) : null;
  const mode = jsCode ? 'js-observer' : 'observer';

  return new Promise((resolve) => {
    let lastError = null;
    let lastValue = null;
    let running = false;

    function cleanup() {
      if (observer) {
        try { observer.disconnect(); } catch (_) {}
        observer = null;
      }
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      if (ticker) {
        clearInterval(ticker);
        ticker = null;
      }
      if (jsCode) {
        try { document.removeEventListener('visibilitychange', onMutation, true); } catch (_) {}
      }
    }

    function finish(matched, extra) {
      if (done) return;
      done = true;
      cleanup();
      const payload = Object.assign({
        ok: matched,
        matched: matched,
        polls: checks,
        checks: checks,
        mode: mode,
        elapsed_ms: Math.round((now() - started) * 10) / 10
      }, extra || {});
      // 仅 --text 条件会填：本次命中判定实际用的是 innerText 还是（超大 DOM 降级后的）
      // textContent。只新增字段，mode / checks 等既有字段保持原样。
      if (textMatchMode) payload.text_match_mode = textMatchMode;
      resolve(payload);
    }

    function describeValue(value) {
      try {
        if (value && value.nodeType === 1) {
          const rect = value.getBoundingClientRect();
          return {
            tag: String(value.tagName || '').toLowerCase(),
            id: value.id || undefined,
            text: String(value.textContent || '').trim().slice(0, 200),
            rect: {
              x: Math.round(rect.x),
              y: Math.round(rect.y),
              w: Math.round(rect.width),
              h: Math.round(rect.height)
            }
          };
        }
      } catch (_) {}
      return value;
    }

    function runPredicate() {
      if (done || running) return;
      running = true;
      lastCheckAt = now();
      checks += 1;
      let pending;
      try {
        pending = predicate();
      } catch (e) {
        running = false;
        lastError = (e && e.message) || String(e);
        return;
      }
      Promise.resolve(pending).then(function (value) {
        running = false;
        lastValue = value;
        lastError = null;
        if (value) finish(true, { value: describeValue(value) });
      }, function (err) {
        running = false;
        lastError = (err && err.message) || String(err);
      });
    }

    // 观察器可能在一次 DOM 变更里连发几十个回调：用微任务合并同一批变更。
    // 这里不能用 setTimeout 做节流——后台标签页会把它节流到 1s，
    // 明明 DOM 已经变了却要晚一秒才求值。
    let pendingMicro = false;
    function onMutation() {
      if (done || running) return;
      const gap = minIntervalMs - (now() - lastCheckAt);
      if (gap <= 0) {
        runPredicate();
        return;
      }
      if (pendingMicro) return;
      pendingMicro = true;
      queueMicrotask(function () {
        pendingMicro = false;
        if (done) return;
        runPredicate();
      });
    }

    function tick(force) {
      if (done) return;
      const current = now();
      if (!force && minIntervalMs > 0 && current - lastCheckAt < minIntervalMs) return;
      lastCheckAt = current;
      checks += 1;
      const result = evaluate();
      if (result.error) return finish(false, { error: result.error });
      if (!result.matched) return;
      const extra = { value: true };
      if (result.node) {
        try {
          const rect = result.node.getBoundingClientRect();
          extra.value = {
            tag: String(result.node.tagName || '').toLowerCase(),
            rect: {
              x: Math.round(rect.x),
              y: Math.round(rect.y),
              w: Math.round(rect.width),
              h: Math.round(rect.height)
            }
          };
        } catch (_) {}
      }
      return finish(true, extra);
    }

    if (jsCode) {
      runPredicate();
    } else {
      tick(true);
    }
    if (done) return;

    const root = document.documentElement || document;
    try {
      observer = new MutationObserver(jsCode ? onMutation : () => tick(false));
      observer.observe(root, {
        childList: true,
        subtree: true,
        attributes: true,
        characterData: true
      });
    } catch (_) {
      // 极端情况下（document 尚未就绪）退回定时检查，仍比 daemon 轮询便宜。
      observer = null;
    }

    if (jsCode) {
      // 非 DOM 条件兜底：定时器 + 标签页可见性变化。
      ticker = setInterval(runPredicate, Math.max(minIntervalMs, 16));
      try {
        document.addEventListener('visibilitychange', onMutation, true);
      } catch (_) {}
    }

    // 兜底定时器：只用于「一直不满足」时按时返回，不参与命中判断。
    timer = setTimeout(function () {
      finish(false, { error: lastError || 'timeout', reason: 'timeout', value: lastValue });
    }, timeoutMs);
  });
}
