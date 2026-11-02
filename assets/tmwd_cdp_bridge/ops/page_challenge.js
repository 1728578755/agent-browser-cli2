// 挑战页 / 验证码识别：只做「识别 + 等待 + 转人工」，不做自动破解。
//
// 为什么不做自动破解：
//   1. 验证码/CF 挑战的目的就是区分人和脚本，自动点掉或调用打码平台属于绕过对方风控；
//   2. 真正高频的场景其实是「CF 的 JS 挑战会在几秒内自动放行」——那只需要等，不需要解；
//   3. 需要人点的时候（Turnstile 复选框、图形/滑块验证码），把标签页带到前台、
//      告诉调用方「这里需要人」比假装能解更可靠。
//
// 识别方式：标题关键词 + 特征 DOM + 脚本/iframe 来源 + 少量正文兜底。
// 全部走 querySelector / document.scripts；只有正文兜底会遍历文本，且累计 2000 字符即停。
function __abcChallenge(opts) {
  opts = opts || {};
  const action = String(opts.action || 'status').toLowerCase();
  const now = () => (typeof performance !== 'undefined' && performance.now ? performance.now() : Date.now());

  const TITLE_HINTS = [
    [/just a moment/i, 'cloudflare-interstitial'],
    [/attention required/i, 'cloudflare-interstitial'],
    [/checking your browser/i, 'cloudflare-interstitial'],
    [/verifying you are human/i, 'cloudflare-interstitial'],
    [/one more step/i, 'cloudflare-interstitial'],
    [/请稍候|正在检查|正在验证/i, 'cloudflare-interstitial'],
    [/access denied/i, 'akamai']
  ];

  function has(selector) {
    try { return !!document.querySelector(selector); } catch (_) { return false; }
  }

  function scriptHas(fragment) {
    const scripts = document.scripts || [];
    for (let i = 0; i < scripts.length; i += 1) {
      if (String(scripts[i].src || '').indexOf(fragment) >= 0) return true;
    }
    return false;
  }

  function windowHas(name) {
    try { return name in window; } catch (_) { return false; }
  }

  function vendorOfSrc(src) {
    const value = String(src || '');
    if (value.indexOf('challenges.cloudflare.com') >= 0) return 'turnstile';
    if (value.indexOf('recaptcha') >= 0) return 'recaptcha';
    if (value.indexOf('hcaptcha') >= 0) return 'hcaptcha';
    if (value.indexOf('datadome') >= 0) return 'datadome';
    if (value.indexOf('px-cdn') >= 0 || value.indexOf('captcha.px') >= 0) return 'perimeterx';
    if (value.indexOf('awswaf') >= 0) return 'aws-waf';
    if (value.indexOf('captcha') >= 0) return 'generic';
    return null;
  }

  /// 只收集「和验证码有关」的 iframe，带坐标，便于转人工时告诉人去哪点。
  function captchaFrames() {
    const out = [];
    let frames = [];
    try { frames = document.querySelectorAll('iframe'); } catch (_) { return out; }
    for (let i = 0; i < frames.length && out.length < 8; i += 1) {
      const frame = frames[i];
      const vendor = vendorOfSrc(frame.src);
      if (!vendor) continue;
      let rect = null;
      try {
        const box = frame.getBoundingClientRect();
        rect = {
          x: Math.round(box.x),
          y: Math.round(box.y),
          w: Math.round(box.width),
          h: Math.round(box.height)
        };
      } catch (_) {}
      out.push({ vendor: vendor, src: String(frame.src || '').slice(0, 300), rect: rect });
    }
    return out;
  }

  /// 验证码的「通过令牌」：Turnstile / reCAPTCHA / hCaptcha 完成后都会往隐藏字段写 token。
  /// 这是最可靠的「人已经点完」信号，比盯 iframe 消失靠谱。
  const TOKEN_SELECTORS = [
    '[name="cf-turnstile-response"]',
    '[name="g-recaptcha-response"]',
    '[name="h-captcha-response"]',
    '#g-recaptcha-response',
    'textarea[name="g-recaptcha-response"]',
    'textarea[name="h-captcha-response"]'
  ];

  function tokenState() {
    const fields = [];
    // 同一个元素可能同时命中 [name=...] 与 #id，用 Set 去重，别把字段列重复。
    const seen = new Set();
    for (const selector of TOKEN_SELECTORS) {
      let nodes = [];
      try { nodes = document.querySelectorAll(selector); } catch (_) { continue; }
      for (const node of nodes) {
        if (seen.has(node)) continue;
        seen.add(node);
        const value = String(node.value || '');
        fields.push({ name: node.getAttribute('name') || node.id || selector, length: value.length });
      }
    }
    // 通用兜底：任何名字像 token 的隐藏字段有值也算（各家 SDK 命名不一）
    try {
      const candidates = document.querySelectorAll('input[type="hidden"]');
      for (const node of candidates) {
        if (seen.has(node)) continue;
        const name = String(node.getAttribute('name') || '');
        if (!/captcha|turnstile|recaptcha|hcaptcha/i.test(name)) continue;
        if (!/token|response|answer/i.test(name)) continue;
        seen.add(node);
        fields.push({ name: name, length: String(node.value || '').length });
      }
    } catch (_) {}
    return {
      token_ready: fields.some((item) => item.length > 0),
      token_fields: fields
    };
  }

  /// 正文兜底的代价控制。
  ///
  /// 之前这里直接读 document.body.textContent 再 slice(0, 2000)：切片发生在**整页序列化之后**，
  /// 每次判断都要把几十万节点拼成一整个字符串。challenge wait 每 250ms 判断一次，
  /// 26 万节点的页面上 30 秒要序列化 120 遍，足以把页面卡死。
  /// 现在改为 TreeWalker 边走边停，凑够 2000 字符就收工；
  /// 且只在「文档已加载完 + 其它特征全没命中」时才扫，并用时间戳门控到最多每 1s 一次。
  const TEXT_SCAN_LIMIT = 2000;
  const TEXT_SCAN_INTERVAL_MS = 1000;
  const TEXT_SCAN_MAX_NODES = 20000;
  const BODY_TEXT_HINT = /verify you are human|checking if the site connection is secure|确认您是真人|请完成安全验证/i;
  let lastTextScanAt = 0;
  // 命中后保持粘性：否则被节流跳过的那次判断会把「拦截页」误判成「已放行」。
  let bodyTextMatched = false;

  function mayScanBodyText() {
    const at = now();
    if (lastTextScanAt && at - lastTextScanAt < TEXT_SCAN_INTERVAL_MS) return false;
    lastTextScanAt = at;
    return true;
  }

  /// 取正文开头的少量文本。shadow DOM / iframe 里的文本看不到，保持「尽力而为」。
  function bodyTextHead(limit) {
    const body = document.body;
    if (!body) return '';
    if (typeof document.createTreeWalker !== 'function' || typeof NodeFilter === 'undefined') {
      try { return String(body.textContent || '').slice(0, limit); } catch (_) { return ''; }
    }
    let out = '';
    let steps = 0;
    try {
      const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT, null, false);
      let node = walker.nextNode();
      while (node && out.length < limit && steps < TEXT_SCAN_MAX_NODES) {
        if (node.nodeValue) out += node.nodeValue;
        node = walker.nextNode();
        steps += 1;
      }
    } catch (_) {}
    return out.length > limit ? out.slice(0, limit) : out;
  }

  function detect() {
    const title = String(document.title || '');
    const url = String(location.href || '');
    const signals = [];
    let vendor = null;
    let kind = 'unknown';
    let blocking = false;
    let interactive = false;
    let auto = false;
    let best = 0;

    function hit(id, score, nextKind, isBlocking, isInteractive, isAuto, why) {
      if (score <= best) return;
      best = score;
      vendor = id;
      kind = nextKind;
      blocking = isBlocking;
      interactive = isInteractive;
      auto = isAuto;
      signals.push(why);
    }

    for (const entry of TITLE_HINTS) {
      if (entry[0].test(title)) {
        hit(entry[1], 12, 'interstitial', true, false, true, 'title:' + title.slice(0, 40));
      }
    }

    // Cloudflare 的 JS 挑战页（会自动放行，值得等）
    if (has('#cf-challenge-running') || has('#challenge-form') || has('#challenge-stage') || has('#cf-please-wait')) {
      hit('cloudflare-interstitial', 11, 'interstitial', true, false, true, 'cf-dom');
    }
    if (scriptHas('cdn-cgi/challenge-platform')) {
      hit('cloudflare-interstitial', 9, 'interstitial', true, false, true, 'cf-script');
    }
    if (windowHas('_cf_chl_opt')) {
      hit('cloudflare-interstitial', 9, 'interstitial', true, false, true, 'cf-chl-opt');
    }

    // 页面上的验证码组件（不阻断浏览，但提交前必须完成）
    if (has('.cf-turnstile') || has('[data-sitekey].cf-turnstile')) {
      hit('turnstile', 8, 'widget', false, true, false, 'turnstile-widget');
    }
    if (has('.g-recaptcha') || scriptHas('recaptcha')) {
      hit('recaptcha', 7, 'widget', false, true, false, 'recaptcha');
    }
    if (has('.h-captcha') || scriptHas('hcaptcha.com')) {
      hit('hcaptcha', 7, 'widget', false, true, false, 'hcaptcha');
    }

    // 滑块 / 点选类（需要人工拖动或点选，识别出来但不当成「可等自动放行」）
    if (has('#nc_1_n1z') || has('.nc-container') || scriptHas('aliyuncaptcha') || scriptHas('AWSC/')) {
      hit('aliyun-nocaptcha', 9, 'slider', false, true, false, 'aliyun-slider');
    }
    if (has('#gt_slider') || has('.geetest_holder') || has('.geetest_slider') || scriptHas('geetest')) {
      hit('geetest', 9, 'slider', false, true, false, 'geetest-slider');
    }
    if (has('#tcaptcha_transform_dy') || scriptHas('turing.captcha.qcloud')) {
      hit('tencent-captcha', 9, 'slider', false, true, false, 'tencent-captcha');
    }
    if (has('#dx_captcha') || scriptHas('dingxiang-inc')) {
      hit('dingxiang', 8, 'slider', false, true, false, 'dingxiang');
    }
    if (has('.sm-captcha') || scriptHas('fengkongcloud')) {
      hit('shumei', 8, 'slider', false, true, false, 'shumei');
    }

    // 会整页拦截的风控
    if (has('#px-captcha') || scriptHas('px-cloud')) {
      hit('perimeterx', 10, 'interstitial', true, true, false, 'perimeterx');
    }
    if (has('#sec-if-cpt-container') || scriptHas('sec-cpt')) {
      hit('akamai', 10, 'interstitial', true, true, false, 'akamai');
    }
    if (has('#aws-captcha-container') || scriptHas('awswaf')) {
      hit('aws-waf', 9, 'interstitial', true, true, false, 'aws-waf');
    }
    if (has('#datadome-captcha') || scriptHas('datadome') || scriptHas('captcha-delivery')) {
      hit('datadome', 9, 'interstitial', true, true, false, 'datadome');
    }

    // 兜底：正文里的验证码图片 / 输入框
    if (!vendor && has('img[src*="captcha" i]')) {
      hit('generic', 5, 'image', false, true, false, 'captcha-image');
    }
    if (!vendor && has('input[name*="captcha" i]')) {
      hit('generic', 4, 'image', false, true, false, 'captcha-input');
    }

    // 文档还在解析时不能下结论：导航刚提交的空白文档既没有挑战标记，
    // 也不是「已放行」，必须先当作 pending。
    const pending = document.readyState === 'loading';

    const frames = captchaFrames();
    if (!vendor && frames.length) {
      vendor = frames[0].vendor;
      kind = 'widget';
      interactive = true;
      signals.push('iframe:' + frames[0].vendor);
    }

    // 标题兜底：标题空/很短的拦截页，正文里出现「verify you are human」也算。
    // 门槛很高（要遍历正文文本），所以三重门控：文档已加载完 + 上面所有特征都没命中 + 最多每 1s 扫一次。
    if (!vendor && document.readyState === 'complete' && (bodyTextMatched || mayScanBodyText())) {
      if (!bodyTextMatched) {
        bodyTextMatched = BODY_TEXT_HINT.test(bodyTextHead(TEXT_SCAN_LIMIT));
      }
      if (bodyTextMatched) {
        hit('cloudflare-interstitial', 6, 'interstitial', true, false, true, 'body-text');
      }
    }

    const hints = [];
    if (blocking && auto) {
      hints.push('拦截式挑战页：通常几秒内自动放行，用 challenge wait 等待即可');
    }
    if (blocking && !auto) {
      hints.push('拦截式挑战页且需要人工：challenge focus 把标签页带到前台，人完成后 challenge wait 会继续');
    }
    if (!blocking && interactive) {
      hints.push(kind === 'slider'
        ? '滑块/点选类验证码：需要人工拖动或点选，先 challenge focus 把标签页带到前台，处理完再继续'
        : '页面里存在验证码组件：可正常浏览，提交前需要人完成');
    }
    if (!vendor) {
      hints.push('未检测到验证码');
    }

    const tokens = tokenState();
    return {
      detected: !!vendor,
      pending: pending,
      token_ready: tokens.token_ready,
      token_fields: tokens.token_fields,
      vendor: vendor,
      kind: kind,
      blocking: blocking,
      interactive: interactive,
      auto: auto,
      title: title,
      url: url,
      frames: frames,
      signals: signals,
      hints: hints
    };
  }

  if (action === 'status' || action === 'detect') {
    const result = detect();
    result.ok = true;
    result.action = 'status';
    return result;
  }

  if (action === 'wait') {
    const timeoutMs = opts.timeoutMs > 0 ? Math.floor(opts.timeoutMs) : 30000;
    const intervalMs = opts.intervalMs > 0 ? Math.floor(opts.intervalMs) : 250;
    const focusAfterMs = opts.focusAfterMs >= 0 ? Math.floor(opts.focusAfterMs) : 0;
    // 「清除」要稳定一小段时间才算数：导航刚提交时页面可能还是空白文档，
    // 一眼看上去「没有挑战」，但下一个任务里真正的挑战页才渲染出来。
    const confirmMs = opts.confirmMs > 0 ? Math.floor(opts.confirmMs) : 250;
    // solve 用：等「验证码通过令牌」出现（人点完 / 隐形挑战自动完成）
    const waitToken = opts.waitToken === true;
    return new Promise(function (resolve) {
      const started = now();
      let checks = 0;
      let done = false;
      let observer = null;
      let timer = null;
      let timeout = null;
      let lastCheckAt = 0;
      let clearSince = 0;
      let current = detect();

      function cleanup() {
        if (observer) { try { observer.disconnect(); } catch (_) {} observer = null; }
        if (timer) { clearInterval(timer); timer = null; }
        if (timeout) { clearTimeout(timeout); timeout = null; }
      }

      function finish(cleared, extra) {
        if (done) return;
        done = true;
        cleanup();
        resolve(Object.assign({
          ok: true,
          action: 'wait',
          cleared: cleared,
          vendor: current.vendor,
          blocking: current.blocking,
          interactive: current.interactive,
          checks: checks,
          elapsed_ms: Math.round(now() - started)
        }, extra || {}));
      }

      function check(force) {
        if (done) return;
        const at = now();
        if (!force && at - lastCheckAt < intervalMs) return;
        lastCheckAt = at;
        checks += 1;
        current = detect();
        if (current.pending) {
          // 还在加载：既不算拦截，也不算清除，继续等。
          clearSince = 0;
          return;
        }
        if (waitToken && current.token_ready) {
          return finish(true, {
            via: 'token',
            vendor: current.vendor,
            token_fields: current.token_fields
          });
        }
        if (waitToken && current.interactive) {
          // solve 模式且页面上确实有验证码：等 token，别当成「已放行」。
          clearSince = 0;
          return;
        }
        if (!current.blocking) {
          if (!clearSince) clearSince = at;
          if (at - clearSince >= confirmMs) {
            return finish(true, { vendor: current.vendor, interactive: current.interactive });
          }
          return;
        }
        clearSince = 0;
        if (focusAfterMs > 0 && (at - started) >= focusAfterMs) {
          return finish(false, {
            reason: 'need_human',
            need_human: true,
            vendor: current.vendor,
            hints: current.hints
          });
        }
      }

      // 本来就没被拦：也要确认一小段，避免把「导航中的空白文档」当成已放行。
      if (!current.pending && !current.blocking && !(waitToken && current.interactive && !current.token_ready)) {
        clearSince = now();
        setTimeout(function () { check(true); }, confirmMs + 10);
      }

      try {
        observer = new MutationObserver(function () { check(false); });
        observer.observe(document.documentElement || document, {
          childList: true,
          subtree: true,
          attributes: true,
          characterData: true
        });
      } catch (_) {
        observer = null;
      }
      timer = setInterval(function () { check(false); }, intervalMs);
      timeout = setTimeout(function () {
        finish(false, { reason: 'timeout', hints: current.hints });
      }, timeoutMs);
    });
  }

  return { ok: false, error: 'challenge: 未知 action ' + action };
}
