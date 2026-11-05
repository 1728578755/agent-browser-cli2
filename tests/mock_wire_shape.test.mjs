// mock 扩展与真实扩展的 WS 回包形状对齐。
//
// daemon 侧只认一种形状：
//   /open  → `targets[]` 由 daemon 组装，扩展回包是 `{id,url,title,active,windowId,window,group}`
//   /exec  → `result.js_return` 来自页面脚本的 data 层（扩展已剥壳，WS 发送裸值）
//   pageOp → WS 发送 `{op, value}`（handlePageOp 的 `{ok,data}` 已被 socketSend 剥掉）
//   cdp    → WS 发送裸 CDP 结果（如 `{nodes:[...]}`），不再带 `{ok,data}`
//   batch  → WS 发送 `{ok, results, count, failed}`（整对象不剥层）
//   status → WS 发送 `{extensionVersion,...}`（剥掉 data 层）
//
// 这里起一个最小 WS 服务端，让 mock_extension.mjs 真的连上来，再逐条发命令，
// 断言它发出的 result 与真实扩展一致——而不是只比对源码字面量。
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import test from 'node:test';

import { startMockExtension } from './mock_extension.mjs';

const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

function encodeFrame(text) {
  const payload = Buffer.from(text, 'utf8');
  const length = payload.length;
  let header;
  if (length < 126) {
    header = Buffer.from([0x81, length]);
  } else if (length < 65536) {
    header = Buffer.alloc(4);
    header[0] = 0x81;
    header[1] = 126;
    header.writeUInt16BE(length, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x81;
    header[1] = 127;
    header.writeBigUInt64BE(BigInt(length), 2);
  }
  return Buffer.concat([header, payload]);
}

function decodeFrames(buffer, onText) {
  let rest = buffer;
  for (;;) {
    if (rest.length < 2) return rest;
    const opcode = rest[0] & 0x0f;
    const masked = (rest[1] & 0x80) !== 0;
    let length = rest[1] & 0x7f;
    let offset = 2;
    if (length === 126) {
      if (rest.length < 4) return rest;
      length = rest.readUInt16BE(2);
      offset = 4;
    } else if (length === 127) {
      if (rest.length < 10) return rest;
      length = Number(rest.readBigUInt64BE(2));
      offset = 10;
    }
    let mask = null;
    if (masked) {
      if (rest.length < offset + 4) return rest;
      mask = rest.subarray(offset, offset + 4);
      offset += 4;
    }
    if (rest.length < offset + length) return rest;
    const payload = Buffer.from(rest.subarray(offset, offset + length));
    if (mask) for (let i = 0; i < payload.length; i += 1) payload[i] ^= mask[i % 4];
    rest = rest.subarray(offset + length);
    if (opcode === 0x1) onText(payload.toString('utf8'));
  }
}

async function startFakeDaemon() {
  const pending = new Map();
  const seen = [];
  let extReadyResolve;
  const extReady = new Promise((resolve) => {
    extReadyResolve = resolve;
  });
  let socket = null;

  const server = createServer();
  server.on('upgrade', (req, rawSocket) => {
    const accept = createHash('sha1').update(String(req.headers['sec-websocket-key']) + WS_GUID).digest('base64');
    rawSocket.write(
      'HTTP/1.1 101 Switching Protocols\r\n' +
        'Upgrade: websocket\r\n' +
        'Connection: Upgrade\r\n' +
        `Sec-WebSocket-Accept: ${accept}\r\n\r\n`,
    );
    socket = rawSocket;
    let buffer = Buffer.alloc(0);
    rawSocket.on('data', (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      buffer = decodeFrames(buffer, (text) => {
        let message;
        try {
          message = JSON.parse(text);
        } catch {
          return;
        }
        if (message.type === 'ext_ready') {
          extReadyResolve(message);
          return;
        }
        seen.push(message);
        // 每条命令先回 ack，再回 result/error；ack 不是终态。
        if (message.type === 'ack') return;
        if (message.id && pending.has(message.id)) {
          const resolve = pending.get(message.id);
          pending.delete(message.id);
          resolve(message);
        }
      });
    });
  });

  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  const mock = startMockExtension({ port });

  const send = (id, payload) =>
    new Promise((resolve, reject) => {
      if (!socket) {
        reject(new Error('mock extension 尚未连接'));
        return;
      }
      pending.set(id, resolve);
      // 真实协议：{ id, code }，code 是字符串或 JSON 字符串；cmd/方法参数都在 code 里。
      const wire =
        typeof payload === 'string'
          ? { id, code: payload }
          : { id, code: JSON.stringify(payload) };
      socket.write(encodeFrame(JSON.stringify(wire)));
      setTimeout(() => {
        if (!pending.has(id)) return;
        pending.delete(id);
        reject(new Error(`命令 ${id} 超时未回包`));
      }, 3000).unref?.();
    });

  return {
    port,
    extReady,
    send,
    seen,
    close() {
      mock.close();
      try {
        socket?.end();
      } catch {
        /* ignore */
      }
      try {
        socket?.destroy();
      } catch {
        /* ignore */
      }
      server.close();
    },
  };
}

function pickKeys(object) {
  return Object.keys(object || {}).sort();
}

test('mock 扩展的 WS 回包形状与真实扩展一致', async (t) => {
  const daemon = await startFakeDaemon();
  t.after(() => daemon.close());

  const ready = await daemon.extReady;
  assert.equal(ready.type, 'ext_ready');
  assert.ok(Array.isArray(ready.tabs), 'ext_ready 应带 tabs[]');

  // ---- openTab：剥 data 层后的目标对象 ----
  const opened = await daemon.send('open-1', {
    cmd: 'openTab',
    url: 'https://example.com/wire',
    active: false,
  });
  assert.equal(opened.type, 'result');
  const openResult = opened.result;
  for (const key of ['id', 'url', 'title', 'active', 'windowId', 'window', 'group']) {
    assert.ok(key in openResult, `open 回包缺少 ${key}：${JSON.stringify(openResult)}`);
  }
  assert.ok(!('data' in openResult), `open 回包不应带 data 层：${pickKeys(openResult)}`);
  assert.equal(openResult.active, false);

  // ---- exec：WS result 是页面脚本剥壳后的裸值 ----
  const exec = await daemon.send('exec-1', 'return document.title');
  assert.equal(exec.type, 'result');
  assert.equal(typeof exec.result, 'string', `exec 应回裸值，实际 ${JSON.stringify(exec.result)}`);
  assert.equal(typeof exec.result === 'object' && exec.result !== null, false, 'exec 不应再包 {ok,data}');

  // ---- pageOp：WS 发送 {op, value}，不包 ok/data ----
  const pageOp = await daemon.send('pageop-1', {
    cmd: 'pageOp',
    op: 'read',
    args: {},
    tabId: 101,
  });
  const pageOpResult = pageOp.result;
  assert.equal(pageOpResult.op, 'read');
  assert.ok('value' in pageOpResult, `pageOp 回包缺少 value：${JSON.stringify(pageOpResult)}`);
  assert.ok(!('data' in pageOpResult) && !('ok' in pageOpResult), `pageOp 不应包 data/ok：${pickKeys(pageOpResult)}`);

  // ---- cdp：WS 发送裸 CDP 结果 ----
  const cdp = await daemon.send('cdp-1', {
    cmd: 'cdp',
    method: 'Accessibility.getFullAXTree',
    tabId: 101,
  });
  const cdpResult = cdp.result;
  assert.ok(Array.isArray(cdpResult.nodes), `cdp 应直接回 {nodes}：${JSON.stringify(cdpResult)}`);
  assert.ok(!('data' in cdpResult) && !('ok' in cdpResult), `cdp 不应包 data/ok：${pickKeys(cdpResult)}`);

  // ---- batch：整对象 {ok, results, count, failed} ----
  const batch = await daemon.send('batch-1', {
    cmd: 'batch',
    tabId: 101,
    commands: [{ method: 'Runtime.callFunctionOn', params: {} }],
  });
  const batchResult = batch.result;
  assert.equal(batchResult.ok, true);
  assert.ok(Array.isArray(batchResult.results), 'batch 应带 results[]');
  assert.equal(batchResult.count, batchResult.results.length);
  assert.equal(batchResult.failed, 0);
  assert.ok(!('data' in batchResult), 'batch 不应包 data 层');

  // ---- status：剥 data 层后的状态对象 ----
  const status = await daemon.send('status-1', { cmd: 'status' });
  const statusResult = status.result;
  assert.equal(typeof statusResult.extensionVersion, 'string');
  assert.ok(!('data' in statusResult) && !('ok' in statusResult), `status 不应包 data/ok：${pickKeys(statusResult)}`);

  // 反向守卫：mock 自己的入口消息不能被误当成命令回包。
  const ids = daemon.seen.filter((item) => item.type === 'result').map((item) => item.id);
  assert.deepEqual(new Set(ids), new Set(['open-1', 'exec-1', 'pageop-1', 'cdp-1', 'batch-1', 'status-1']));
});
