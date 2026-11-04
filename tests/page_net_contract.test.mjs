// page_net.js 的返回字段契约（上游）。
//
// daemon 侧持久化规则时要读「真实数组」，而页面脚本在不同 action 下把数组放在
// 不同字段里：
//   route   / unroute → all:<array>          （没有 routes_list）
//   hook    / routes  → routes_list:<array>  （hook 没有 all）
// 且 hook 的 routes 历史上是**计数**（数字），保留它只是为了不打断旧调用方。
//
// 这几条一旦漂移，daemon 要么存进一个数字、要么永远读到空数组—— 就是
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

function createContext(beforeRun) {
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
  // beforeRun 用来在加载脚本之前布置页面现场（例如放一个 getter 型假槽位）。
  if (beforeRun) beforeRun(context);
  vm.runInContext(readFileSync(PAGE_NET, 'utf8'), context, { filename: PAGE_NET });
  return context;
}

function loadPageNet() {
  const context = createContext();
  const fn = context.__abcNet || context.window.__abcNet;
  if (typeof fn !== 'function') throw new Error('page_net.js 未暴露 __abcNet');
  // 入口挂的是 context.window 这个普通对象，不是 vm 全局；后续属性断言要用到它。
  fn.__context = context;
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

// ---- 入口属性：stealth scrub 依赖的三个描述符位 ----
//
// 站点能不能发现我们，取决于 __abcNet 是不是「可枚举」；scrub 能不能删干净，
// 取决于它是不是「可配置」；旧版本在页面上留下过不可配置绑定，那条路径靠
// writable 赋值把实现换掉。三个位缺一，反检测就是纸面上的。
const entryContext = net.__context;
const entry = entryContext.window && Object.getOwnPropertyDescriptor(entryContext.window, '__abcNet');
check('__abcNet 挂在 window 上且不可枚举', !!entry && entry.enumerable === false, JSON.stringify(entry));
check('__abcNet 可配置（scrub 的 delete 才能真的成功）', !!entry && entry.configurable === true, JSON.stringify(entry));
check('__abcNet 可写（旧版不可配置绑定时靠赋值兜底）', !!entry && entry.writable === true, JSON.stringify(entry));
check('Object.keys(window) 里看不到 __abcNet', !Object.keys(entryContext.window).includes('__abcNet'), Object.keys(entryContext.window).join(','));

// ---- 状态槽定位不能读值：getter 型槽位一次都不能被触发 ----
//
// 槽位定位是「按名字 + 描述符找」，不能退化成 window[name] 读取。否则页面放一个
// 求值即报错/计数的 getter，我们每条命令都会踩上去 —— 那本身就是可检测的行为。
let slotReads = 0;
const probeContext = createContext((context) => {
  Object.defineProperty(context.window, '__atb_net_deadbeef', {
    get() { slotReads += 1; return {}; },
    enumerable: false,
    configurable: true,
  });
});
const probeNet = probeContext.__abcNet;
if (typeof probeNet === 'function') probeNet({ action: 'routes' });
check('加载/调用时不读取状态槽的值（getter 未被触发）', slotReads === 0, 'reads=' + slotReads);

// 放最后：前面已经用过 entry，删掉不影响上面的断言。
check('delete 能真的删掉 __abcNet', delete entryContext.window.__abcNet === true);

const failed = results.filter((item) => !item.ok);
console.log(`\n${results.length - failed.length}/${results.length} passed`);
if (failed.length) {
  console.log('failed:');
  for (const item of failed) console.log('  - ' + item.name);
  process.exit(1);
}
process.exit(0);
