// 扩展 CSP 策略 + 可选 API 加载鲁棒性测试。
//
// 这里用一个「形状齐全」的假 chrome 在 vm 里真正执行 background.js：
// 只有真执行才能发现「某个 API 在本机不存在时整个后台脚本直接抛错」这类问题
// （回归守卫：Chrome 版本差异 / 精简构建下 webNavigation、debugger 可能缺失）。
import assert from 'node:assert/strict';
import { webcrypto } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

const backgroundPath = new URL('../assets/tmwd_cdp_bridge/background.js', import.meta.url);
const contentPath = new URL('../assets/tmwd_cdp_bridge/content.js', import.meta.url);

function eventTarget(listeners = []) {
  return {
    addListener(listener) {
      listeners.push(listener);
    },
    hasListener(listener) {
      return listeners.includes(listener);
    },
    removeListener() {},
    get listeners() {
      return listeners;
    },
  };
}

// background.js 顶层会直接调用的 API 必须在这里齐全；用到的其余 API 一并补齐，
// 保证任何一条消息处理路径被触发时也不会因为「形状不对」而误报。
function createChromeStub({ omit = [] } = {}) {
  const onInstalled = [];
  const onMessage = [];
  const onCommitted = [];
  const debuggerEvents = [];
  const debuggerDetach = [];
  const dynamicRuleCalls = [];

  const chrome = {
    alarms: {
      clear: async () => true,
      create() {},
      onAlarm: eventTarget(),
    },
    contentSettings: new Proxy(
      {},
      {
        get: () => ({
          async set() {},
          async get() {
            return { setting: 'allow' };
          },
        }),
      },
    ),
    cookies: {
      async getAll() {
        return [];
      },
    },
    debugger: {
      async attach() {},
      async detach() {},
      async getTargets() {
        return [];
      },
      async sendCommand() {
        return {};
      },
      onDetach: eventTarget(debuggerDetach),
      onEvent: eventTarget(debuggerEvents),
    },
    declarativeNetRequest: {
      async updateDynamicRules(update) {
        dynamicRuleCalls.push(update);
      },
    },
    downloads: {
      async cancel() {},
      async download() {
        return 1;
      },
      async erase() {
        return [];
      },
      async search() {
        return [];
      },
    },
    extension: {
      async isAllowedFileSchemeAccess() {
        return false;
      },
    },
    management: {
      async getAll() {
        return [];
      },
      async setEnabled() {},
    },
    runtime: {
      id: 'test-extension-id',
      getManifest() {
        return { version: 'test' };
      },
      onInstalled: eventTarget(onInstalled),
      onMessage: eventTarget(onMessage),
      onStartup: eventTarget(),
      reload() {},
    },
    scripting: {
      async executeScript() {
        return [];
      },
      async getRegisteredContentScripts() {
        return [];
      },
      async registerContentScripts() {},
      async unregisterContentScripts() {},
    },
    storage: {
      local: {
        async get(defaults) {
          return defaults ?? {};
        },
        async set() {},
      },
    },
    tabGroups: {
      async query() {
        return [];
      },
      async update() {
        return {};
      },
    },
    tabs: {
      async create() {
        return { id: 1 };
      },
      async get() {
        return { id: 1, url: 'https://example.com/' };
      },
      async group() {
        return 1;
      },
      async query() {
        return [];
      },
      async remove() {},
      async update() {
        return { id: 1 };
      },
      onCreated: eventTarget(),
      onRemoved: eventTarget(),
      onUpdated: eventTarget(),
    },
    webNavigation: {
      async getAllFrames() {
        return [];
      },
      onCommitted: eventTarget(onCommitted),
    },
    windows: {
      async create() {
        return { id: 1 };
      },
      async update() {
        return {};
      },
    },
  };

  for (const key of omit) delete chrome[key];

  return {
    chrome,
    lists: { debuggerDetach, debuggerEvents, onCommitted, onInstalled, onMessage },
    dynamicRuleCalls,
  };
}

function createBackgroundContext({ omit = [] } = {}) {
  const { chrome, lists, dynamicRuleCalls } = createChromeStub({ omit });

  class FakeWebSocket {}
  FakeWebSocket.CONNECTING = 0;
  FakeWebSocket.OPEN = 1;
  FakeWebSocket.CLOSING = 2;
  FakeWebSocket.CLOSED = 3;

  return {
    context: vm.createContext({
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
    }),
    dynamicRuleCalls,
    lists,
  };
}

async function flushMicrotasks() {
  for (let i = 0; i < 5; i += 1) await Promise.resolve();
}

test('extension startup removes the legacy CSP rule without installing a replacement', async () => {
  const source = await readFile(backgroundPath, 'utf8');
  const { context, dynamicRuleCalls, lists } = createBackgroundContext();

  vm.runInContext(source, context, { filename: backgroundPath.pathname });
  await flushMicrotasks();

  assert.deepEqual(JSON.parse(JSON.stringify(dynamicRuleCalls[0])), { removeRuleIds: [9999] });
  assert.ok(dynamicRuleCalls.every((update) => !('addRules' in update)));

  for (const listener of lists.onInstalled) await listener({ reason: 'update' });
  assert.ok(dynamicRuleCalls.every((update) => !('addRules' in update)));
});

test('content script leaves page CSP metadata untouched', async () => {
  const source = await readFile(contentPath, 'utf8');
  assert.doesNotMatch(source, /Content-Security-Policy/i);
});

// 回归守卫：background.js 是经典脚本，顶层任何一次属性读取抛错都会让整个
// service worker 起不来（扩展「连不上」但没有任何报错）。所以可选的 chrome
// 命名空间必须用可选链访问。
test('background.js loads when optional chrome APIs (webNavigation/debugger) are missing', async () => {
  const source = await readFile(backgroundPath, 'utf8');
  const { context } = createBackgroundContext({ omit: ['webNavigation', 'debugger'] });

  assert.doesNotThrow(() => {
    vm.runInContext(source, context, { filename: backgroundPath.pathname });
  }, '缺少 chrome.webNavigation / chrome.debugger 时 background.js 仍应能加载');
  await flushMicrotasks();
});

test('background.js registers the navigation and debugger listeners when the APIs exist', async () => {
  const source = await readFile(backgroundPath, 'utf8');
  const { context, lists } = createBackgroundContext();

  vm.runInContext(source, context, { filename: backgroundPath.pathname });
  await flushMicrotasks();

  assert.equal(lists.onCommitted.length, 1, '应当注册一个 webNavigation.onCommitted 监听器');
  assert.equal(lists.debuggerEvents.length, 1, '应当注册一个 debugger.onEvent 监听器');
  assert.equal(lists.debuggerDetach.length, 1, '应当注册一个 debugger.onDetach 监听器');
});
