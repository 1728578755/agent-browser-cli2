// Fetch 拦截「必须续传」不变式。
//
// Fetch.requestPaused 之后如果不调用 continueRequest / failRequest，页面请求会
// 一直挂着（表现为页面永久白屏、fetch 永不返回）。历史上 continueFetchResponse
// 只在「continueResponse 报 not found」这一条分支里续传，其他任何失败都会把请求
// 永久挂起。
//
// 这里不靠读源码猜缩进，而是把 background.js 原样放进 node:vm 执行，然后直接
// 调用全局的 continueFetchResponse，用注入的假 sendFetchCommand 观察它到底发了
// 哪些 CDP 命令 —— 行为断言，比正则稳。
import { readFileSync } from 'node:fs';
import { webcrypto } from 'node:crypto';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const HERE = dirname(fileURLToPath(import.meta.url));
const BACKGROUND = join(HERE, '..', 'assets', 'atb_bridge', 'background.js');

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${ok || !detail ? '' : '\n        → ' + detail}`);
}

function eventTarget() {
  return { addListener() {}, removeListener() {}, hasListener: () => false };
}

// respond(method, params) 决定该次 CDP 命令成功还是失败。
function buildContext(respond) {
  const calls = [];
  const chrome = {
    alarms: { create() {}, clear: async () => true, onAlarm: eventTarget() },
    contentSettings: new Proxy({}, { get: () => ({ async set() {} }) }),
    cookies: { getAll: async () => [] },
    debugger: {
      attach: async () => {},
      detach: async () => {},
      getTargets: async () => [],
      sendCommand: async (target, method, params) => {
        calls.push({ method, params: params || {} });
        return respond(method, params || {});
      },
      onEvent: eventTarget(),
      onDetach: eventTarget(),
    },
    declarativeNetRequest: { updateDynamicRules: async () => {} },
    downloads: { download: async () => 1, search: async () => [], cancel: async () => {}, erase: async () => [] },
    extension: { isAllowedFileSchemeAccess: async () => false },
    management: { getAll: async () => [], setEnabled: async () => {} },
    runtime: {
      id: 'test-extension-id',
      getManifest: () => ({ version: 'test' }),
      onInstalled: eventTarget(),
      onMessage: eventTarget(),
      onStartup: eventTarget(),
      reload() {},
    },
    scripting: {
      executeScript: async () => [],
      getRegisteredContentScripts: async () => [],
      registerContentScripts: async () => {},
      unregisterContentScripts: async () => {},
    },
    storage: { local: { get: async (d) => d ?? {}, set: async () => {} } },
    tabGroups: { query: async () => [], update: async () => ({}) },
    tabs: {
      query: async () => [],
      get: async () => ({ id: 1, url: 'https://example.com/' }),
      create: async () => ({ id: 1 }),
      update: async () => ({ id: 1 }),
      remove: async () => {},
      group: async () => 1,
      onCreated: eventTarget(),
      onRemoved: eventTarget(),
      onUpdated: eventTarget(),
    },
    webNavigation: { getAllFrames: async () => [], onCommitted: eventTarget() },
    windows: { create: async () => ({ id: 1 }), update: async () => ({}) },
  };

  class FakeWebSocket {}
  FakeWebSocket.CONNECTING = 0;
  FakeWebSocket.OPEN = 1;
  FakeWebSocket.CLOSING = 2;
  FakeWebSocket.CLOSED = 3;

  const context = vm.createContext({
    AbortController,
    TextDecoder,
    TextEncoder,
    WebSocket: FakeWebSocket,
    chrome,
    console: { log() {}, warn() {}, error() {} },
    crypto: webcrypto,
    fetch: async () => {
      throw new Error('daemon unavailable in test');
    },
    setTimeout: () => 1,
    clearTimeout: () => {},
    setInterval: () => 1,
    clearInterval: () => {},
  });

  vm.runInContext(readFileSync(BACKGROUND, 'utf8'), context, { filename: BACKGROUND });
  return { calls, context };
}

const methodsOf = (calls) => calls.map((call) => call.method);
const countOf = (calls, method) => methodsOf(calls).filter((m) => m === method).length;

async function runCase(respond) {
  const { calls, context } = buildContext(respond);
  const fn = context.continueFetchResponse;
  if (typeof fn !== 'function') throw new Error('background.js 未暴露 continueFetchResponse');
  const session = { tabId: 1, intercept: { stats: {} } };
  let error = null;
  try {
    await fn(session, 'REQ-1');
  } catch (e) {
    error = e;
  }
  return { calls, error };
}

async function main() {
  // 1) 首选方法报「Invalid InterceptionId」（真实常见失败，且**不是** not found）
  {
    const { calls, error } = await runCase((method) => {
      if (method === 'Fetch.continueResponse') {
        throw new Error("Fetch.continueResponse wasn't found");
      }
      return {};
    });
    check(
      '1. continueResponse 不可用时回落到 continueRequest',
      countOf(calls, 'Fetch.continueRequest') === 1,
      JSON.stringify(methodsOf(calls)),
    );
    check('1b. 回落后不再抛错', !error, error && error.message);
  }

  // 2) 首选方法报其他错误（拦截已失效、请求已被处理等）—— 仍必须续传，
  //    否则该请求永久挂起。这条是核心断言。
  {
    const { calls, error } = await runCase((method) => {
      if (method === 'Fetch.continueResponse') {
        throw new Error('Fetch.continueResponse: Invalid InterceptionId.');
      }
      return {};
    });
    check(
      '2. continueResponse 报非 not-found 错误时也必须调用 continueRequest',
      countOf(calls, 'Fetch.continueRequest') === 1,
      JSON.stringify(methodsOf(calls)),
    );
    check('2b. 续传失败后不向调用方抛错（避免拖垮整个事件循环）', !error, error && error.message);
  }

  // 3) 首选方法正常时不得重复续传（重复 continueRequest 会报 Invalid InterceptionId）。
  {
    const { calls } = await runCase(() => ({}));
    check(
      '3. continueResponse 成功时 continueRequest 恰好 0 次（不重复续传）',
      countOf(calls, 'Fetch.continueRequest') === 0,
      JSON.stringify(methodsOf(calls)),
    );
    check(
      '3b. 成功路径始终调用了 continueResponse 且恰好一次',
      countOf(calls, 'Fetch.continueResponse') === 1,
      JSON.stringify(methodsOf(calls)),
    );
  }

  // 5) 最外层入口：handleFetchPausedResponse 成功路径只能续传一次。
  //
  // 旧用例只调用 continueFetchResponse（最内层），既看不到「外层是否把 return
  // 之后的第二个分支也跑了一遍」，也看不到 guard.settled 是否真的落定。
  {
    const { calls, context } = buildContext(() => ({}));
    const fn = context.handleFetchPausedResponse;
    check('5. background.js 暴露最外层 handleFetchPausedResponse', typeof fn === 'function');
    if (typeof fn === 'function') {
      const session = { tabId: 1, intercept: { stats: {} } };
      const state = {
        rules: [{ match: '/api/success', response: { status: 202, headers: { 'x-rewritten': '1' } } }],
        log: [],
        debugPass: true,
      };
      const guard = {};
      let error = null;
      try {
        await fn(session, state, {
          requestId: 'R-OK',
          request: { url: 'https://example.com/api/success', method: 'GET' },
          responseStatusCode: 200,
          responseStatusText: 'OK',
          responseHeaders: [{ name: 'content-type', value: 'application/json' }],
        }, guard);
      } catch (e) {
        error = e;
      }
      check('5b. 最外层成功路径不抛错', !error, error && error.message);
      check(
        '5c. 最外层成功路径 continueResponse 恰好一次、continueRequest 零次',
        countOf(calls, 'Fetch.continueResponse') === 1 && countOf(calls, 'Fetch.continueRequest') === 0,
        JSON.stringify(methodsOf(calls)),
      );
      check('5d. 最外层成功路径把 guard 标记为已落定', guard.settled === true, JSON.stringify(guard));
      check(
        '5e. 最外层成功路径写入一条 rewrite 日志（stage=response/status=202）',
        state.log.some((item) => item.requestId === 'R-OK' && item.action === 'rewrite' && item.stage === 'response' && item.status === 202),
        JSON.stringify(state.log),
      );
    }
  }

  // 6) 最外层入口：continueResponse 失败时仍必须由外层补一次 continueRequest。
  {
    const { calls, context } = buildContext((method) => {
      if (method === 'Fetch.continueResponse') throw new Error('Invalid InterceptionId');
      return {};
    });
    const fn = context.handleFetchPausedResponse;
    if (typeof fn === 'function') {
      const session = { tabId: 1, intercept: { stats: {} } };
      const state = {
        rules: [{ match: '/api/fallback', response: { status: 202 } }],
        log: [],
        debugPass: true,
      };
      const guard = {};
      let error = null;
      try {
        await fn(session, state, {
          requestId: 'R-FB',
          request: { url: 'https://example.com/api/fallback', method: 'GET' },
          responseStatusCode: 200,
          responseStatusText: 'OK',
          responseHeaders: [{ name: 'content-type', value: 'application/json' }],
        }, guard);
      } catch (e) {
        error = e;
      }
      check('6. 最外层回退路径不抛错', !error, error && error.message);
      check(
        '6b. 最外层回退路径至少一次 continueResponse + 恰好一次 continueRequest',
        countOf(calls, 'Fetch.continueResponse') >= 1 && countOf(calls, 'Fetch.continueRequest') === 1,
        JSON.stringify(methodsOf(calls)),
      );
      check('6c. 最外层回退路径同样标记 guard 落定', guard.settled === true, JSON.stringify(guard));
    }
  }

  // 4) 首选与回退都失败：不得抛错（requestPaused 处理器不能被异常打断）。
  {
    const { error } = await runCase(() => {
      throw new Error('boom');
    });
    check('4. 两条续传路径都失败时不抛错', !error, error && error.message);
  }

  const failed = results.filter((item) => !item.ok);
  console.log(`\n${results.length - failed.length}/${results.length} passed`);
  if (failed.length) {
    console.log('failed:');
    for (const item of failed) console.log('  - ' + item.name);
    process.exit(1);
  }
  process.exit(0);
}

main().catch((error) => {
  console.error('FATAL', error);
  process.exit(1);
});
