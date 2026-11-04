
// 识别方式：标题关键词 + 特征 DOM + 脚本/iframe 来源 + 少量正文兜底。
// 全部走 querySelector / document.scripts；只有正文兜底会遍历文本，且累计 2000 字符即停。
function __abcChallenge(opts) {
  opts = opts || {};
  const action = String(opts.action || 'status').toLowerCase();
  const now = () => (typeof performance !== 'undefined' && performance.now ? performance.now() : Date.now());

  // 标题关键词分两档：
  //   强档 —— CF 挑战页的专属文案，正常站点不会拿它当标题；
  //   弱档 —— "One more step" 是无数营销/结账页的标题，"Access Denied" 是各站 403 页的
  //           通用标题。只凭标题就判拦截，会把普通页面挂进 challenge wait 一直等到超时，
  //           还把调用方的归因引向「需要人工过验证码」。弱档只记 suspected，
  //           必须等到 DOM / 脚本实锤才升级。
  const STRONG_TITLE_HINTS = [
    [/just a moment/i, 'cloudflare-interstitial'],
    [/attention required/i, 'cloudflare-interstitial'],
    [/checking your browser/i, 'cloudflare-interstitial'],
    [/verifying you are human/i, 'cloudflare-interstitial'],
    [/请稍候|正在检查|正在验证/i, 'cloudflare-interstitial']
  ];
  const WEAK_TITLE_HINTS = [
    [/one more step/i, 'cloudflare-interstitial'],
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

  /// CF「真的还在挑战」的脚本证据。
  ///
  /// 不能用 `cdn-cgi/challenge-platform` 当判据：CF 会给**每一个**受保护站点注入
  /// `/cdn-cgi/challenge-platform/h/b/scripts/jsd/main.js`（隐形探测信标），
  /// 正常页面也带它。拿它判拦截，等于把 kick.com 这种已放行的站点永远当拦截页，
  /// wait 会一直等到超时（实测 30s 白等，页面其实早已正常渲染）。
  ///
  /// 真正的挑战页加载的是 orchestrate 下的编排脚本（chl_page / managed / challenge），
  /// 放行后这段脚本随挑战文档一起消失，判据才有效。
  function challengeScript() {
    const scripts = document.scripts || [];
    for (let i = 0; i < scripts.length; i += 1) {
      const src = String(scripts[i].src || '');
      if (src.indexOf('challenge-platform') < 0) continue;
      if (src.indexOf('/scripts/jsd/') >= 0) continue;
      if (src.indexOf('/orchestrate/') >= 0) return src;
    }
    return '';
  }

  /// CF 硬拦截页（"Sorry, you have been blocked"）：等多久都没用，不是「自动放行」类。
  function isHardBlock() {
    if (has('.cf-error-details') || has('#cf-error-details')) return true;
    try {
      const text = String((document.body && document.body.textContent) || '').slice(0, 4000);
      return /you have been blocked|has been blocked|access denied|error 1020|error 1015/i.test(text);
    } catch (_) {
      return false;
    }
  }

  function windowHas(name) {
    try { return name in window; } catch (_) { return false; }
  }

  /// CF 编排脚本在页面上声明「这一页要走哪种挑战」。
  ///
  /// `cType: "interactive"` 表示站点**一定会**渲染一个可交互的 Turnstile 组件（勾选框）。
  /// 这个字段是页面自己的配置，不是猜的：只要它在，页面上就有组件要处理——
  /// 哪怕 `querySelectorAll('iframe')` 一个都看不见。
  ///
  /// 为什么看不见：CF 现在把组件挂在一个 **closed shadow root** 里，
  /// 页面 JS 的 querySelector / elementFromPoint 都穿不透它（elementFromPoint 只会
  /// 返回宿主 DIV）。只有 CDP 的 pierce DOM 能看到里面的 iframe。
  /// 早期实现拿「iframe 数为 0」当作「站点没下发组件」，于是在这种页面上永远
  /// 判 `stalled: turnstile_not_rendered`，直接放弃 —— 实际上组件就在那儿，等你去点。
  function challengeCType() {
    try {
      const opt = window._cf_chl_opt;
      return opt && opt.cType ? String(opt.cType).toLowerCase() : '';
    } catch (_) {
      return '';
    }
  }

  function vendorOfSrc(src) {
    const value = String(src || '');
    if (value.indexOf('challenges.cloudflare.com') >= 0) return 'turnstile';
    if (value.indexOf('recaptcha') >= 0) return 'recaptcha';
    if (value.indexOf('hcaptcha') >= 0) return 'hcaptcha';
    // DataDome 的验证 iframe 挂在 geo.captcha-delivery.com 上，域名里没有 "datadome" 字样；
    // 先按 captcha-delivery 认，否则会掉进下面的 generic 分支，厂商名就丢了。
    if (value.indexOf('datadome') >= 0 || value.indexOf('captcha-delivery') >= 0) return 'datadome';
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
        // 风控 SDK 常在正常页面里挂 0×0 的隐形 iframe（datadome / awswaf 都这么干）。
        // 把这种也算「页面上有验证码组件」，会让 open 报 need_human，属于假警报。
        if (box.width < 16 || box.height < 16) continue;
        const style = window.getComputedStyle(frame);
        if (style && (style.display === 'none' || style.visibility === 'hidden')) continue;
        rect = {
          x: Math.round(box.x),
          y: Math.round(box.y),
          w: Math.round(box.width),
          h: Math.round(box.height)
        };
      } catch (_) {
        continue;
      }
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
  // 点完勾选框后 CF 会切到「Verifying you are human. This may take a few seconds.」。
  // 这一步常常要 3-10 秒，期间再点一次会把流程打回原点 —— 调用方必须知道「正在验证」。
  const VERIFYING_HINT = /verifying you are human|this may take a few seconds|正在验证|请稍候/i;
  let lastVerifyScanAt = 0;
  function mayScanVerifying() {
    const at = now();
    if (lastVerifyScanAt && at - lastVerifyScanAt < TEXT_SCAN_INTERVAL_MS) return false;
    lastVerifyScanAt = at;
    return true;
  }
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

    for (const entry of STRONG_TITLE_HINTS) {
      if (entry[0].test(title)) {
        hit(entry[1], 12, 'interstitial', true, false, true, 'title:' + title.slice(0, 40));
      }
    }
    let suspected = null;
    for (const entry of WEAK_TITLE_HINTS) {
      if (entry[0].test(title)) suspected = entry[1];
    }

    // 先把「等也没用」的硬拦截判掉，避免 wait 把 30s 预算全耗在一个死页上。
    const hardBlock = isHardBlock() && (!!vendor || /attention required|access denied|blocked/i.test(title));

    // Cloudflare 的 JS 挑战页（会自动放行，值得等）
    if (has('#cf-challenge-running') || has('#challenge-form') || has('#challenge-stage') || has('#cf-please-wait')) {
      hit('cloudflare-interstitial', 11, 'interstitial', true, false, true, 'cf-dom');
    }
    // 注意：这里**只能**用「看得见的拦截页证据」判定 blocking。
    // CF 会在正常页面上也预置 `_cf_chl_opt` 引导对象、也注入 challenge-platform 脚本，
    // 拿它们当判据会把已放行的站点永久判成拦截页（实测 kick.com 放行后仍报 blocking=true，
    // challenge wait 白等 30s）。真正还在拦的时候，标题与 #challenge-stage 一定在。
    const cfScript = challengeScript();
    if (cfScript) {
      signals.push('cf-orchestrate');
    }
    // 弱标题只有等到实锤才升级：CF 的 "One more step" 页面一定带 orchestrate 编排脚本
    // 或 #challenge-stage；Akamai 的 "Access Denied" 一定带 sec-cpt 容器/脚本。
    // 两样都没有就只是一个措辞像拦截页的普通页面，按普通页面处理。
    if (suspected && !best) {
      if (suspected === 'cloudflare-interstitial' && (cfScript || has('#challenge-stage') || has('#cf-challenge-running'))) {
        hit('cloudflare-interstitial', 12, 'interstitial', true, false, true, 'title+cf-dom:' + title.slice(0, 40));
      } else if (suspected === 'akamai' && (has('#sec-if-cpt-container') || scriptHas('sec-cpt'))) {
        hit('akamai', 12, 'interstitial', true, true, false, 'title+akamai-dom:' + title.slice(0, 40));
      } else {
        signals.push('title-only-suspect:' + title.slice(0, 40));
      }
    }
    if (hardBlock) {
      hit('cloudflare-block', 13, 'blocked', true, false, false, 'cf-hard-block');
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

    // 滑块 / 点选类
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

    // 会整页拦截的风控：同样只认「看得见的挑战 UI」。
    // 各家 SDK 脚本（px-cloud / sec-cpt / awswaf / datadome）在正常页面上也常驻，
    // 拿它们判拦截的话，cloudflare 之外的风控会成片误报（实测 coinmarketcap 正常渲染
    // 却被判 aws-waf 拦截）。脚本只记进 signals 供排障，不参与 blocking 判定。
    if (scriptHas('px-cloud')) signals.push('px-script');
    if (scriptHas('sec-cpt')) signals.push('akamai-script');
    if (scriptHas('awswaf')) signals.push('aws-waf-script');
    if (scriptHas('datadome') || scriptHas('captcha-delivery')) signals.push('datadome-script');
    if (has('#px-captcha')) {
      hit('perimeterx', 10, 'interstitial', true, true, false, 'perimeterx');
    }
    if (has('#sec-if-cpt-container')) {
      hit('akamai', 10, 'interstitial', true, true, false, 'akamai');
    }
    if (has('#aws-captcha-container')) {
      hit('aws-waf', 9, 'interstitial', true, true, false, 'aws-waf');
    }
    if (has('#datadome-captcha')) {
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
      const first = frames[0];
      vendor = first.vendor;
      const rect = first.rect || { w: 0, h: 0 };
      const vw = window.innerWidth || 0;
      const vh = window.innerHeight || 0;
      // 铺满视口的验证码（DataDome / PX 的拦截页就是这样）挡的是整个页面，
      // 跟「页面里挂了个小勾选框」是两码事：按 widget 处理会让 open 报
      // 「可正常浏览」，调用方以为进去了，其实页面上一个字都读不到。
      const coversViewport = vw > 0 && vh > 0 && rect.w >= vw * 0.6 && rect.h >= vh * 0.6;
      if (coversViewport) {
        kind = 'interstitial';
        blocking = true;
        interactive = true;
        auto = false;
      } else {
        kind = 'widget';
        interactive = true;
      }
      signals.push('iframe:' + first.vendor + (coversViewport ? '(fullscreen)' : ''));
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
    if (kind === 'blocked') {
      hints.push('CF 硬拦截（IP/环境被判定为风险）：等待无效，需要换出口 IP 或提高环境可信度后重试');
    } else if (blocking && auto) {
      hints.push('拦截式挑战页：通常几秒内自动放行，用 challenge wait 等待即可');
    } else if (blocking && !auto) {
      hints.push('拦截式挑战页且需要人工：challenge focus 把标签页带到前台，人完成后 challenge wait 会继续');
    }
    if (!blocking && interactive) {
      hints.push(kind === 'slider'
        ? '滑块/点选类验证码：需要人工拖动或点选，先 challenge focus 把标签页带到前台，处理完再继续'
        : '页面里存在验证码组件：可正常浏览，提交前需要人完成');
    }
    if (!vendor && suspected) {
      hints.push('标题像拦截页（' + suspected + '）但没有任何 DOM/脚本证据：按普通页面处理，不要按验证码等待');
    } else if (!vendor) {
      hints.push('未检测到验证码');
    }

    // 「正在验证」只在已判定被拦时才有意义，且只扫一次（1s 节流）。
    let verifying = false;
    if (blocking && document.readyState !== 'loading' && mayScanVerifying()) {
      verifying = VERIFYING_HINT.test(bodyTextHead(TEXT_SCAN_LIMIT));
    }

    // 环境信号。CF 的 Turnstile 在隐藏页面 / 零尺寸窗口里根本不会执行：
    // `--background` 打开时窗口往往是**最小化**的，此时 `outerWidth=0`、
    // `visibilityState='hidden'`，挑战脚本跑不起来也不会自己恢复。把这些信号
    // 报出去，调用方才能区分「挑战还在进行」和「这个环境不可能过」。
    const env = {
      visibility: String(document.visibilityState || ''),
      hidden: !!document.hidden,
      has_focus: typeof document.hasFocus === 'function' ? !!document.hasFocus() : null,
      outer: [Number(window.outerWidth) || 0, Number(window.outerHeight) || 0],
      inner: [Number(window.innerWidth) || 0, Number(window.innerHeight) || 0]
    };

    // Turnstile 到底有没有真的渲染出来。CF 的托管挑战挂死时的典型形态是：
    // api.js 已加载（`window.turnstile` 存在）、`_cf_chl_opt` 也在，但页面上
    // **没有任何 `challenges.cloudflare.com` 的 iframe**，也没有 `#challenge-stage`
    // —— 脚本拿到了 iframe 的 HTML（200）却不插入 DOM。这时等再久也不会放行。
    let tsIframes = 0;
    try {
      tsIframes = Array.prototype.filter.call(
        document.querySelectorAll('iframe'),
        (f) => /challenges\.cloudflare\.com/i.test(String((f && f.src) || ''))
      ).length;
    } catch (_) {}
    const cType = challengeCType();
    // 组件挂载点：Turnstile 渲染时会往页面写一个隐藏的 `cf-turnstile-response`。
    // 它的存在证明「页面上确实有一个 Turnstile 实例」，比 `_cf_chl_opt.cType` 稳 ——
    // 后者在编排脚本跑起来之后会被混淆成随机键名，稳态下读不到。
    let tokenFieldPresent = false;
    try {
      tokenFieldPresent = !!document.querySelector(
        '[name="cf-turnstile-response"], [id*="cf-chl-widget"][id$="_response"]'
      );
    } catch (_) {}
    const hasStage = !!document.querySelector('#challenge-stage');
    // 「组件藏在 closed shadow root 里」：CF 交出了 Turnstile 实例（api + 隐藏响应字段），
    // 但页面 JS 看不到 iframe / stage —— 只能说明它被放进了 closed shadow root，
    // 不是「站点没下发组件」。
    // 判据只用 DOM：ISOLATED world 里读不到 window.turnstile（那是页面全局），
    // 但「有没有 Turnstile 的隐藏响应字段」在 DOM 里一目了然。
    const shadowWidget = kind === 'interstitial'
      && tsIframes === 0
      && !hasStage
      && (cType === 'interactive' || tokenFieldPresent);
    if (shadowWidget) {
      // 有组件就必须点，别按「等它自己放行」处理 —— 这正是旧实现失败的根因。
      interactive = true;
      auto = true;
      signals.push('cf-ctype:interactive(shadow-widget)');
    }
    const turnstile = {
      api: !!(window.turnstile && typeof window.turnstile === 'object'),
      iframes: tsIframes,
      stage: hasStage,
      ctype: cType || null,
      shadow_widget: shadowWidget,
      response_field: tokenFieldPresent
    };

    // 不在 window 上记时间戳。
    // 挑战页由 CF 全程监控，任何「凭空出现的全局变量」都是现成的自动化指纹：
    // 之前这里写 window.__abcChlSeenAt，人工点能过、脚本点过不去，差别就在这里。
    // stalled 的判定改由 daemon 侧按「连续多少次 status 都是同一个挑战」来算。
    const stalledMs = typeof opts.stalledMs === 'number' ? opts.stalledMs : 0;
    // 静默挂死：判定为拦截已经超过 10s，Turnstile api 在却没有 iframe / stage，
    // 页面上也没有可点控件 —— 这是 CF 不下发挑战，不是「还在验证中」。
    // shadowWidget 是例外：组件确实在（藏在 closed shadow root 里），只是页面 JS 看不到，
    // 这种情况要交给 CDP 去点，不能报「没下发组件」然后放弃。
    const stalled = !!vendor && !interactive && stalledMs >= 10000
      && turnstile.api && turnstile.iframes === 0 && !turnstile.stage;

    if (shadowWidget) {
      // 「几秒内自动放行」的前提是页面上没有可交互组件，这里正好相反：有勾选框要人点。
      const naive = hints.findIndex((item) => String(item).indexOf('通常几秒内自动放行') >= 0);
      if (naive >= 0) hints.splice(naive, 1);
      hints.unshift('验证组件在 closed shadow root 里（页面 JS 查不到 iframe/尺寸）：需要 CDP 定位后点勾选框，challenge solve / open --challenge auto 会自动处理');
    }
    if (stalled) {
      // 「通常几秒内自动放行」和下面的结论直接矛盾，去掉它：两条并排摆着，
      // 用户只会更犹豫（到底该等还是该换环境）。
      const naive = hints.findIndex((item) => String(item).indexOf('通常几秒内自动放行') >= 0);
      if (naive >= 0) hints.splice(naive, 1);
      hints.push('站点未下发 Turnstile 组件（api 已加载，但页面无组件、无 #challenge-stage）：换出口 IP / 换浏览器环境再试，或确认站点的 Turnstile 配置 —— 等更久不会放行');
    }
    if (env.hidden || env.outer[0] === 0 || env.outer[1] === 0) {
      hints.push('窗口不可见（最小化/被遮挡）：CF 不会在这种页面里完成验证，先 challenge focus 恢复窗口再重试');
    }

    const tokens = tokenState();
    return {
      detected: !!vendor,
      // 「标题疑似拦截页但无实锤」单独标出来：调用方据此走普通页面路径，
      // 而不是白等一轮 challenge wait。
      suspected: !!suspected && !vendor,
      verifying: verifying,
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
      hints: hints,
      env: env,
      turnstile: turnstile,
      stalled_ms: stalledMs,
      stalled: stalled
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
        // 页面上挂着**已知厂商**的验证码组件、但还没拿到 token：继续等，别当成已放行。
        //
        // `blocking=false` 只表示「页面能浏览」，不表示「挑战已过」：登录页 / 结账页上
        // 挂着一个没勾的 Turnstile 就是这种形态。旧实现里 solve 模式还会在这里等 token，
        // 而 wait 模式（waitToken=false）直接落到下面的 `!blocking` 分支，250ms 后就
        // 返回 cleared:true —— 调用方以为进去了，提交时才被拦。
        //
        // 只对「识别出厂商」的组件这么做：`interactive` 是启发式判据，没有厂商佐证时
        // 宁可沿用旧行为，免得把正常页面判成永远过不去（假阴性比假阳性更难排查）。
        if (current.vendor && current.interactive && !current.token_ready) {
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

      // 本来就没被拦（且没有未通过的组件）：也要确认一小段，避免把「导航中的空白文档」
      // 当成已放行。
      if (!current.pending && !current.blocking && !(current.vendor && current.interactive && !current.token_ready)) {
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

// 实现标记（与 page_net / page_stealth 同一套）：challenge 现在跑在
// MAIN world（要读 window.turnstile / window._cf_chl_opt），入口全局对页面可见。
// 页面若抢先定义同名函数，pageOp 会因缺少 __abcOwn 而拒绝执行页面提供的实现。
try { Object.defineProperty(__abcChallenge, '__abcOwn', { value: true }); } catch (_) {}
try { window.__abcChallenge = __abcChallenge; } catch (_) {}
