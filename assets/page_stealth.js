// 自动化痕迹审计与自查清理（stealth audit / scrub）。
//
// 定位：本工具不伪造指纹、不改 canvas/WebGL/UA —— 那是另一类问题（伪装成别人），
// 而且改坏了自己先出 bug。这里只做两件事：
//   1. audit：把「当前页面能被站点用来判断访客是自动化」的信号逐条列出来，
//      并标明责任方（本工具 / 浏览器启动方式 / 未知），给可执行的修复建议；
//   2. scrub：只清除**本工具自己留下的**痕迹（注入的全局、页面角标、被包装的 fetch/XHR）。
//
// 为什么单独一个文件、且在 MAIN world 跑：
//   站点能看到的只有 MAIN world 的 window。ISOLATED world 里的同名变量它根本读不到，
//   所以审计必须在 MAIN world 做，才能得到「站点视角」的结论。
//
// 所有检查都是只读的（scrub 除外），不写 cookie、不发请求、不改 UA。
//
// 入口定义：整个文件包在 IIFE 里，末尾用 defineProperty 把 __abcStealth 挂到 window。
// 与 page_net.js 同因同理：顶层 `function __abcStealth(){}` 在扩展 executeScript({files})
// 这条 classic script 路径上会创建 non-configurable 的全局绑定（DONT_DELETE 位），
// `delete window.__abcStealth` 恒返回 false —— 审计函数自己就成了永远擦不掉的指纹；
// 而且函数声明是可枚举的，`Object.keys(window)` 一行就能列出来。
;(function () {
function __abcStealth(opts) {
  opts = opts || {};
  const action = String(opts.action || 'audit').toLowerCase();

  // 本工具可能留在页面里的全局名。改这里必须同步 background.js 的 PAGE_OP_FILES。
  //
  // 后两个不是 `__abc*`，但同样是本工具注入的入口：simphtml 的 scan / find_list
  // 脚本（daemon eval 通道，见 html.rs）把 optHTML / findMainList 临时挂到 window
  // 供同一段脚本里的 `return optHTML(...)` 解析。它们已经改成
  // 不可枚举 + 可配置 + 用完即撤；列在这里是为了「撤不掉时」audit 能报、scrub 能删 ——
  // 只列 `__abc*` 会让这两条完全漏网（历史问题：audit 报「干净」，页面其实留着入口）。
  const OWN_GLOBALS = [
    '__abcStealth',
    '__abcNet',
    '__abcRead',
    '__abcFind',
    '__abcSnapshot',
    '__abcAct',
    '__abcWait',
    '__abcChallenge',
    'optHTML',
    'findMainList'
  ];
  // 本工具可能留在 DOM 里的节点（扩展角标与其样式表）。
  // atb-status-host 是 5.0 起的 Shadow DOM 宿主，atb-* 是历史版本留下的节点。
  const OWN_DOM_IDS = ['atb-ind', 'atb-style', 'atb-status-host'];

  function text(value, limit) {
    const out = String(value === undefined || value === null ? '' : value);
    return out.length > (limit || 200) ? out.slice(0, limit || 200) + '…' : out;
  }

  function safe(fn, fallback) {
    try {
      return fn();
    } catch (_) {
      return fallback;
    }
  }

  /// 全局是否“真的存在”。
  ///
  /// 不用 `typeof window[name] !== undefined`：页面把 __abcNet 赋成 undefined 之后，
  /// typeof 判成“不存在”，但站点用 getOwnPropertyNames 照样能看见这个键 —— 那就是假干净。
  function ownGlobalPresent(name) {
    return safe(function () { return Object.prototype.hasOwnProperty.call(window, name); }, false);
  }

  /// 审计/清理函数本身也是痕迹：任务结束后立刻从 window 上消失，
  /// 下次调用会由扩展重新注入（多一次 executeScript，几毫秒）。
  function eraseSelfSoon() {
    setTimeout(function () {
      safe(function () { delete window.__abcStealth; });
    }, 0);
  }

  /// 网络钩子的状态槽名字。
  ///
  /// 5.0 起 page_net.js 把状态藏进不可枚举属性，名字是 `__atb_net_` 前缀 + 随机后缀，
  /// 用来躲避 `Object.getOwnPropertyNames` 之外的直读；旧版本叫 `__abcNetState`。
  /// 两代都要能识别，否则「升级后审计说页面干净」是个假结论。
  // page_net.js 建槽位时用的名字形状：__atb_net_ + 8 位小写字母数字。
  // 必须加锚点：旧版本用过没有锚点的前缀匹配，页面塞一个 __atb_net_fake 就会被误判。
  const STATE_SLOT = /^__atb_net_[a-z0-9]{8}$/;

  /// 真实网络状态槽的形状（page_net.js 的 createState）。
  ///
  /// 只看名字还不够：页面可以自己造一个形状合法的同名属性。这里再核对值结构，
  /// 只有「名字 + 不可枚举 + 这几个字段」全中才认，避免 scrub 删掉别人的东西。
  function looksLikeNetState(value) {
    if (!value || typeof value !== 'object') return false;
    return Array.isArray(value.routes) &&
      Array.isArray(value.log) &&
      typeof value.installed === 'boolean' &&
      typeof value.seq === 'number';
  }

  /// 读一个属性的「值」，但不给页面执行 getter 的机会。
  ///
  /// window[name] 这种读法会触发页面定义的 getter：对方放一个「求值即报错 / 计数 /
  /// 打点」的 getter，我们每跑一次 stealth 就在帮他标记自动化。只认数据属性。
  function dataValue(name) {
    const desc = safe(function () { return Object.getOwnPropertyDescriptor(window, name); }, null);
    if (!desc || !Object.prototype.hasOwnProperty.call(desc, 'value')) return null;
    return desc.value;
  }

  function netStateKey() {
    // 旧名同样只按数据属性读：hasOwnProperty 能看见访问器属性，读值却会踩 getter。
    const legacy = dataValue('__abcNetState');
    if (looksLikeNetState(legacy)) return '__abcNetState';
    // 真实槽位有三个固定特征：名字形状、不可枚举、值结构。少一个都不认。
    const names = safe(function () {
      return Object.getOwnPropertyNames(window).filter(function (name) {
        if (typeof name !== 'string' || !STATE_SLOT.test(name)) return false;
        const desc = safe(function () { return Object.getOwnPropertyDescriptor(window, name); }, null);
        if (!desc || desc.enumerable !== false) return false;
        // 显式判种类，别靠「访问器没有 value 槽位」这种巧合：访问器属性永远不算我们的槽位。
        if (!Object.prototype.hasOwnProperty.call(desc, 'value')) return false;
        return looksLikeNetState(desc.value);
      });
    }, []);
    return names.length ? names[0] : null;
  }

  function nativeFunction(fn) {
    return safe(function () {
      return /\[native code\]/.test(Function.prototype.toString.call(fn));
    }, false);
  }

  /// 取 WebGL 的 unmasked renderer：headless / 软渲染会露出 SwiftShader、llvmpipe。
  function webglRenderer() {
    const canvas = safe(function () { return document.createElement('canvas'); }, null);
    if (!canvas) return '';
    const gl = safe(function () {
      return canvas.getContext('webgl') || canvas.getContext('experimental-webgl');
    }, null);
    if (!gl) return '';
    const info = safe(function () { return gl.getExtension('WEBGL_debug_renderer_info'); }, null);
    if (!info) return '';
    return text(safe(function () { return gl.getParameter(info.UNMASKED_RENDERER_WEBGL); }, ''), 120);
  }

  function permissionMismatch() {
    // headless 里 Notification.permission 常是 denied，但 permissions.query 报 prompt。
    const direct = safe(function () {
      return typeof Notification === 'undefined' ? null : Notification.permission;
    }, null);
    if (!direct) return null;
    return new Promise(function (resolve) {
      let settled = false;
      const done = function (value) { if (!settled) { settled = true; resolve(value); } };
      try {
        const query = navigator.permissions.query({ name: 'notifications' });
        if (!query || typeof query.then !== 'function') return done(null);
        query.then(function (status) {
          done(direct === 'denied' && status && status.state === 'prompt'
            ? { direct: direct, query: status.state }
            : null);
        }, function () { done(null); });
        setTimeout(function () { done(null); }, 300);
      } catch (_) {
        done(null);
      }
    });
  }

  function envSnapshot() {
    const plugins = safe(function () { return navigator.plugins ? navigator.plugins.length : -1; }, -1);
    const mimes = safe(function () { return navigator.mimeTypes ? navigator.mimeTypes.length : -1; }, -1);
    return {
      user_agent: text(navigator.userAgent, 160),
      platform: text(safe(function () { return navigator.platform; }, ''), 40),
      languages: safe(function () { return Array.prototype.slice.call(navigator.languages || []); }, []).slice(0, 8),
      webdriver: safe(function () { return navigator.webdriver; }, null),
      plugins: plugins,
      mime_types: mimes,
      hardware_concurrency: safe(function () { return navigator.hardwareConcurrency; }, null),
      outer_size: safe(function () { return [window.outerWidth, window.outerHeight]; }, [0, 0]),
      screen_size: safe(function () { return [screen.width, screen.height]; }, [0, 0]),
      device_pixel_ratio: safe(function () { return window.devicePixelRatio; }, null),
      timezone: text(safe(function () { return Intl.DateTimeFormat().resolvedOptions().timeZone; }, ''), 60),
      webgl_renderer: webglRenderer(),
      visibility: safe(function () { return document.visibilityState; }, ''),
      has_focus: safe(function () { return document.hasFocus(); }, null),
      has_chrome_object: safe(function () { return !!window.chrome; }, false),
      chrome_runtime_in_page: safe(function () { return !!(window.chrome && window.chrome.runtime); }, false)
    };
  }

  function collectFindings(env) {
    const findings = [];
    function add(id, severity, title, detail, fix, scope) {
      findings.push({
        id: id,
        severity: severity,
        title: title,
        detail: text(detail, 400),
        fix: fix,
        scope: scope || 'unknown'
      });
    }

    // ---- 本工具自身的痕迹（最该修，也是 scrub 唯一会动的部分）----
    const ownHits = OWN_GLOBALS.filter(function (name) {
      // __abcStealth 就是审计函数自己，正在执行时必然存在，不能算作站点可见的残留。
      if (name === '__abcStealth') return false;
      return ownGlobalPresent(name);
    });
    if (ownHits.length) {
      add(
        'own_globals',
        'high',
        '页面 window 上有本工具注入的全局函数',
        ownHits.join(', '),
        '执行 `atb stealth scrub` 清除；日常使用把 page ops 固定在 ISOLATED world，就不会再写进页面',
        'self'
      );
    }
    const ownNodes = OWN_DOM_IDS.filter(function (id) {
      return safe(function () { return !!document.getElementById(id); }, false);
    });
    if (ownNodes.length) {
      add(
        'own_dom_badge',
        'high',
        '页面 DOM 里有扩展角标节点',
        ownNodes.map(function (id) { return '#' + id; }).join(', '),
        '扩展 popup 里关闭「显示状态角标」，或 `atb stealth scrub` 临时移除',
        'self'
      );
    }
    const netState = netStateKey();
    const fetchNative = nativeFunction(safe(function () { return window.fetch; }, null));
    const xhrNative = nativeFunction(safe(function () { return XMLHttpRequest.prototype.open; }, null));
    if (netState) {
      add(
        'own_net_hook',
        'high',
        'fetch / XMLHttpRequest 被本工具的网络钩子包装过',
        '状态槽 ' + netState + ' 存在（fetch 的 [native code] 标记=' + fetchNative +
          '；5.0 起包装函数已做 toString 伪装，这个标记不可靠，只作为参考）',
        '`atb network stop` 或 `atb stealth scrub` 还原；只抓包时用 `--engine cdp`，不碰页面函数',
        'self'
      );
    } else if (!fetchNative || !xhrNative) {
      add(
        'patched_network',
        'medium',
        'fetch 或 XMLHttpRequest 已不是原生实现',
        'fetch native=' + fetchNative + '，xhr native=' + xhrNative,
        '若同时装了别的脚本（油猴等）会这样；本工具未开网络钩子时不会改这两个函数。注意 5.0 起包装函数会伪装成 native，这条只对伪装之前/别人的改动有效',
        'unknown'
      );
    }

    // ---- 浏览器 / 启动方式造成的痕迹 ----
    if (env.webdriver === true) {
      add(
        'navigator_webdriver',
        'high',
        'navigator.webdriver === true',
        '这是最直接的自动化标记，站点一行 JS 就能读到',
        '用真实 Chrome + 本扩展（本工具不设置该标记）；若为 true，说明浏览器是被 --enable-automation 或调试端口拉起的',
        'browser'
      );
    }
    if (/HeadlessChrome/i.test(env.user_agent)) {
      add('ua_headless', 'high', 'UA 里带 HeadlessChrome', env.user_agent, '用有头模式启动 Chrome', 'browser');
    }
    if (env.plugins === 0 && env.mime_types === 0) {
      add('no_plugins', 'high', 'navigator.plugins / mimeTypes 为空', 'plugins=0, mimeTypes=0', '有头 Chrome 正常会有 PDF 等内置插件；为空通常是 headless', 'browser');
    }
    if (!env.languages || !env.languages.length) {
      add('no_languages', 'medium', 'navigator.languages 为空', '正常浏览器至少有 1 个语言', '检查浏览器语言设置；headless 默认为空', 'browser');
    }
    if (!env.has_chrome_object) {
      add('no_chrome_object', 'medium', 'window.chrome 不存在', '非 Chromium 内核或被裁剪的构建', '换官方 Chrome / Edge', 'browser');
    }
    if (env.chrome_runtime_in_page) {
      add(
        'chrome_runtime_exposed',
        'medium',
        '页面能读到 chrome.runtime（扩展对外暴露）',
        '说明有扩展声明了 externally_connectable 或用 web_accessible_resources 注入',
        '本扩展没有 externally_connectable；若命中，检查是否装了其它扩展',
        'unknown'
      );
    }
    if (env.outer_size && (env.outer_size[0] === 0 || env.outer_size[1] === 0)) {
      // 窗口最小化时 outerWidth/Height 也会是 0，这不是自动化特征。
      // 用 visibilityState 区分：隐藏 + 尺寸为 0 → 最小化；可见 + 尺寸为 0 → 才像 headless。
      const minimized = safe(function () { return document.visibilityState === 'hidden'; }, false);
      add(
        'zero_outer_size',
        minimized ? 'low' : 'high',
        'window.outerWidth/outerHeight 为 0',
        JSON.stringify(env.outer_size) + (minimized ? '（窗口处于最小化/隐藏状态，恢复窗口后复测）' : ''),
        minimized ? '把浏览器窗口恢复出来再复测' : '有头模式重开浏览器',
        'browser'
      );
    }
    if (env.screen_size && (env.screen_size[0] === 0 || env.screen_size[1] === 0)) {
      add('zero_screen_size', 'high', 'screen.width/height 为 0', JSON.stringify(env.screen_size), '有头模式重开浏览器', 'browser');
    }
    if (/swiftshader|llvmpipe|software|mesa offscreen/i.test(env.webgl_renderer)) {
      add('software_webgl', 'high', 'WebGL 走软件渲染', env.webgl_renderer, '关闭 --disable-gpu / --use-gl=swiftshader，用真实 GPU', 'browser');
    }
    if (!env.timezone) {
      add('no_timezone', 'low', 'Intl 时区为空', 'headless 环境常见', '设置系统时区', 'browser');
    }
    // 后台标签 / 最小化窗口：不是「伪装成机器人」，但风控拿它当真实用户的反证 ——
    // 真人正在看的页面 visibilityState 一定是 visible；hidden 还会把定时器节流到分钟级，
    // 挑战脚本（CF 的 RTT 上报与放行轮询）根本跑不完，表现为 wait 一路超时。
    if (env.visibility === 'hidden') {
      add(
        'tab_hidden',
        'medium',
        '页面处于后台标签（visibilityState=hidden）',
        'hasFocus=' + env.has_focus + '；后台标签的定时器被 Chrome 节流，挑战脚本可能跑不完',
        '需要过验证码/挑战时先用 `atb challenge focus` 把标签页切到前台；批量抓取时把这一步留在挑战页上',
        'browser'
      );
    }

    // ---- 常见自动化框架残留 ----
    const frameworkKeys = safe(function () {
      return Object.getOwnPropertyNames(window).filter(function (name) {
        return /^(__playwright|__puppeteer|__nightmare|__selenium|_phantom|callPhantom|cdc_|__webdriver|_Selenium_IDE_Recorder)/.test(name);
      });
    }, []);
    if (frameworkKeys.length) {
      add(
        'automation_framework',
        'high',
        '页面里有自动化框架的注入残留',
        frameworkKeys.slice(0, 10).join(', '),
        '这些不是本工具写的；把对应框架的启动参数 / 注入脚本去掉',
        'unknown'
      );
    }

    return findings;
  }

  async function audit() {
    eraseSelfSoon();
    const env = envSnapshot();
    const findings = collectFindings(env);
    const mismatch = await permissionMismatch();
    if (mismatch) {
      findings.push({
        id: 'notification_permission_mismatch',
        severity: 'medium',
        title: 'Notification 权限与 permissions.query 不一致',
        detail: 'Notification.permission=' + mismatch.direct + '，query=' + mismatch.query,
        fix: 'headless 下常见；有头模式重开浏览器',
        scope: 'browser'
      });
    }

    const weight = { high: 25, medium: 10, low: 4 };
    let score = 100;
    findings.forEach(function (item) { score -= weight[item.severity] || 0; });
    score = Math.max(0, Math.min(100, score));
    const level = score >= 90 ? 'clean' : score >= 70 ? 'low' : score >= 40 ? 'medium' : 'high';
    const selfHits = findings.filter(function (item) { return item.scope === 'self'; }).length;

    return {
      ok: true,
      action: 'audit',
      score: score,
      level: level,
      headline: level === 'clean'
        ? '未发现可被站点利用的自动化痕迹'
        : selfHits
          ? '发现本工具自身留下的痕迹，可执行 stealth scrub 清除'
          : '发现与浏览器启动方式相关的痕迹，本工具无法在页面内修复',
      findings: findings,
      self_fixable: selfHits,
      env: env,
      url: text(location.href, 400),
      title: text(document.title, 200)
    };
  }

  /// 只清本工具自己的痕迹。顺序很重要：先还原被包装的函数，再删全局，
  /// 否则删掉 __abcNet 之后包装还在，反而更糟。
  function scrub() {
    eraseSelfSoon();
    const restored = [];
    const removed = [];

    const stateKey = netStateKey();
    // __abcNet 也只按数据属性读：页面若把它定义成 getter，typeof window.__abcNet 会触发它。
    const netFn = dataValue('__abcNet');
    if (typeof netFn === 'function' && stateKey) {
      const outcome = safe(function () { return netFn({ action: 'reset' }); }, null);
      if (outcome) restored.push('fetch/xhr');
      // reset 自己应该会删掉状态槽；万一旧版本没删干净，这里补一刀。
      safe(function () { delete window[stateKey]; });
    } else if (stateKey) {
      // 状态槽还在、__abcNet 已经被删（上次 scrub 的中间态）：直接摘掉，避免误报。
      safe(function () { delete window[stateKey]; });
      restored.push('state-only');
      // 这条只删槽位、没有可还原的包装（__abcNet 已经不在了），
      // 所以 restored 里的 'fetch/xhr' 与 'state-only' 是互斥的两种情形。
    }

    OWN_GLOBALS.forEach(function (name) {
      if (name === '__abcStealth') return; // 本函数还在跑，最后单独删
      const existed = ownGlobalPresent(name);
      if (!existed) return;
      const deleted = safe(function () { return delete window[name]; }, false);
      if (deleted) removed.push(name);
    });

    OWN_DOM_IDS.forEach(function (id) {
      const node = safe(function () { return document.getElementById(id); }, null);
      if (!node) return;
      safe(function () { node.remove(); });
      removed.push('#' + id);
    });

    const leftovers = OWN_GLOBALS.filter(function (name) {
      if (name === '__abcStealth') return false;
      return ownGlobalPresent(name);
    });
    // 网络状态槽不在 OWN_GLOBALS 里（名字是随机的），必须单独复查：
    // 只删 __abcNet、槽位没删掉的话，页面照样能用 getOwnPropertyNames 看到它，
    // 这时再报 ok:true 就是假干净。
    const leftoverState = netStateKey();
    if (leftoverState) leftovers.push(leftoverState);
    const leftoverNodes = OWN_DOM_IDS.filter(function (id) {
      return safe(function () { return !!document.getElementById(id); }, false);
    });

    return {
      ok: leftovers.length === 0 && leftoverNodes.length === 0,
      action: 'scrub',
      restored: restored,
      removed: removed,
      remaining: leftovers.concat(leftoverNodes),
      note: leftovers.length || leftoverNodes.length
        ? '仍有痕迹未清除（可能被页面以不可配置属性固定），刷新页面即可恢复干净状态'
        : '页面已无本工具痕迹；后续 page ops 走 ISOLATED world，不会再写进页面 window' + (restored.indexOf('state-only') >= 0 ? '（本次只摘掉了残留状态槽，没有可还原的包装函数）' : '')
    };
  }

  if (action === 'audit' || action === 'status' || action === 'detect') return audit();
  if (action === 'scrub' || action === 'clean') return scrub();
  return { ok: false, error: 'stealth: 未知 action ' + action };
}

// 实现标记：MAIN world 的页面可能抢先定义同名全局，扩展侧据此
// 判断执行的是我们的实现还是页面提供的实现。不可枚举，不新增可枚举痕迹。
try { Object.defineProperty(__abcStealth, '__abcOwn', { value: true }); } catch (_) {}

// 入口挂载：可配置 —— eraseSelfSoon / scrub 的 delete 才能真的成功；
// 不可枚举 —— 不出现在 Object.keys(window) / for...in 里。
try {
  Object.defineProperty(window, '__abcStealth', {
    value: __abcStealth,
    writable: true,
    enumerable: false,
    configurable: true
  });
} catch (_) {
  // 页面上已有旧版本留下的不可配置绑定：属性描述改不动，但值是 writable 的，
  // 赋值能把实现换成本版本（仍然不可删，只能等页面重载）。
  try { window.__abcStealth = __abcStealth; } catch (_) {}
}
})();
