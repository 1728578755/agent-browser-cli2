// 扩展 WS 生命周期回归。
//
// 现有测试把 setTimeout/setInterval 全 stub 成 `() => 1`、把 WebSocket 换成空类，
// 于是「心跳是否每 30s 发一次」「90s 无 pong 是否重连」「daemon 重启/断开后是否
// 重新调度」「token 轮换是否换一条连接」「同 id 重发是否只执行一次」全部零覆盖。
// 这里用可控假时钟 + 可触发的假 WebSocket，在 vm 里真正执行 background.js。
import assert from 'node:assert/strict';
import { webcrypto } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const HERE = dirname(fileURLToPath(import.meta.url));
const BACKGROUND = join(HERE, '..', 'assets', 'atb_bridge', 'background.js');
const SOURCE = readFileSync(BACKGROUND, 'utf8');

const flush = async (rounds = 12) => {
  for (let i = 0; i < rounds; i += 1) await Promise.resolve();
};

function eventTarget() {
  const listeners = [];
  return {
    addListener(fn) {
      listeners.push(fn);
    },
    removeListener(fn) {
      const at = listeners.indexOf(fn);
      if (at >= 0) listeners.splice(at, 1);
    },
    hasListener(fn) {
      return listeners.includes(fn);
    },
    _emit(...args) {
      for (const fn of [...listeners]) fn(...args);
    },
    get listeners() {
      return listeners;
    },
  };
}

function buildHarness() {
  // ---- 可控假时钟 ----
  let now = 0;
  let seq = 1;
  const timers = new Map();
  const clock = {
    timers,
    now: () => now,
    setTimeout(fn, ms = 0) {
      const id = seq++;
      timers.set(id, { at: now + Math.max(0, Number(ms) || 0), fn, interval: null });
      return id;
    },
    setInterval(fn, ms = 0) {
      const id = seq++;
      const period = Math.max(1, Number(ms) || 0);
      timers.set(id, { at: now + period, fn, interval: period });
      return id;
    },
    clear(id) {
      timers.delete(id);
    },
    async advance(ms) {
      const target = now + ms;
      for (;;) {
        let next = null;
        for (const [id, timer] of timers) {
          if (timer.at <= target && (!next || timer.at < next[1].at)) next = [id, timer];
        }
        if (!next) break;
        const [id, timer] = next;
        now = timer.at;
        if (timer.interval) timer.at = now + timer.interval;
        else timers.delete(id);
        timer.fn();
        await flush(4);
      }
      now = target;
      await flush(4);
    },
  };

  // Date.now 必须跟假时钟同源：background.js 的静默判定用的是 Date.now()，
  // 只改 setTimeout 而留原生 Date，会把「90s 无 pong」测成 0s。
  class FakeDate extends Date {
    constructor(...args) {
      super(...(args.length ? args : [now]));
    }
    static now() {
      return now;
    }
  }

  // ---- 假 WebSocket（测试可手动 open/close/message）----
  const sockets = [];
  class FakeWebSocket {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;
    constructor(url) {
      this.url = String(url);
      this.readyState = 0;
      this.sent = [];
      this.onopen = null;
      this.onmessage = null;
      this.onclose = null;
      this.onerror = null;
      sockets.push(this);
    }
    send(payload) {
      if (this.readyState !== 1) throw new Error('socket is not open');
      this.sent.push(String(payload));
    }
    close() {
      this.readyState = 3;
    }
    async open() {
      this.readyState = 1;
      if (this.onopen) await this.onopen({});
      await flush();
    }
    async message(payload) {
      const text = typeof payload === 'string' ? payload : JSON.stringify(payload);
      if (this.onmessage) await this.onmessage({ data: text });
      await flush();
    }
    async closeByPeer(code = 1006) {
      this.readyState = 3;
      if (this.onclose) await this.onclose({ code, wasClean: false });
      await flush();
    }
  }

  // ---- chrome 桩 ----
  let serverAlive = true;
  const alarmCreates = [];
  const storage = { wsPort: 18765, wsToken: '' };
  const storageChanged = eventTarget();
  const execCalls = [];
  let execImpl = async () => [{ result: { ok: true, data: 1 } }];

  const chrome = {
    alarms: {
      create(name, info) {
        alarmCreates.push({ name, info });
      },
      clear: async () => true,
      get: async () => null,
      onAlarm: eventTarget(),
    },
    contentSettings: new Proxy({}, { get: () => ({ async set() {} }) }),
    cookies: { getAll: async () => [] },
    debugger: {
      attach: async () => {},
      detach: async () => {},
      getTargets: async () => [],
      sendCommand: async () => ({}),
      onEvent: eventTarget(),
      onDetach: eventTarget(),
    },
    declarativeNetRequest: { updateDynamicRules: async () => {} },
    downloads: { download: async () => 1, search: async () => [], cancel: async () => {}, erase: async () => [] },
    extension: { isAllowedFileSchemeAccess: async () => false },
    management: { getAll: async () => [], setEnabled: async () => {} },
    runtime: {
      id: 'test-extension-id',
      getManifest: () => ({ version: '5.2.9', manifest_version: 3 }),
      getURL: (path) => 'chrome-extension://test-extension-id/' + path,
      onInstalled: eventTarget(),
      onMessage: eventTarget(),
      onStartup: eventTarget(),
      onConnect: eventTarget(),
      reload() {},
    },
    scripting: {
      async executeScript(...args) {
        execCalls.push(args);
        return execImpl(...args);
      },
      getRegisteredContentScripts: async () => [],
      registerContentScripts: async () => {},
      unregisterContentScripts: async () => {},
    },
    storage: {
      local: {
        async get(defaults) {
          const out = Object.assign({}, defaults || {});
          for (const key of Object.keys(out)) if (key in storage) out[key] = storage[key];
          return out;
        },
        async set(values) {
          Object.assign(storage, values);
        },
      },
      onChanged: storageChanged,
    },
    tabGroups: { query: async () => [], update: async () => ({}) },
    tabs: {
      query: async () => [{ id: 1, url: 'https://example.com/', title: 'example', active: true, windowId: 1 }],
      get: async (tabId) => ({ id: tabId, url: 'https://example.com/', title: 'example', windowId: 1, active: true }),
      create: async () => ({ id: 1, url: 'https://example.com/', title: 'example', active: true, windowId: 1 }),
      update: async (tabId, info) => Object.assign({ id: tabId, windowId: 1 }, info),
      remove: async () => {},
      group: async () => 1,
      sendMessage: async () => ({}),
      onCreated: eventTarget(),
      onRemoved: eventTarget(),
      onUpdated: eventTarget(),
    },
    webNavigation: { getAllFrames: async () => [], onCommitted: eventTarget() },
    windows: { create: async () => ({ id: 1, tabs: [] }), update: async () => ({}), get: async () => ({ state: 'normal' }) },
  };

  const context = vm.createContext({
    AbortController,
    TextDecoder,
    TextEncoder,
    WebSocket: FakeWebSocket,
    chrome,
    console: { log() {}, warn() {}, error() {} },
    crypto: webcrypto,
    fetch: async () => {
      if (!serverAlive) throw new Error('daemon unavailable');
      return { ok: true, text: async () => '' };
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clear,
    setInterval: clock.setInterval,
    clearInterval: clock.clear,
    btoa: (value) => Buffer.from(String(value), 'binary').toString('base64'),
    atob: (value) => Buffer.from(String(value), 'base64').toString('binary'),
    performance,
    URL,
    Date: FakeDate,
    Math,
    JSON,
    Promise,
    Uint8Array,
    ArrayBuffer,
  });

  vm.runInContext(SOURCE, context, { filename: BACKGROUND });

  return {
    context,
    chrome,
    sockets,
    clock,
    alarmCreates,
    storageChanged,
    execCalls,
    setServerAlive(value) {
      serverAlive = value;
    },
    setExecImpl(fn) {
      execImpl = fn;
    },
    getStorage: () => storage,
  };
}

async function boot() {
  const harness = buildHarness();
  await flush(16);
  return harness;
}

function currentSocket(harness) {
  const socket = harness.sockets[harness.sockets.length - 1];
  assert.ok(socket, 'background.js 应该尝试建立 WebSocket');
  return socket;
}

test('心跳每 30s 发一次 ping，连接换代后旧 interval 停止', async () => {
  const harness = await boot();
  const socket = currentSocket(harness);
  await socket.open();
  socket.sent.length = 0;

  await harness.clock.advance(30000);
  const pings1 = socket.sent.filter((item) => item.includes('"type":"ping"')).length;
  assert.equal(pings1, 1, `30s 应发一次 ping，实际 ${pings1}；sent=${JSON.stringify(socket.sent)}`);

  await harness.clock.advance(30000);
  const pings2 = socket.sent.filter((item) => item.includes('"type":"ping"')).length;
  assert.equal(pings2, 2, `60s 应累计两次 ping，实际 ${pings2}`);

  // 旧 socket 被替换后，它的 interval 必须自清（否则旧连接上继续 send 会刷错）。
  socket.readyState = 3;
  await harness.clock.advance(30000);
  const pings3 = socket.sent.filter((item) => item.includes('"type":"ping"')).length;
  assert.equal(pings3, 2, 'socket 关闭后不应继续 ping');
});

test('90s 无 pong 且 daemon 已不在监听：丢弃半开连接并调度重连', async () => {
  const harness = await boot();
  const socket = currentSocket(harness);
  // 先推进 10ms 再 open：lastPongAt 为 0 会被实现当成「从未收到过」，静默判定不触发。
  await harness.clock.advance(10);
  await socket.open();
  const before = harness.sockets.length;
  harness.setServerAlive(false);

  // 让 fake clock 走到 90s 之后：lastPongAt 停在 0，silenceMs > 75s 阈值。
  await harness.clock.advance(90001);
  const keepalive = harness.chrome.alarms.onAlarm;
  keepalive._emit({ name: 'tmwd-ws-keepalive' });
  await flush(20);

  assert.equal(socket.readyState, 3, '半开连接应被 close()');
  assert.ok(
    harness.alarmCreates.some((item) => item.name === 'tmwd-ws-probe'),
    `应重新排 probe alarm，实际 ${JSON.stringify(harness.alarmCreates.map((i) => i.name))}`,
  );

  // fast retry 由 finally 调度：daemon 恢复后 300ms 内应尝试新建连接。
  harness.setServerAlive(true);
  await harness.clock.advance(400);
  await flush(20);
  assert.ok(harness.sockets.length > before, '应创建新的 WebSocket（快速重连）');
});

test('daemon 重启（onclose）即使清理步骤抛错，也在 finally 里调度重连', async () => {
  const harness = await boot();
  const socket = currentSocket(harness);
  await socket.open();
  const before = harness.sockets.length;
  // 让 onSocketClosed 内部的 detachAllDebugSessions 抛错：重连调度不能被它带走。
  harness.chrome.debugger.getTargets = async () => {
    throw new Error('debugger gone during shutdown');
  };

  await socket.closeByPeer(1006);
  assert.ok(
    harness.alarmCreates.some((item) => item.name === 'tmwd-ws-probe'),
    'onclose 的 finally 应排 probe',
  );
  harness.setServerAlive(true);
  await harness.clock.advance(400);
  await flush(20);
  assert.ok(harness.sockets.length > before, '清理步骤失败后仍应快速重连');
});

test('token 轮换只换一条新连接，并把 token 带进 URL', async () => {
  const harness = await boot();
  const first = currentSocket(harness);
  await first.open();
  assert.ok(!first.url.includes('token='), `无 token 时不应拼查询串：${first.url}`);

  // 真实 Chrome 里 popup/CLI 先写 storage，再触发 onChanged；这里保持一致。
  harness.getStorage().wsToken = 'rotated-token';
  harness.storageChanged._emit({ wsToken: { oldValue: '', newValue: 'rotated-token' } }, 'local');
  await flush(24);

  const second = currentSocket(harness);
  assert.notEqual(second, first, 'token 变化应重建连接');
  assert.equal(first.readyState, 3, '旧 socket 必须先被关闭');
  assert.match(second.url, /[?&]token=rotated-token/, `新连接 URL 应带 token：${second.url}`);
  // 值没变时不应无限重连。
  const count = harness.sockets.length;
  harness.storageChanged._emit({ wsToken: { oldValue: 'rotated-token', newValue: 'rotated-token' } }, 'local');
  await flush(12);
  assert.equal(harness.sockets.length, count, 'token 值未变化时不应重建连接');
});

test('同 id 重发：running 回 duplicate_inflight，done 回放结果且不重复执行', async () => {
  const harness = await boot();
  const socket = currentSocket(harness);
  await socket.open();
  socket.sent.length = 0;

  let release;
  let execCount = 0;
  harness.setExecImpl(() => {
    execCount += 1;
    return new Promise((resolve) => {
      release = () => resolve([{ result: { ok: true, data: 42 } }]);
    });
  });

  const command = { id: 'cmd-1', tabId: 1, code: 'return 42' };
  // 第一次：登记 running 并开始执行（不 await，保持 in-flight）。
  const firstRun = socket.message(command);
  await flush(6);

  // 第二次：同 id、仍 running → 立即回 duplicate_inflight，不进入执行。
  await socket.message(command);
  const duplicate = socket.sent
    .map((item) => JSON.parse(item))
    .find((item) => item.type === 'error' && item.id === 'cmd-1');
  assert.ok(duplicate, `应回一条 duplicate 错误，实际 ${JSON.stringify(socket.sent)}`);
  assert.equal(duplicate.error?.code, 'duplicate_inflight');
  assert.equal(execCount, 1, `running 期间的重复命令不应再次执行，实际执行 ${execCount} 次`);

  // 放行原命令 → 登记 done。
  release();
  await firstRun;
  await flush(10);
  const resultMessage = socket.sent
    .map((item) => JSON.parse(item))
    .find((item) => item.type === 'result' && item.id === 'cmd-1');
  assert.ok(resultMessage, `首次执行应回 result，实际 ${JSON.stringify(socket.sent)}`);
  assert.equal(resultMessage.result, 42);

  // 第三次：同 id、已 done → 回放原结果，不再执行。
  socket.sent.length = 0;
  await socket.message(command);
  const replay = socket.sent.map((item) => JSON.parse(item)).find((item) => item.id === 'cmd-1');
  assert.ok(replay, 'done 后重发应直接回放');
  assert.equal(replay.type, 'result');
  assert.equal(replay.result, 42);
  assert.equal(execCount, 1, `done 后重发不能再执行，实际执行 ${execCount} 次`);
});
