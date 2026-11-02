// page_net 的「响应改写」安全性（N3-03）。
//
// 旧实现只要规则里出现 `response`，就会用 `await response.clone().text()` 取原响应体；
// 取不到时 body 保持空串，照样 `new Response('')` 返回给页面，日志还写
// `response_modified: true`。结果：页面拿到「200 + 空 body」——既不是原响应，
// 也不是改写后的响应，真实数据被静默吞掉。
//
// 正确行为：读不到原始响应体、且规则**没有显式给 body** 时，必须放行原响应，
// 并在日志里标 `response_modified: false` + `response_rewrite_skipped`。
// 这条测试锁的就是这个分叉——它是「假成功」里代价最高的一类（数据丢失）。
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const HERE = dirname(fileURLToPath(import.meta.url));
const PAGE_NET = join(HERE, '..', 'assets', 'page_net.js');

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${ok || !detail ? '' : '\n        → ' + detail}`);
}

// 原响应：clone().text() 抛错，模拟「body 已被消费 / 不透明响应」。
function makeUnreadableResponse(status = 200) {
  return {
    status,
    statusText: 'OK',
    ok: true,
    headers: new Headers({ 'content-type': 'application/json' }),
    clone() {
      return {
        text: async () => {
          throw new TypeError('body stream already read');
        },
      };
    },
    text: async () => {
      throw new TypeError('body stream already read');
    },
  };
}

function loadPageNet(originalFetch, XhrClass) {
  // page_net 的 install() 会同时包装 fetch 与 XHR，必须给一个最小的 XHR 形状。
  class FakeXhr {
    open() {}
    send() {}
    setRequestHeader() {}
    abort() {}
    addEventListener() {}
    removeEventListener() {}
  }
  FakeXhr.prototype.readyState = 0;
  FakeXhr.prototype.status = 0;
  FakeXhr.prototype.responseText = '';
  const Xhr = XhrClass || FakeXhr;

  const listeners = new Map();
  const window = {
    fetch: originalFetch,
    addEventListener: (type, fn) => listeners.set(type, fn),
    XMLHttpRequest: Xhr,
    removeEventListener: () => {},
    location: { href: 'https://example.com/page' },
    performance: { now: () => Date.now() },
    setTimeout,
    clearTimeout,
  };
  window.window = window;
  const context = vm.createContext({
    window,
    self: window,
    document: { addEventListener: () => {}, readyState: 'complete', documentElement: {} },
    location: window.location,
    console,
    setTimeout,
    clearTimeout,
    Response,
    Headers,
    Request,
    URL,
    URLSearchParams,
    TextDecoder,
    TextEncoder,
    JSON,
    Math,
    Date,
    Object,
    Array,
    String,
    Number,
    Boolean,
    Promise,
    Error,
    TypeError,
    performance: window.performance,
    fetch: originalFetch,
    XMLHttpRequest: Xhr,
  });
  vm.runInContext(readFileSync(PAGE_NET, 'utf8'), context, { filename: PAGE_NET });
  const fn = context.__abcNet || window.__abcNet;
  if (typeof fn !== 'function') throw new Error('page_net.js 未暴露 __abcNet');
  return { net: fn, window };
}

// ---- 场景 1：只有 status 改写、原响应体读不出来 → 必须放行原响应 ----
{
  const original = makeUnreadableResponse(200);
  const { net, window } = loadPageNet(async () => original);
  net({ action: 'hook', routes: [{ match: '/api/thing', response: { status: 204 } }] });

  let returned;
  let threw = null;
  try {
    returned = await window.fetch('https://example.com/api/thing');
  } catch (err) {
    threw = err;
  }

  check('未抛错（放行是正常路径，不是失败）', threw === null, threw && threw.message);
  check(
    '返回的就是原响应对象（不是新建的空 body Response）',
    returned === original,
    returned === original ? '' : `返回了不同类型的对象：status=${returned && returned.status}`,
  );

  const log = net({ action: 'log' });
  // 同一请求会先落一条「请求开始」日志，改写结果在随后那条上；
  // 这里取「带 response_modified 字段」的那条，否则会拿到还没改写的记录。
  const entries = (log.items || log.entries || []).filter((item) => String(item.url || '').includes('/api/thing'));
  const entry = entries.find((item) => 'response_modified' in item) || entries[entries.length - 1];
  check('日志里有这条请求', Boolean(entry), JSON.stringify(log).slice(0, 300));
  if (entry) {
    check('日志标记未改写（response_modified=false）', entry.response_modified === false, JSON.stringify(entry));
    check(
      '日志给出跳过原因（response_rewrite_skipped=body_unreadable）',
      entry.response_rewrite_skipped === 'body_unreadable',
      JSON.stringify(entry.response_rewrite_skipped),
    );
  }
}

// ---- 场景 2：规则显式给了 body → 即使原响应体读不出来，也按规则改写 ----
{
  const original = makeUnreadableResponse(200);
  const { net, window } = loadPageNet(async () => original);
  net({ action: 'hook', routes: [{ match: '/api/patched', response: { status: 201, body: '{"patched":true}' } }] });

  let returned;
  let threw = null;
  try {
    returned = await window.fetch('https://example.com/api/patched');
  } catch (err) {
    threw = err;
  }

  check('显式 body 时不抛错', threw === null, threw && threw.message);
  check('返回的是新响应，不是原对象', returned !== original, '返回了原对象，说明改写被跳过');
  if (returned && returned !== original) {
    const text = await returned.text();
    check('新响应带规则里的 body', text === '{"patched":true}', text);
    check('新响应带规则里的 status', returned.status === 201, String(returned.status));
  }

  const log = net({ action: 'log' });
  const patchedEntries = (log.items || log.entries || []).filter((item) => String(item.url || '').includes('/api/patched'));
  const entry = patchedEntries.find((item) => 'response_modified' in item) || patchedEntries[patchedEntries.length - 1];
  if (entry) {
    check('显式 body 时日志标记已改写', entry.response_modified === true, JSON.stringify(entry));
  }
}

// ---- 场景 3：原响应体可读、规则只改 status → 正常改写（不该被上面的保护误伤） ----
{
  const original = {
    status: 200,
    statusText: 'OK',
    ok: true,
    headers: new Headers({ 'content-type': 'text/plain' }),
    clone() {
      return { text: async () => 'real-body' };
    },
  };
  const { net, window } = loadPageNet(async () => original);
  net({ action: 'hook', routes: [{ match: '/api/readable', response: { status: 202 } }] });

  let returned;
  try {
    returned = await window.fetch('https://example.com/api/readable');
  } catch (err) {
    returned = null;
    check('可读 body 场景不应抛错', false, err.message);
  }
  if (returned) {
    check('可读 body 时仍然改写（未被保护误伤）', returned !== original, '返回了原对象');
    check('body 保留原内容', (await returned.text()) === 'real-body', '');
    check('status 已按规则改写', returned.status === 202, String(returned.status));
  }
}

// ---- 场景 4：XHR 的「跳过改写」分支（v4 §4.3：此前只有代码、没有断言）----
//
// XHR 不会构造新 response（改写通过 responseText getter 生效），所以这里要验证的是
// **日志是否如实**：跳过改写时必须给出 response_rewrite_skipped，排查"为什么这条
// 请求没被 mock"全靠它。
//
// 桩必须凑齐四个原生访问器：page_net 的 guardXhr() 要求
// `props.responseText && props.response` 同时存在才接管，否则整条 XHR 路径不生效
// （第一次写测试就踩了这个坑——只给 responseText，结果永远是 body_unreadable）。
{
  class XhrBase {
    constructor() {
      this.listeners = new Map();
      this._status = 200;
      this._statusText = 'OK';
      this._text = '';
      this.responseType = '';
      this.readyState = 4;
    }
    open(method, url) {
      this._method = method;
      this._url = url;
    }
    send() {}
    setRequestHeader() {}
    abort() {}
    addEventListener(type, fn) {
      if (!this.listeners.has(type)) this.listeners.set(type, []);
      this.listeners.get(type).push(fn);
    }
    removeEventListener() {}
    fire(type) {
      for (const fn of this.listeners.get(type) || []) fn.call(this, { type });
    }
  }
  for (const [name, getter] of [
    ['status', function () { return this._status; }],
    ['statusText', function () { return this._statusText; }],
    ['responseText', function () { return this._text; }],
    ['response', function () { return this._text; }],
  ]) {
    Object.defineProperty(XhrBase.prototype, name, { configurable: true, get: getter });
  }

  function xhrLog(net, needle) {
    const entries = (net({ action: 'log' }).items || []).filter((item) =>
      String(item.url || '').includes(needle),
    );
    return entries.find((item) => 'response_modified' in item) || entries[entries.length - 1];
  }

  // A) responseText 抛错、规则也没给 response.body → 必须标 body_unreadable
  class UnreadableXhr extends XhrBase {
    get responseText() {
      throw new Error('body unreadable');
    }
  }
  {
    const { net, window } = loadPageNet(async () => null, UnreadableXhr);
    net({ action: 'hook', routes: [{ match: '/api/xhr-unreadable', response: { status: 204 } }] });
    const xhr = new window.XMLHttpRequest();
    xhr.open('GET', 'https://example.com/api/xhr-unreadable');
    xhr.send();
    xhr.fire('loadend');

    const entry = xhrLog(net, '/api/xhr-unreadable');
    check('XHR 体读不出来时有日志', Boolean(entry), '没有该请求的日志');
    if (entry) {
      check('XHR 跳过改写时 response_modified=false', entry.response_modified === false, JSON.stringify(entry));
      check(
        'XHR 跳过改写的原因可读（body_unreadable）',
        entry.response_rewrite_skipped === 'body_unreadable',
        JSON.stringify(entry.response_rewrite_skipped),
      );
    }
  }

  // B) blob 响应 → 原因要指出是 blob，而不是笼统的 body_unreadable（排查时差很多）
  class BlobXhr extends XhrBase {
    constructor() {
      super();
      this.responseType = 'blob';
    }
  }
  {
    const { net, window } = loadPageNet(async () => null, BlobXhr);
    net({ action: 'hook', routes: [{ match: '/api/xhr-blob', response: { status: 204 } }] });
    const xhr = new window.XMLHttpRequest();
    xhr.open('GET', 'https://example.com/api/xhr-blob');
    xhr.send();
    xhr.fire('loadend');

    const entry = xhrLog(net, '/api/xhr-blob');
    if (entry) {
      check(
        'XHR blob 响应给出 blob 专属原因',
        String(entry.response_rewrite_skipped || '').startsWith('blob-without-body'),
        JSON.stringify(entry.response_rewrite_skipped),
      );
    }
  }

  // C) 体可读 + 规则改 status → 正常改写，日志说改了
  {
    const { net, window } = loadPageNet(async () => null, XhrBase);
    net({ action: 'hook', routes: [{ match: '/api/xhr-readable', response: { status: 201 } }] });
    const xhr = new window.XMLHttpRequest();
    xhr.open('GET', 'https://example.com/api/xhr-readable');
    xhr._text = 'orig-body';
    xhr.send();
    xhr.fire('loadend');

    const entry = xhrLog(net, '/api/xhr-readable');
    check('XHR 可读体时有日志', Boolean(entry), '没有该请求的日志');
    if (entry) {
      check('XHR 可读体时标记为已改写', entry.response_modified === true, JSON.stringify(entry));
      check('XHR 改写后 status 生效', entry.status === 201, String(entry.status));
    }
  }
}

const failed = results.filter((item) => !item.ok);
console.log(`\n${results.length - failed.length}/${results.length} passed`);
if (failed.length) {
  console.log('failed:');
  for (const item of failed) console.log('  - ' + item.name);
  process.exit(1);
}
