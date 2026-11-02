// page_net.js 的返回字段契约（的上游）。
//
// daemon 侧持久化规则时要读「真实数组」，而页面脚本在不同 action 下把数组放在
// 不同字段里：
//   route   / unroute → all:<array>          （没有 routes_list）
//   hook    / routes  → routes_list:<array>  （hook 没有 all）
// 且 hook 的 routes 历史上是**计数**（数字），保留它只是为了不打断旧调用方。
//
// 这几条一旦漂移，daemon 要么存进一个数字、要么永远读到空数组——就是
// 「把 hook 的 routes 计数当数组存下去」造成的规则丢失。这里不靠读源码猜，直接
// 把 page_net.js 放进 vm 执行，调用 __abcNet 观察真实回包。
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

// page_net 会包装 fetch / XHR，这里给最小可用的形状即可（不需要真实网络）。
class FakeXhr {
  open() {}
  send() {}
  setRequestHeader() {}
  addEventListener() {}
  removeEventListener() {}
  abort() {}
}

function loadPageNet() {
  const context = vm.createContext({
    window: {},
    location: { href: 'https://example.com/' },
    document: { querySelector: () => null, addEventListener() {} },
    console: { log() {}, warn() {}, error() {} },
    fetch: async () => ({ clone: () => ({}) }),
    XMLHttpRequest: FakeXhr,
    Headers: class {},
    Request: class {},
    Response: class {},
    performance: { now: () => 0 },
    setTimeout,
    clearTimeout,
    Promise,
    JSON,
    Math,
    Date,
    URL,
  });
  vm.runInContext(readFileSync(PAGE_NET, 'utf8'), context, { filename: PAGE_NET });
  const fn = context.__abcNet || context.window.__abcNet;
  if (typeof fn !== 'function') throw new Error('page_net.js 未暴露 __abcNet');
  return fn;
}

const net = loadPageNet();
const rule = { match: '/api/contract/*', status: 201, body: '{"ok":1}' };

// ---- route：数组放在 all，且没有 routes_list ----
const route = net({ action: 'route', rule });
check('route 返回 ok 且回显规则', route.ok === true && route.rule?.match === rule.match, JSON.stringify(route));
check('route 的规则数组在 all 字段', Array.isArray(route.all) && route.all.length === 1, JSON.stringify(route.all));
check('route 不返回 routes_list（那是 hook/routes 的字段）', !('routes_list' in route), Object.keys(route).join(','));

// ---- hook：真实数组在 routes_list，routes 是计数（数字），且没有 all ----
const hook = net({ action: 'hook', routes: [rule] });
check('hook 返回 ok 且 installed', hook.ok === true && hook.installed === true, JSON.stringify(hook));
check(
  'hook 的 routes 是计数（number），不是数组',
  typeof hook.routes === 'number',
  JSON.stringify(hook.routes),
);
check('hook 提供 routes_list 数组', Array.isArray(hook.routes_list), JSON.stringify(hook.routes_list));
check(
  'hook 的 routes_list 与传入规则一致（daemon 持久化只能读这个字段）',
  hook.routes_list.length === 1 && hook.routes_list[0].match === rule.match,
  JSON.stringify(hook.routes_list),
);
check('hook 不提供 all（all 是 route/unroute 专用）', !('all' in hook), Object.keys(hook).join(','));
check(
  'hook 的 count 与 routes 一致',
  hook.count === hook.routes,
  JSON.stringify({ routes: hook.routes, count: hook.count }),
);

// ---- routes：两种字段都给，兼容新旧调用方 ----
const routes = net({ action: 'routes' });
check(
  'routes 同时提供 routes 数组与 routes_list 数组',
  Array.isArray(routes.routes) && Array.isArray(routes.routes_list),
  JSON.stringify(routes),
);
check('routes 的 count 等于数组长度', routes.count === routes.routes.length, JSON.stringify(routes.count));

// ---- unroute：数组同样在 all ----
const unroute = net({ action: 'unroute', match: rule.match });
check('unroute 返回 removed 与 all 数组', typeof unroute.removed === 'number' && Array.isArray(unroute.all), JSON.stringify(unroute));
check('unroute 之后 rules 为空', net({ action: 'routes' }).count === 0, JSON.stringify(net({ action: 'routes' })));

const failed = results.filter((item) => !item.ok);
console.log(`\n${results.length - failed.length}/${results.length} passed`);
if (failed.length) {
  console.log('failed:');
  for (const item of failed) console.log('  - ' + item.name);
  process.exit(1);
}
process.exit(0);
