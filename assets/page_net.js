// 页面网络能力：
//   fetch  —— 在页面上下文里发请求（带 cookie / 同源策略 / 页面 CSP 视角）
//   hook   —— 包装 window.fetch 与 XMLHttpRequest，记录、改写请求、伪造/改写响应
//   log    —— 读取拦截记录
//   route  —— 增加 / 删除规则
//   reset  —— 还原所有包装并清空状态
//
// 规则字段（都可选）：
//   engine                由 daemon 使用：page（默认，本文件）/ cdp（扩展侧网络层拦截）
//   match / method        命中条件（URL 片段或正则；method 支持 *）
//   requestHeaders        追加/覆盖请求头
//   requestBody           替换请求体（字符串或对象）
//   status / body / headers / abort   命中即伪造响应（不再真的发出去）
//   response              放行真实请求后改写响应：{status, body, headers}
//
// 重要：本文件每次命令都会重新注入，因此状态必须挂在 window 上，
// 包装函数只在首次注入时安装，避免 fetch 被套多层。
function __abcNet(opts) {
  opts = opts || {};
  const action = String(opts.action || 'fetch').toLowerCase();
  const now = () => (typeof performance !== 'undefined' && performance.now ? performance.now() : Date.now());

  const state = (function () {
    if (window.__abcNetState) return window.__abcNetState;
    const created = {
      installed: false,
      seq: 0,
      log: [],
      routes: [],
      originals: null,
      maxLog: 200,
      captureBodies: false,
      xhrGuarded: false,
      xhrNative: null,
      xhrRestore: null
    };
    window.__abcNetState = created;
    return created;
  })();

  function pushLog(entry) {
    entry.id = ++state.seq;
    entry.at = Date.now();
    state.log.push(entry);
    if (state.log.length > state.maxLog) state.log.splice(0, state.log.length - state.maxLog);
    return entry;
  }

  function toHeaders(input) {
    const out = {};
    if (!input) return out;
    if (typeof Headers !== 'undefined' && input instanceof Headers) {
      input.forEach(function (value, key) { out[key] = value; });
      return out;
    }
    if (Array.isArray(input)) {
      for (const pair of input) {
        if (pair && pair.length >= 2) out[String(pair[0])] = String(pair[1]);
      }
      return out;
    }
    if (typeof input === 'object') {
      for (const key of Object.keys(input)) out[key] = String(input[key]);
      return out;
    }
    return out;
  }

  function matchRule(url, method) {
    const target = String(url || '');
    const verb = String(method || 'GET').toUpperCase();
    for (const rule of state.routes) {
      if (rule.method && rule.method !== '*' && String(rule.method).toUpperCase() !== verb) continue;
      if (!rule.match) continue;
      let hit = false;
      try {
        hit = target.includes(rule.match) || new RegExp(rule.match).test(target);
      } catch (_) {
        hit = target.includes(rule.match);
      }
      if (hit) return rule;
    }
    return null;
  }

  function ruleBody(rule, fallback) {
    const body = rule.body !== undefined ? rule.body : fallback;
    if (body === undefined || body === null) return '';
    return typeof body === 'string' ? body : JSON.stringify(body);
  }

  function ruleHeaders(rule) {
    return Object.assign({ 'content-type': 'application/json' }, rule.headers || {});
  }

  function isMock(rule) {
    if (!rule) return false;
    return rule.abort === true || rule.status !== undefined || rule.body !== undefined;
  }

  // ---- XHR 响应改写：接管访问器，而不是在事件回调里改实例 ----------------
  //
  // 事件监听器按「注册顺序」触发。页面通常在建好 XHR 后立刻 xhr.onload = ...
  // 再 send()，我们的 loadend 监听器只能注册在它们之后；即便在回调里改写
  // xhr.responseText，页面也早就把原始响应读走了。所以这里接管
  // XMLHttpRequest.prototype 上的 responseText / response / status / statusText
  // 以及两个 header 方法：读取时只要实例命中改写规则就返回改写值，
  // 与谁先注册监听器、读得多早都无关。规则在 send() 里挂到实例上，
  // 一定早于任何响应事件。
  const XHR_ACCESSORS = ['responseText', 'response', 'status', 'statusText'];

  function findDescriptor(name) {
    let cursor = XMLHttpRequest.prototype;
    while (cursor) {
      const found = Object.getOwnPropertyDescriptor(cursor, name);
      if (found) return found;
      cursor = Object.getPrototypeOf(cursor);
    }
    return null;
  }

  function captureXhrNative() {
    const props = {};
    for (const name of XHR_ACCESSORS) {
      const descriptor = findDescriptor(name);
      if (descriptor && descriptor.get) props[name] = descriptor;
    }
    return {
      props: props,
      allHeaders: XMLHttpRequest.prototype.getAllResponseHeaders,
      oneHeader: XMLHttpRequest.prototype.getResponseHeader
    };
  }

  function parseHeaderLines(raw) {
    const map = {};
    String(raw || '').split(/\r?\n/).forEach(function (line) {
      const at = line.indexOf(':');
      if (at <= 0) return;
      map[line.slice(0, at).trim().toLowerCase()] = line.slice(at + 1).trim();
    });
    return map;
  }

  /// 把原始响应体读成文本；读不出来返回 null（blob 等无法同步读取）。
  function rawXhrText(xhr) {
    const native = state.xhrNative;
    if (!native) return null;
    const type = xhr.responseType || '';
    try {
      if (type === '' || type === 'text') return native.props.responseText.get.call(xhr);
      if (type === 'json') {
        const value = native.props.response.get.call(xhr);
        return value === undefined ? null : JSON.stringify(value);
      }
      if (type === 'arraybuffer') {
        const buffer = native.props.response.get.call(xhr);
        if (!buffer || typeof TextDecoder === 'undefined') return null;
        return new TextDecoder('utf-8').decode(new Uint8Array(buffer));
      }
      if (type === 'document') {
        const doc = native.props.response.get.call(xhr);
        return doc && doc.documentElement ? doc.documentElement.outerHTML : null;
      }
    } catch (_) {}
    return null;
  }

  function buildRewrite(xhr, spec, out) {
    if (spec.mock) {
      out.text = spec.mock.text === undefined || spec.mock.text === null ? '' : String(spec.mock.text);
      if (spec.mock.status !== undefined) out.status = Number(spec.mock.status);
      if (spec.mock.statusText !== undefined) out.statusText = String(spec.mock.statusText);
      if (spec.mock.headers) out.headers = spec.mock.headers;
    }
    const rewrite = spec.response;
    if (!rewrite) return;
    let text = null;
    if (rewrite.body !== undefined) {
      text = typeof rewrite.body === 'string' ? rewrite.body : JSON.stringify(rewrite.body);
    } else {
      text = rawXhrText(xhr);
    }
    if (text === null) {
      // 原始响应体读不出来（blob / 读访问器抛错），又没给 response.body：
      // 整条改写作废、放行原生响应，只在日志里记 response_rewrite_skipped。
      // 绝不能拿空串顶替——访问器会把这个空值交给页面，等于吞掉真实响应。
      out.unsupported = xhr.responseType === 'blob'
        ? 'blob-without-body（想改原始二进制体请用 net mock --engine cdp）'
        : 'body_unreadable';
      return;
    }
    out.text = text;
    if (rewrite.status !== undefined) out.status = Number(rewrite.status);
    if (rewrite.statusText !== undefined) out.statusText = String(rewrite.statusText);
    // rule.headers 是「响应头」的统一入口（mock 与改写都认），
    // response.headers 只对改写生效且优先级更高。
    if (spec.headers || rewrite.headers) {
      out.headers = Object.assign({}, spec.headers || {}, rewrite.headers || {});
    }
  }

  /// 惰性构建 + 缓存：第一次被读时算一次，之后直接复用。
  function rewriteOf(xhr) {
    const spec = xhr.__abcNetRespSpec;
    if (!spec) return null;
    if (xhr.readyState !== 4) return null;
    if (xhr.__abcNetRespBuilt) return xhr.__abcNetRespBuilt;
    const out = { text: null, status: null, statusText: null, headers: null, unsupported: null };
    try {
      buildRewrite(xhr, spec, out);
    } catch (e) {
      out.unsupported = (e && e.message) || String(e);
    }
    try {
      Object.defineProperty(xhr, '__abcNetRespBuilt', { value: out, configurable: true });
    } catch (_) {}
    return out;
  }

  function rewriteValue(xhr, out) {
    // out.text === null 表示「响应体没有真的被改写」（读不出来，或规则只改了
    // status/headers）。此时必须放行原生响应：否则下面 json 分支的
    // JSON.parse(null) 会得到 null，页面拿到的不是原响应而是一个空值。
    if (!out || out.text === null) return undefined;
    const type = xhr.responseType || '';
    if (type === '' || type === 'text') return out.text;
    if (type === 'json') {
      try { return JSON.parse(out.text); } catch (_) { return null; }
    }
    if (type === 'arraybuffer') {
      if (typeof TextEncoder === 'undefined') return undefined;
      const bytes = new TextEncoder().encode(out.text);
      return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    }
    if (type === 'document') {
      try { return new DOMParser().parseFromString(out.text, 'text/html'); } catch (_) { return undefined; }
    }
    if (type === 'blob') {
      // 原始 blob 无法同步读出，所以只有规则显式给了 body 才能构造新 blob。
      // 想改写「原始二进制体」需要用网络层拦截（net mock --engine cdp）。
      if (typeof Blob === 'undefined' || out.text === null) return undefined;
      const headers = out.headers || {};
      let contentType = null;
      for (const key of Object.keys(headers)) {
        if (String(key).toLowerCase() === 'content-type') contentType = String(headers[key]);
      }
      if (!contentType) {
        // 规则没改 content-type 就沿用原始响应头，别把 JSON blob 变成二进制流。
        try {
          const native = state.xhrNative && state.xhrNative.oneHeader;
          if (native) contentType = native.call(xhr, 'content-type');
        } catch (_) {}
      }
      return new Blob([out.text], { type: contentType || 'application/octet-stream' });
    }
    return undefined;
  }

  function guardXhr() {
    if (state.xhrGuarded) return;
    const native = captureXhrNative();
    if (!native.props.responseText || !native.props.response) return;
    state.xhrNative = native;
    const proto = XMLHttpRequest.prototype;
    const restore = [];

    function define(name, descriptor) {
      // 浏览器实现里拿不到原生 getter 就干脆不接管，调用方仍走原生路径。
      if (!native.props[name]) return;
      try {
        Object.defineProperty(proto, name, descriptor);
        restore.push({ name: name, descriptor: native.props[name] });
      } catch (_) {}
    }

    define('responseText', {
      configurable: true,
      enumerable: true,
      get: function () {
        const out = rewriteOf(this);
        // responseType 非 text 时原生访问器本身就该抛错，保持原语义。
        if (!out || out.text === null || this.responseType) {
          return native.props.responseText.get.call(this);
        }
        return out.text;
      }
    });

    define('response', {
      configurable: true,
      enumerable: true,
      get: function () {
        const out = rewriteOf(this);
        if (!out) return native.props.response.get.call(this);
        const value = rewriteValue(this, out);
        return value === undefined ? native.props.response.get.call(this) : value;
      }
    });

    define('status', {
      configurable: true,
      enumerable: true,
      get: function () {
        const out = rewriteOf(this);
        if (!out || out.status === null) return native.props.status.get.call(this);
        return out.status;
      }
    });

    define('statusText', {
      configurable: true,
      enumerable: true,
      get: function () {
        const out = rewriteOf(this);
        if (!out || out.statusText === null) return native.props.statusText.get.call(this);
        return out.statusText;
      }
    });

    proto.getAllResponseHeaders = function () {
      const raw = native.allHeaders.call(this);
      const out = rewriteOf(this);
      if (!out || !out.headers) return raw;
      const map = parseHeaderLines(raw);
      for (const key of Object.keys(out.headers)) {
        map[String(key).toLowerCase()] = String(out.headers[key]);
      }
      // 体被换掉后长度/编码必然对不上，直接去掉，免得页面按旧长度截断。
      if (out.text !== null) {
        delete map['content-length'];
        delete map['content-encoding'];
      }
      const keys = Object.keys(map);
      if (!keys.length) return '';
      return keys.map(function (key) { return key + ': ' + map[key]; }).join('\r\n') + '\r\n';
    };

    proto.getResponseHeader = function (name) {
      const key = String(name || '').toLowerCase();
      const out = rewriteOf(this);
      if (out && out.headers) {
        for (const header of Object.keys(out.headers)) {
          if (String(header).toLowerCase() === key) return String(out.headers[header]);
        }
        if (out.text !== null && (key === 'content-length' || key === 'content-encoding')) return null;
      }
      return native.oneHeader.call(this, name);
    };

    state.xhrRestore = restore;
    state.xhrGuarded = true;
  }

  function install() {
    if (state.installed) return state.originals;
    const originals = {
      fetch: window.fetch,
      xhrOpen: XMLHttpRequest.prototype.open,
      xhrSend: XMLHttpRequest.prototype.send,
      xhrSetHeader: XMLHttpRequest.prototype.setRequestHeader
    };
    state.originals = originals;
    // XHR 的响应改写靠访问器接管，必须和 fetch 一起装上。
    guardXhr();

    window.fetch = async function (input, init) {
      const url = typeof input === 'string' ? input : (input && input.url) || '';
      const method = String((init && init.method) || (input && input.method) || 'GET').toUpperCase();
      const rule = matchRule(url, method);
      const started = now();

      if (rule && rule.abort) {
        pushLog({ kind: 'fetch', url, method, mocked: true, aborted: true, ok: false });
        throw new TypeError('Failed to fetch (blocked by agent-browser-cli rule)');
      }

      if (rule && isMock(rule)) {
        const body = ruleBody(rule, '');
        const response = new Response(body, {
          status: rule.status || 200,
          statusText: rule.statusText || 'OK',
          headers: ruleHeaders(rule)
        });
        pushLog({
          kind: 'fetch', url, method, mocked: true, status: rule.status || 200,
          ok: true, elapsed_ms: Math.round(now() - started)
        });
        return response;
      }

      // 改写请求：只在规则确实带 request* 字段时重建 init，避免影响正常调用。
      let callInit = init;
      let rewritten = false;
      if (rule && (rule.requestHeaders || rule.requestBody !== undefined)) {
        const headers = Object.assign(
          toHeaders(input && input.headers),
          toHeaders(init && init.headers),
          toHeaders(rule.requestHeaders)
        );
        const next = Object.assign({}, init || {}, { headers: headers });
        // GET/HEAD 不能带 body，带规则的 body 只在其它方法上生效。
        if (rule.requestBody !== undefined && method !== 'GET' && method !== 'HEAD') {
          next.body = typeof rule.requestBody === 'string'
            ? rule.requestBody
            : JSON.stringify(rule.requestBody);
        }
        callInit = next;
        rewritten = true;
      }

      try {
        const response = await originals.fetch.call(this, input, callInit);
        const entry = {
          kind: 'fetch', url, method, status: response.status, ok: response.ok,
          elapsed_ms: Math.round(now() - started), mocked: false,
          request_rewritten: rewritten || undefined
        };
        if (state.captureBodies) {
          try {
            const clone = response.clone();
            const text = await clone.text();
            entry.response_preview = text.slice(0, 2000);
            entry.response_length = text.length;
          } catch (_) {}
        }
        pushLog(entry);

        // 放行后改写响应：读取原响应体再构造一个新的 Response。
        if (rule && rule.response) {
          const spec = rule.response;
          let body = null;
          try {
            body = await response.clone().text();
          } catch (_) {
            body = null;
          }
          // 读不到原始响应体（body 已被消费 / 不透明响应）时，只有规则**显式**
          // 给了 response.body 才能安全地构造新响应。否则 nextBody 会是一个空串，
          // 页面拿到「200 + 空 body」，日志却写着改写成功——真实数据静默丢失。
          // 宁可放行原响应，也不要造一个假响应。
          if (body === null && spec.body === undefined) {
            pushLog({
              kind: 'fetch', url, method, response_modified: false,
              response_rewrite_skipped: 'body_unreadable',
              elapsed_ms: Math.round(now() - started)
            });
            return response;
          }
          const nextBody = spec.body !== undefined ? ruleBody({ body: spec.body }, '') : body;
          const nextHeaders = Object.assign(
            toHeaders(response.headers),
            toHeaders(rule.headers),
            toHeaders(spec.headers)
          );
          const modified = new Response(nextBody, {
            status: spec.status || response.status,
            statusText: response.statusText,
            headers: nextHeaders
          });
          pushLog({
            kind: 'fetch', url, method, response_modified: true,
            status: modified.status, ok: true
          });
          return modified;
        }
        return response;
      } catch (e) {
        pushLog({ kind: 'fetch', url, method, ok: false, error: e.message, elapsed_ms: Math.round(now() - started) });
        throw e;
      }
    };

    XMLHttpRequest.prototype.open = function (method, url) {
      this.__abcNet = {
        method: String(method || 'GET').toUpperCase(),
        url: String(url || ''),
        // 请求头先缓冲，send 时统一下发：原生 setRequestHeader 对同名头是「追加」，
        // 页头已经设过 x-token 时，规则再设一次会变成 "origin, rewritten"。
        headerPairs: []
      };
      // 复用同一个 xhr 时清掉上一次的改写状态，否则会拿旧响应体。
      this.__abcNetRespSpec = null;
      try { delete this.__abcNetRespBuilt; } catch (_) {}
      return originals.xhrOpen.apply(this, arguments);
    };

    XMLHttpRequest.prototype.setRequestHeader = function (name, value) {
      if (!this.__abcNet) return originals.xhrSetHeader.apply(this, arguments);
      // 非 OPENED 状态保持原生行为（抛 InvalidStateError）。
      if (this.readyState !== 1) return originals.xhrSetHeader.apply(this, arguments);
      this.__abcNet.headerPairs.push([String(name), String(value)]);
    };

    XMLHttpRequest.prototype.send = function (body) {
      const meta = this.__abcNet || { method: 'GET', url: '', headers: {} };
      meta.started = now();
      if (typeof body === 'string') meta.request_preview = body.slice(0, 1000);
      const rule = matchRule(meta.url, meta.method);
      const xhr = this;

      if (rule && rule.abort) {
        setTimeout(function () {
          try {
            xhr.dispatchEvent(new Event('error'));
            xhr.dispatchEvent(new Event('loadend'));
          } catch (_) {}
        }, 0);
        pushLog({ kind: 'xhr', url: meta.url, method: meta.method, mocked: true, aborted: true, ok: false });
        return;
      }

      if (rule && isMock(rule)) {
        const text = ruleBody(rule, '');
        // 只伪造 readyState，其余字段交给访问器守卫统一返回，
        // 这样 status / responseText / getResponseHeader 在任何时刻读都一致。
        xhr.__abcNetRespSpec = {
          mock: {
            text: text,
            status: rule.status === undefined ? 200 : rule.status,
            statusText: rule.statusText || 'OK',
            headers: ruleHeaders(rule)
          },
          response: null
        };
        setTimeout(function () {
          try {
            if (!Object.getOwnPropertyDescriptor(xhr, 'readyState')) {
              Object.defineProperty(xhr, 'readyState', { value: 4, configurable: true });
            }
            if (typeof xhr.onreadystatechange === 'function') xhr.onreadystatechange();
            xhr.dispatchEvent(new Event('readystatechange'));
            xhr.dispatchEvent(new Event('load'));
            xhr.dispatchEvent(new Event('loadend'));
          } catch (_) {}
        }, 0);
        pushLog({
          kind: 'xhr', url: meta.url, method: meta.method, mocked: true,
          status: rule.status === undefined ? 200 : rule.status, ok: true
        });
        return;
      }

      // 改写请求头 / 请求体（XHR 在 send 之前仍可 setRequestHeader）。
      let nextBody = body;
      const overrideHeaders = rule && rule.requestHeaders ? toHeaders(rule.requestHeaders) : null;
      if (overrideHeaders || (meta.headerPairs && meta.headerPairs.length)) {
        const pairs = meta.headerPairs || [];
        const overridden = overrideHeaders
          ? Object.keys(overrideHeaders).map(function (key) { return key.toLowerCase(); })
          : [];
        const finalPairs = pairs.filter(function (pair) {
          return overridden.indexOf(pair[0].toLowerCase()) < 0;
        });
        if (overrideHeaders) {
          for (const key of Object.keys(overrideHeaders)) {
            finalPairs.push([key, String(overrideHeaders[key])]);
          }
        }
        for (const pair of finalPairs) {
          try { originals.xhrSetHeader.call(this, pair[0], pair[1]); } catch (_) {}
        }
      }
      if (rule && rule.requestBody !== undefined
          && meta.method !== 'GET' && meta.method !== 'HEAD') {
        nextBody = typeof rule.requestBody === 'string'
          ? rule.requestBody
          : JSON.stringify(rule.requestBody);
      }

      // 响应改写：必须在 send 之前挂上，页面监听器无论何时注册、读多早都拿到改写值。
      if (rule && rule.response) {
        xhr.__abcNetRespSpec = {
          mock: null,
          response: rule.response,
          headers: rule.headers || null
        };
      }

      xhr.addEventListener('loadend', function () {
        const computed = rule && rule.response ? rewriteOf(xhr) : null;
        const entry = {
          kind: 'xhr', url: meta.url, method: meta.method,
          status: computed && computed.status !== null ? computed.status : xhr.status,
          ok: true, mocked: false,
          request_rewritten: (rule && (rule.requestHeaders || rule.requestBody !== undefined)) || undefined,
          elapsed_ms: Math.round(now() - (meta.started || now()))
        };
        entry.ok = entry.status >= 200 && entry.status < 400;
        if (computed) {
          entry.response_modified = computed.text !== null && !computed.unsupported;
          if (computed.unsupported) entry.response_rewrite_skipped = computed.unsupported;
        }
        pushLog(entry);
      });
      return originals.xhrSend.call(this, nextBody);
    };

    state.installed = true;
    return originals;
  }

  async function readBody(response, maxChars, responseType) {
    const contentType = response.headers.get('content-type') || '';
    if (responseType === 'base64') {
      const buffer = await response.arrayBuffer();
      const bytes = new Uint8Array(buffer);
      let binary = '';
      const limit = Math.min(bytes.length, maxChars);
      for (let i = 0; i < limit; i += 1) binary += String.fromCharCode(bytes[i]);
      return {
        body: btoa(binary),
        encoding: 'base64',
        truncated: bytes.length > limit,
        total_bytes: bytes.length,
        content_type: contentType
      };
    }
    const text = await response.text();
    const truncated = text.length > maxChars;
    return {
      body: truncated ? text.slice(0, maxChars) : text,
      encoding: 'text',
      truncated,
      total_chars: text.length,
      content_type: contentType
    };
  }

  async function doFetch() {
    const url = String(opts.url || '').trim();
    if (!url) return { ok: false, error: 'net fetch: url is required' };
    const method = String(opts.method || 'GET').toUpperCase();
    const headers = Object.assign({}, opts.headers || {});
    const init = {
      method,
      headers,
      credentials: opts.credentials || 'include',
      redirect: 'follow'
    };
    if (opts.body !== undefined && opts.body !== null && method !== 'GET' && method !== 'HEAD') {
      init.body = typeof opts.body === 'string' ? opts.body : JSON.stringify(opts.body);
      if (!Object.keys(headers).some((key) => key.toLowerCase() === 'content-type')) {
        headers['content-type'] = typeof opts.body === 'string'
          ? 'text/plain;charset=UTF-8'
          : 'application/json';
      }
    }
    const started = now();
    let response;
    try {
      response = await fetch(url, init);
    } catch (e) {
      return {
        ok: false,
        url,
        method,
        error: 'fetch failed: ' + (e && e.message ? e.message : String(e)),
        elapsed_ms: Math.round(now() - started)
      };
    }
    const responseHeaders = {};
    response.headers.forEach(function (value, key) { responseHeaders[key] = value; });
    const maxChars = opts.maxChars > 0 ? Math.floor(opts.maxChars) : 200000;
    const payload = await readBody(response, maxChars, opts.responseType);
    let json = null;
    if (opts.parseJson !== false && /json/i.test(payload.content_type)) {
      try { json = JSON.parse(payload.body); } catch (_) {}
    }
    return {
      ok: response.ok,
      url: response.url || url,
      method,
      status: response.status,
      status_text: response.statusText,
      headers: responseHeaders,
      body: payload.body,
      encoding: payload.encoding,
      truncated: payload.truncated,
      total_chars: payload.total_chars,
      total_bytes: payload.total_bytes,
      json,
      elapsed_ms: Math.round(now() - started)
    };
  }

  if (action === 'fetch') return doFetch();

  if (action === 'hook' || action === 'install') {
    state.captureBodies = opts.captureBodies === true;
    if (opts.maxLog > 0) state.maxLog = Math.floor(opts.maxLog);
    // 持久注入重放时会带上已有规则，避免刷新后规则丢失。
    if (Array.isArray(opts.routes)) {
      state.routes = opts.routes.filter((rule) => rule && typeof rule === 'object').slice(0, 50);
    }
    install();
    return {
      ok: true,
      action: 'hook',
      installed: true,
      capture_bodies: state.captureBodies,
      // routes 历史上是「规则条数」（数字），保留给老调用方；
      // 新调用方（daemon 持久化后重放）必须读 routes_list —— 把数字当数组存下来，
      // 刷新后原样下发会被上面的 Array.isArray(opts.routes) 跳过，规则就静默丢了。
      routes: state.routes.length,
      count: state.routes.length,
      routes_list: state.routes
    };
  }

  if (action === 'log') {
    const limit = opts.limit > 0 ? Math.floor(opts.limit) : 50;
    const needle = opts.match ? String(opts.match).toLowerCase() : '';
    let items = state.log;
    if (needle) {
      items = items.filter(function (entry) {
        return String(entry.url || '').toLowerCase().includes(needle);
      });
    }
    const sliced = items.slice(-limit);
    const out = {
      ok: true, action: 'log', installed: state.installed,
      count: items.length, items: sliced
    };
    if (opts.clear === true) state.log = [];
    return out;
  }

  if (action === 'route' || action === 'mock') {
    const rule = Object.assign({}, opts.rule || {});
    if (!rule.match && opts.url) rule.match = String(opts.url);
    if (!rule.match) return { ok: false, error: 'net route: 需要 match / url' };
    if (rule.method) rule.method = String(rule.method).toUpperCase();
    state.routes = state.routes.filter(function (item) {
      return item.match !== rule.match
        || String(item.method || '*') !== String(rule.method || '*');
    });
    state.routes.push(rule);
    if (!state.installed) install();
    return {
      ok: true, action: 'route', routes: state.routes.length,
      rule: rule, all: state.routes
    };
  }

  if (action === 'unroute') {
    const needle = String(opts.match || opts.url || '');
    if (!needle) return { ok: false, error: 'net unroute: 需要 match' };
    const before = state.routes.length;
    state.routes = state.routes.filter(function (item) { return item.match !== needle; });
    return {
      ok: true, action: 'unroute', removed: before - state.routes.length,
      routes: state.routes.length, all: state.routes
    };
  }

  if (action === 'routes') {
    // routes_list 与 hook 分支同名同义（真实数组），持久化逻辑只认这一个字段。
    return { ok: true, action: 'routes', count: state.routes.length, routes: state.routes, routes_list: state.routes };
  }

  if (action === 'reset' || action === 'unhook') {
    if (state.originals) {
      window.fetch = state.originals.fetch;
      XMLHttpRequest.prototype.open = state.originals.xhrOpen;
      XMLHttpRequest.prototype.send = state.originals.xhrSend;
      XMLHttpRequest.prototype.setRequestHeader = state.originals.xhrSetHeader;
    }
    // 还原被接管的访问器，避免 hook 关掉后页面还留着我们的 getter。
    if (state.xhrGuarded && state.xhrNative) {
      for (const item of state.xhrRestore || []) {
        try { Object.defineProperty(XMLHttpRequest.prototype, item.name, item.descriptor); } catch (_) {}
      }
      XMLHttpRequest.prototype.getAllResponseHeaders = state.xhrNative.allHeaders;
      XMLHttpRequest.prototype.getResponseHeader = state.xhrNative.oneHeader;
      state.xhrGuarded = false;
      state.xhrNative = null;
      state.xhrRestore = null;
    }
    const cleared = state.log.length;
    state.routes = [];
    state.log = [];
    state.installed = false;
    state.originals = null;
    return { ok: true, action: 'reset', cleared_log: cleared };
  }

  return { ok: false, error: 'net: 未知 action ' + action };
}
