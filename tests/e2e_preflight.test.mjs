// e2e 前置条件与 CI 接入守卫。
//
// scripts/e2e.mjs 是顶层 await 的入口脚本，直接 import 会把整套 e2e 跑起来；
// 所以这里做源码级契约断言：锁定「扩展未连接 → 退出 2」「stale → 退出 2」
// 「API 端口不可连 → 退出 2」三条闸门，以及 CI 的 e2e-live job 确实调用它。
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..');
const E2E = readFileSync(join(ROOT, 'scripts', 'e2e.mjs'), 'utf8');
const CI = readFileSync(join(ROOT, '.github', 'workflows', 'ci.yml'), 'utf8');

/// 断言「代码里没有 X」时必须先去注释：e2e.mjs 里恰好有一段解释旧写法的注释。
const stripComments = (text) =>
  text
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:"'\\])\/\/[^\n]*/g, '$1');

function sliceFn(text, name) {
  const start = text.indexOf(`async function ${name}(`);
  assert.ok(start >= 0, `找不到 async function ${name}`);
  let depth = 0;
  let seen = false;
  for (let i = start; i < text.length; i += 1) {
    if (text[i] === '{') {
      depth += 1;
      seen = true;
    } else if (text[i] === '}') {
      depth -= 1;
      if (seen && depth === 0) return text.slice(start, i + 1);
    }
  }
  return null;
}

test('preflight 把「扩展未连接」收紧为硬失败', () => {
  const body = sliceFn(stripComments(E2E), 'preflight');
  assert.ok(body, '应能定位 preflight()');
  // 旧实现是 `!connected && activeTabs === 0`：只要残留 active_tabs>0 就放行。
  assert.doesNotMatch(body, /!connected\s*&&\s*activeTabs/, '不得再用 activeTabs 作为放行条件');
  assert.match(body, /if\s*\(!connected\)\s*\{[\s\S]*?return\s+2;/, '未连接必须 return 2');
  assert.match(body, /activeTabs/, 'active_tabs 仍应出现在报错 detail 里');
});

test('preflight 校验扩展 stale 与 API 端口占用', () => {
  const body = sliceFn(E2E, 'preflight');
  assert.match(body, /extensionStale\s*!==\s*false/, 'stale 不是明确的 false 就应拒绝');
  assert.match(body, /return\s+2;/, 'stale 分支必须退出 2');
  assert.match(body, /probePort\s*\(/, '应真正探测端口，而不是只看 status.ports');
  assert.match(body, /18767/, '默认 API 端口必须是 18767');
  assert.match(body, /portState\s*!==\s*'open'/, '端口不可连必须判失败');
});

test('e2e 场景清单完整', () => {
  for (const name of ['basic-flow', 'find-redaction', 'screenshot', 'spa-wait', 'scrape', 'download']) {
    assert.match(E2E, new RegExp(`name:\\s*'${name.replace('-', '\\-')}'`), `缺少场景 ${name}`);
  }
  assert.match(E2E, /SMOKE_NAMES/, 'CI 用 smoke 子集必须有显式白名单');
});

test('CI 的 e2e-live job 真正执行 scripts/e2e.mjs', () => {
  assert.match(CI, /^\s{2}e2e-live:/m, 'ci.yml 应有 e2e-live job');
  const job = CI.slice(CI.indexOf('e2e-live:'));
  assert.match(job, /node scripts\/e2e\.mjs/, 'job 内必须执行 e2e.mjs');
  assert.match(job, /ATB_E2E:\s*smoke/, 'CI 应使用 smoke 子集控制时长');
  // 刻意不锁 `--load-extension` / `setup-chrome` / `xvfb-run`：Chrome for Testing 的
  // 扩展加载方式可能随版本调整（例如换 flag 或改用别的安装器），那是 packaging 的
  // 实现细节；测试只守「CI 里确实跑了 e2e.mjs + smoke」这条不会随环境变化的契约。
  assert.doesNotMatch(job, /--only=\S+/, 'CI 不应只跑单个场景');
});
