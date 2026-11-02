// 资源一致性守卫。
//
// assets/page_*.js 是「源文件」，assets/tmwd_cdp_bridge/ops/page_*.js 是扩展里
// 真正被 scripting.executeScript 注入的那一份。两份靠复制同步，历史上就出现过
// 只改一边、线上跑的是旧逻辑的问题（TOKEN_SELECTORS 被删掉就是这么漏出去的）。
// 这里做逐字节比对，并给出差异偏移，方便定位。
import { existsSync, readFileSync, statSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..');
const ASSETS = join(ROOT, 'assets');
const OPS = join(ASSETS, 'tmwd_cdp_bridge', 'ops');

const PAGES = ['action', 'challenge', 'find', 'net', 'read', 'snapshot', 'wait'];

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${ok || !detail ? '' : '\n        → ' + detail}`);
}

function firstDiffOffset(a, b) {
  const limit = Math.min(a.length, b.length);
  for (let i = 0; i < limit; i += 1) {
    if (a[i] !== b[i]) return i;
  }
  return limit;
}

function describeMismatch(sourceBuf, copyBuf, offset) {
  const lineOf = (buf, at) => {
    let line = 1;
    for (let i = 0; i < at && i < buf.length; i += 1) if (buf[i] === 0x0a) line += 1;
    return line;
  };
  const snippet = (buf, at) => {
    const from = Math.max(0, at - 30);
    const to = Math.min(buf.length, at + 30);
    return JSON.stringify(buf.slice(from, to).toString('utf8'));
  };
  return [
    `首个差异偏移 ${offset}（源第 ${lineOf(sourceBuf, offset)} 行 / 副本第 ${lineOf(copyBuf, offset)} 行）`,
    `  源 : ${snippet(sourceBuf, offset)}`,
    `  副本: ${snippet(copyBuf, offset)}`,
  ].join('\n        ');
}

for (const page of PAGES) {
  const sourcePath = join(ASSETS, `page_${page}.js`);
  const copyPath = join(OPS, `page_${page}.js`);
  const label = `page_${page}.js 与 ops/page_${page}.js 逐字节一致`;

  if (!existsSync(sourcePath)) {
    check(label, false, `缺少源文件 assets/page_${page}.js`);
    continue;
  }
  if (!existsSync(copyPath)) {
    check(label, false, `缺少副本 assets/tmwd_cdp_bridge/ops/page_${page}.js`);
    continue;
  }

  const sourceBuf = readFileSync(sourcePath);
  const copyBuf = readFileSync(copyPath);

  if (sourceBuf.equals(copyBuf)) {
    check(label, true);
    continue;
  }

  const offset = firstDiffOffset(sourceBuf, copyBuf);
  const newer =
    statSync(sourcePath).mtimeMs >= statSync(copyPath).mtimeMs ? 'assets（源）' : 'ops（副本）';
  check(
    label,
    false,
    [
      `字节数  源=${sourceBuf.length}  副本=${copyBuf.length}  差=${copyBuf.length - sourceBuf.length}`,
      `较新的一边：${newer}`,
      describeMismatch(sourceBuf, copyBuf, offset),
      '修复方式：node scripts/sync-extension-assets.mjs（以 assets/ 为唯一真相源覆盖 ops/，幂等）',
    ].join('\n        '),
  );
}

// 防止有人再次把 token 抓取逻辑整段删掉：TOKEN_SELECTORS 是 challenge 绕过
// 的关键数据，删了不会报错，只会静默失效。
const challengePath = join(ASSETS, 'page_challenge.js');
if (!existsSync(challengePath)) {
  check('page_challenge.js 含 TOKEN_SELECTORS', false, '文件不存在');
} else {
  const source = readFileSync(challengePath, 'utf8');
  check('page_challenge.js 含 TOKEN_SELECTORS 定义', /const TOKEN_SELECTORS\s*=\s*\[/.test(source));
  check('page_challenge.js 使用 TOKEN_SELECTORS', /TOKEN_SELECTORS\.(forEach|map|filter|some|length)|for\s*\([^)]*TOKEN_SELECTORS/.test(source));
  const opsChallenge = join(OPS, 'page_challenge.js');
  if (existsSync(opsChallenge)) {
    check(
      'ops/page_challenge.js 含 TOKEN_SELECTORS',
      /TOKEN_SELECTORS/.test(readFileSync(opsChallenge, 'utf8')),
    );
  }
}

const failed = results.filter((item) => !item.ok);
console.log(`\n${results.length - failed.length}/${results.length} passed`);
if (failed.length) {
  console.log('failed:');
  for (const item of failed) console.log('  - ' + item.name);
  process.exit(1);
}
process.exit(0);
