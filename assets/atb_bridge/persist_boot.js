// 在 document_start 运行（ISOLATED world）。
// 只做一件事：如果当前 URL 命中已登记的持久注入，立刻通知 Service Worker
// 去 MAIN world 执行，比等 webNavigation.onCommitted 更早。
(async () => {
  try {
    const store = await chrome.storage.local.get({ persistInjections: {} });
    const entries = Object.entries(store.persistInjections || {});
    if (!entries.length) return;

    const url = location.href;
    // ==========================================================================
    // persist match pattern 口径 —— **必须与 background.js 的 matchesPersistPattern
    // 逐字等价**（那边有一份同样的注释块）。两处服务同一份 `persistInjections`：
    //   * 本文件：document_start 在页面里做第一次匹配；
    //   * background.js：动态注册失败时，webNavigation.onCommitted 之后重放。
    // 口径不一致会让「有些页面 document_start 生效、有些要等导航后」，且极难自查。
    //
    // 语义 = Chrome match pattern 的「scheme / host / path」三段匹配，但 match
    // pattern 不允许写端口、实际 URL 又常带端口（本地服务、内网），所以：
    //   1. `<all_urls>`          → 只放行 http/https（`file:` 必须显式声明）
    //   2. `<scheme>://host/path`
    //        - scheme 为 `*` 时仅放行 http/https；其余按字面相等（file/ftp…）
    //        - host 只比 hostname（**天然忽略端口**）；写 `:端口` 也会被剥掉后忽略
    //        - host 里的 `*` 匹配任意字符但不跨 `/`
    //        - path 里的 `*` 匹配任意字符（含 `/`），整体 `^...$`
    //   3. 不含 `://` 的写法（如 `*/api/*`）→ 退回「整条 URL 做 glob」
    //   4. patterns 配置损坏（存在但不是非空字符串数组）→ **不注入**，只 console.warn；
    //      只有 patterns 缺失（undefined/null）才退回 `<all_urls>`（见下面的 normalize）
    // ==========================================================================
    const escape = (text) => text.replace(/[.+?^${}()|[\]\\]/g, '\\$&');
    // 与 background.js 的 normalizePersistPatterns 行为等价：
    //   undefined / null → 没配，沿用历史 `<all_urls>`；
    //   非空字符串数组    → 过滤掉非字符串项后使用；
    //   其它（字符串 / 空数组 / 对象 / 全是非字符串项）→ null = 不注入。
    // 以前 `Array.isArray(patterns) && patterns.length ? … : ['<all_urls>']` 会让
    // 「配置一坏」变成「全站注入」（fail-open）。
    const normalize = (patterns) => {
      if (patterns === undefined || patterns === null) return ['<all_urls>'];
      if (!Array.isArray(patterns)) return null;
      const list = patterns.filter((item) => typeof item === 'string' && item.trim() !== '');
      return list.length ? list : null;
    };
    const describePatterns = (patterns) => {
      if (Array.isArray(patterns)) {
        const kinds = Array.from(new Set(patterns.map((item) => (item === null ? 'null' : typeof item))));
        return `array(length=${patterns.length}, itemTypes=${kinds.join('|') || 'none'})`;
      }
      return typeof patterns;
    };
    const matches = (patterns, target) => {
      const list = normalize(patterns);
      if (!list) {
        console.warn('[TMWD] 持久注入的 matches 配置损坏，已跳过注入（不会退回 <all_urls>）:', describePatterns(patterns));
        return false;
      }
      let parsed = null;
      try {
        parsed = new URL(target);
      } catch (_) {
        parsed = null;
      }
      return list.some((pattern) => {
        if (pattern === '<all_urls>') return /^https?:/i.test(target);
        if (!parsed) return false;
        const at = String(pattern).indexOf('://');
        if (at <= 0) {
          try {
            return new RegExp('^' + escape(pattern).replace(/\*/g, '.*') + '$', 'i').test(target);
          } catch (_) {
            return false;
          }
        }
        const scheme = pattern.slice(0, at).toLowerCase();
        const rest = pattern.slice(at + 3);
        const slash = rest.indexOf('/');
        // 端口写在 host 里时先剥掉：这里只比 hostname，端口不该影响命中。
        const hostPattern = (slash >= 0 ? rest.slice(0, slash) : rest).toLowerCase().replace(/:\d+$/, '');
        const pathPattern = slash >= 0 ? rest.slice(slash) : '/';
        const protocol = String(parsed.protocol || '').replace(':', '').toLowerCase();
        if (scheme !== '*' && scheme !== protocol) return false;
        if (scheme === '*' && !/^https?$/.test(protocol)) return false;
        const hostRe = new RegExp('^' + escape(hostPattern).replace(/\*/g, '[^/]*') + '$', 'i');
        if (!hostRe.test(parsed.hostname)) return false;
        const pathRe = new RegExp('^' + escape(pathPattern).replace(/\*/g, '.*') + '$', 'i');
        return pathRe.test(parsed.pathname || '/');
      });
    };

    const hits = entries
      .filter(([, entry]) => matches(entry.matches, url))
      .map(([name, entry]) => ({ name, code: entry.code, world: entry.world || 'MAIN' }));
    if (!hits.length) return;

    chrome.runtime.sendMessage({ type: 'persist-apply', entries: hits }).catch(() => {});
  } catch (_) {
    /* 注入失败不能影响页面本身 */
  }
})();
