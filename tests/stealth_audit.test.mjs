// page_stealth.js 的行为契约：audit 的判定/归责、scrub 的清理范围与顺序。
//
// 这里不读源码猜逻辑，而是把 assets/page_stealth.js 放进 vm 执行，再用一个
// 「形状像浏览器」的 sandbox 调 __abcStealth，观察真实回包。
//
// sandbox 的几条关键形状（缺一样就会误判成 headless）：
//   - window 与 globalThis 是同一个对象（脚本里 window / 裸全局两种写法都有），
//     所以 sandbox.window = sandbox 之后就不再单独注入 globalThis；
//   - document.createElement('canvas').getContext('webgl') 要能一路给出
//     WEBGL_debug_renderer_info + UNMASKED_RENDERER_WEBGL，否则 webgl_renderer
//     为空字符串，会被 software_webgl 的判定吃掉（空串不命中正则，但拿不到显卡名
//     就失去了「干净环境 100 分」的意义）；
//   - fetch / XMLHttpRequest.prototype.open 的「原生」版本用 bind 出来的函数：
//     Function.prototype.toString.call(boundFn) 恒为 "function () { [native code] }"，
//     和真实浏览器一致，且不需要替换 Function.prototype；
//   - setTimeout 只记录回调不执行（脚本里 eraseSelfSoon / permissionMismatch 用它兜底）；
//   - location / document.title / screen / Intl 都是裸全局访问，必须给值。
//
// 断言注意：结果对象来自 vm 的另一个 realm，数组的原型不同，assert.deepEqual
// 在 strict 模式下会因 [[Prototype]] 不一致而失败，所以数组一律比 length / join。
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const HERE = dirname(fileURLToPath(import.meta.url));
const SOURCE = readFileSync(join(HERE, '..', 'assets', 'page_stealth.js'), 'utf8');

const CLEAN_UA =
  'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36';
const HEADLESS_UA =
  'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/126.0.0.0 Safari/537.36';
const NVIDIA_RENDERER = 'ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0)';

/// 造一个「原生」函数：bind 之后没有源码文本，toString 只会给出 [native code]。
function nativeFn(name) {
  const fn = function () {};
  Object.defineProperty(fn, 'name', { value: name, configurable: true });
  return fn.bind(null);
}

/// page_net.js 的 state 真实形状（assets/page_net.js:107-110）：
/// { installed, seq, log:[], routes:[] }。page_stealth 只认这个结构，
/// 名字对但值是随便一个对象不算——否则页面塞个同名属性就会被误判成我们的槽位。
function netState(overrides) {
  return Object.assign({ installed: true, seq: 0, log: [], routes: [] }, overrides || {});
}

function makeEnv(overrides = {}) {
  const cfg = Object.assign(
    {
      ua: CLEAN_UA,
      platform: 'Win32',
      webdriver: false,
      plugins: 3,
      mimeTypes: 2,
      languages: ['zh-CN', 'en'],
      chrome: true,
      chromeRuntime: false,
      renderer: NVIDIA_RENDERER,
      outer: [1920, 1080],
      screen: [1920, 1080],
      timezone: 'Asia/Shanghai',
      hardwareConcurrency: 8,
      notification: { permission: 'granted', query: 'granted' },
      domIds: [],
      fetch: null,
      xhrOpen: null,
      globals: null,
      href: 'https://example.test/page',
      title: 'Example Page'
    },
    overrides
  );

  const timers = [];
  const nodes = new Map();
  const present = new Set(cfg.domIds.map(String));

  const document = {
    title: cfg.title,
    readyState: 'complete',
    scripts: [],
    body: { textContent: 'body text' },
    createElement(tag) {
      if (String(tag).toLowerCase() !== 'canvas') {
        return { tagName: String(tag).toUpperCase(), style: {}, remove() {} };
      }
      return {
        tagName: 'CANVAS',
        getContext(kind) {
          if (kind !== 'webgl' && kind !== 'experimental-webgl') return null;
          return {
            getExtension(name) {
              if (name !== 'WEBGL_debug_renderer_info') return null;
              return { UNMASKED_RENDERER_WEBGL: 0x9246, UNMASKED_VENDOR_WEBGL: 0x9245 };
            },
            getParameter(param) {
              return param === 0x9246 ? cfg.renderer : '';
            }
          };
        }
      };
    },
    getElementById(id) {
      const key = String(id);
      if (!present.has(key)) return null;
      if (!nodes.has(key)) {
        nodes.set(key, {
          id: key,
          remove() {
            present.delete(key);
            nodes.delete(key);
          }
        });
      }
      return nodes.get(key);
    },
    querySelector(selector) {
      return /^#[^ >]+$/.test(selector) ? this.getElementById(selector.slice(1)) : null;
    },
    querySelectorAll() {
      return [];
    }
  };

  const navigator = {
    userAgent: cfg.ua,
    platform: cfg.platform,
    languages: cfg.languages,
    webdriver: cfg.webdriver,
    plugins: { length: cfg.plugins },
    mimeTypes: { length: cfg.mimeTypes },
    hardwareConcurrency: cfg.hardwareConcurrency,
    permissions: {
      // 同步 thenable：不引入跨 realm 的 Promise，也就不会有悬而未决的微任务。
      query() {
        return {
          then(onFulfilled) {
            onFulfilled({ state: cfg.notification ? cfg.notification.query : 'prompt' });
          }
        };
      }
    }
  };

  const sandbox = {};
  sandbox.window = sandbox; // 让 window 与 globalThis 是同一个对象
  sandbox.location = { href: cfg.href };
  sandbox.document = document;
  sandbox.navigator = navigator;
  sandbox.screen = { width: cfg.screen[0], height: cfg.screen[1] };
  sandbox.outerWidth = cfg.outer[0];
  sandbox.outerHeight = cfg.outer[1];
  sandbox.devicePixelRatio = 1;
  sandbox.Intl = {
    DateTimeFormat() {
      return { resolvedOptions: () => ({ timeZone: cfg.timezone }) };
    }
  };
  sandbox.setTimeout = (fn) => {
    timers.push(fn);
    return timers.length;
  };
  sandbox.clearTimeout = () => {};
  sandbox.fetch = cfg.fetch || nativeFn('fetch');
  sandbox.XMLHttpRequest = function XMLHttpRequest() {};
  sandbox.XMLHttpRequest.prototype.open = cfg.xhrOpen || nativeFn('open');
  if (cfg.chrome) sandbox.chrome = cfg.chromeRuntime ? { runtime: { id: 'abcdef' } } : { loadTimes() {}, csi() {} };
  if (cfg.notification) sandbox.Notification = { permission: cfg.notification.permission };
  if (cfg.globals) Object.assign(sandbox, cfg.globals);

  const load = () => vm.runInNewContext(SOURCE + '\n;__abcStealth', sandbox, { filename: 'page_stealth.js' });

  return { sandbox, document, timers, load };
}

function ids(result) {
  return result.findings.map((item) => item.id);
}

function scopesOf(result, list) {
  const byId = new Map(result.findings.map((item) => [item.id, item]));
  return list.map((id) => (byId.has(id) ? byId.get(id).scope : `missing:${id}`));
}

test('干净环境：score 100 / level clean / 零发现', async () => {
  const env = makeEnv();
  const stealth = env.load();

  const res = await stealth({ action: 'audit' });

  assert.equal(res.ok, true);
  assert.equal(res.action, 'audit');
  assert.equal(res.findings.length, 0, `意外发现：${JSON.stringify(res.findings)}`);
  assert.equal(res.score, 100);
  assert.equal(res.level, 'clean');
  assert.equal(res.self_fixable, 0);
  assert.match(res.headline, /未发现/);
  assert.equal(res.url, 'https://example.test/page');
  assert.equal(res.title, 'Example Page');

  // env 快照必须真的读到了 stub 的值，否则「100 分」可能只是所有检查都没跑起来。
  assert.equal(res.env.webdriver, false);
  assert.equal(res.env.plugins, 3);
  assert.equal(res.env.mime_types, 2);
  assert.equal(res.env.languages.join(','), 'zh-CN,en');
  assert.equal(res.env.has_chrome_object, true);
  assert.equal(res.env.chrome_runtime_in_page, false);
  assert.equal(res.env.webgl_renderer, NVIDIA_RENDERER);
  assert.equal(res.env.outer_size.join(','), '1920,1080');
  assert.equal(res.env.screen_size.join(','), '1920,1080');
  assert.equal(res.env.timezone, 'Asia/Shanghai');
  assert.equal(res.env.hardware_concurrency, 8);
});

test('入口隐藏：__abcStealth 不可枚举、可删除，删了也不影响已持有的引用', async () => {
  const env = makeEnv();
  const stealth = env.load();
  const win = env.sandbox;

  assert.equal(typeof stealth, 'function');

  // 站点最常见的两种枚举手法都看不到它。
  assert.equal(Object.keys(win).includes('__abcStealth'), false, 'Object.keys 泄露了入口名');
  const enumerated = [];
  for (const key in win) enumerated.push(key);
  assert.equal(enumerated.includes('__abcStealth'), false, 'for...in 泄露了入口名');

  // 但它必须在 window 上（daemon 侧是裸标识符 `return __abcStealth(...)`，靠全局属性解析）。
  assert.equal(Object.getOwnPropertyNames(win).includes('__abcStealth'), true);
  const desc = Object.getOwnPropertyDescriptor(win, '__abcStealth');
  assert.equal(desc.enumerable, false);
  assert.equal(desc.configurable, true);
  assert.equal(desc.writable, true);

  // 可配置才是关键：顶层函数声明那条路径 delete 恒 false，审计函数自己就成了永久指纹。
  assert.equal(delete win.__abcStealth, true);
  assert.equal(Object.prototype.hasOwnProperty.call(win, '__abcStealth'), false);
  assert.equal('__abcStealth' in win, false);

  // 删掉的是全局入口，已经拿到引用的调用方不受影响。
  const res = await stealth({ action: 'audit' });
  assert.equal(res.ok, true);
  assert.equal(res.action, 'audit');
});

test('eraseSelfSoon：定时器到期后入口真的从 window 消失', async () => {
  // 这条锁的是**行为**而不是形状，别当冗余删掉：用例 2 只断言描述符（configurable
  // 为 true），而顶层 function 声明那条路径上描述符看着也对、delete 却被 的
  // DONT_DELETE 挡掉。把入口的 configurable 改回 false 时，这条会红在
  // 「eraseSelfSoon 没真的删掉入口」，用例 2 只能红在描述符断言上——两者缺一不可。
  const env = makeEnv();
  const stealth = env.load();
  const win = env.sandbox;
  const has = () => Object.prototype.hasOwnProperty.call(win, '__abcStealth');

  const res = await stealth({ action: 'audit' });
  assert.equal(res.ok, true);
  // 删除是异步的（setTimeout 0），调用返回时还应该在。
  assert.equal(has(), true);

  // 跑一次挂起的定时器，等价于 setTimeout(0) 到期。
  env.timers.splice(0).forEach((fn) => fn());
  assert.equal(has(), false, 'eraseSelfSoon 没真的删掉入口');

  // 同页第二次调用必须由调用方重新注入；已经拿到引用的调用方不受影响。
  const again = await stealth({ action: 'audit' });
  assert.equal(again.ok, true);
});

test('本工具自身痕迹：own_globals / own_dom_badge / own_net_hook 全部归 self', async () => {
  const env = makeEnv({
    domIds: ['atb-ind'],
    fetch: function fetch() {}, // 已被包装，toString 里没有 [native code]
    globals: {
      __abcRead() {},
      __abcAct() {},
      __abcNetState: netState()
    }
  });

  const res = await env.load()({ action: 'audit' });
  const got = ids(res);
  const own = ['own_globals', 'own_dom_badge', 'own_net_hook'];

  for (const id of own) assert.ok(got.includes(id), `缺少 ${id}，实际有 ${got.join(',')}`);
  assert.equal(scopesOf(res, own).join(','), 'self,self,self');

  const byId = new Map(res.findings.map((item) => [item.id, item]));
  assert.match(byId.get('own_globals').detail, /__abcRead/);
  assert.match(byId.get('own_globals').detail, /__abcAct/);
  assert.match(byId.get('own_dom_badge').detail, /#atb-ind/);

  assert.ok(res.self_fixable >= 3, `self_fixable=${res.self_fixable}`);
  assert.ok(res.score < 100, `score=${res.score}`);
  // 有本工具的网络钩子时，不该再报成用户脚本/未知来源。
  assert.equal(got.includes('patched_network'), false);
  assert.match(res.headline, /本工具自身/);
});

test('headless 特征：六项浏览器痕迹全部归 browser', async () => {
  const env = makeEnv({
    webdriver: true,
    ua: HEADLESS_UA,
    plugins: 0,
    mimeTypes: 0,
    chrome: false,
    renderer: 'Google SwiftShader',
    outer: [0, 0],
    notification: null
  });

  const res = await env.load()({ action: 'audit' });
  const got = ids(res);
  const expected = [
    'navigator_webdriver',
    'ua_headless',
    'no_plugins',
    'no_chrome_object',
    'software_webgl',
    'zero_outer_size'
  ];

  for (const id of expected) assert.ok(got.includes(id), `缺少 ${id}，实际有 ${got.join(',')}`);
  assert.equal(scopesOf(res, expected).join(','), expected.map(() => 'browser').join(','));
  assert.equal(res.env.webgl_renderer, 'Google SwiftShader');
  assert.equal(res.self_fixable, 0);
  assert.equal(res.level, 'high');
});

test('用户脚本改动不算本工具：patched_network 归 unknown', async () => {
  const env = makeEnv({
    fetch: function fetch() {}, // 无 __abcNetState，说明不是本工具的网络钩子
    globals: {}
  });

  const res = await env.load()({ action: 'audit' });
  const got = ids(res);
  assert.ok(got.includes('patched_network'), `实际有 ${got.join(',')}`);

  const hit = res.findings.find((item) => item.id === 'patched_network');
  assert.equal(hit.scope, 'unknown');
  assert.notEqual(hit.scope, 'self');
  assert.match(hit.detail, /fetch native=false/);
  assert.equal(got.some((id) => id.startsWith('own_')), false);
  assert.equal(res.self_fixable, 0);
});

test('scrub：先还原 fetch/xhr，再删全局与角标，最后无残留', () => {
  const env = makeEnv({
    domIds: ['atb-ind'],
    fetch: function fetch() {}, // scrub 前是被包装状态
    globals: {
      __abcRead() {},
      __abcNetState: netState()
    }
  });
  const win = env.sandbox;
  let resetCalls = 0;
  win.__abcNet = function (opts) {
    resetCalls += 1;
    assert.equal(opts && opts.action, 'reset');
    win.fetch = nativeFn('fetch'); // reset 会把被包装的函数换回原生
    return { ok: true, action: 'reset' };
  };

  const res = env.load()({ action: 'scrub' });

  assert.equal(res.ok, true);
  assert.equal(res.action, 'scrub');
  assert.equal(resetCalls, 1);
  assert.equal(res.restored.join(','), 'fetch/xhr');
  assert.ok(res.removed.includes('__abcRead'), `removed=${res.removed.join(',')}`);
  assert.ok(res.removed.includes('#atb-ind'), `removed=${res.removed.join(',')}`);
  assert.equal(res.remaining.length, 0, `remaining=${res.remaining.join(',')}`);

  // 回包说干净了，实际状态也得干净。
  assert.match(Function.prototype.toString.call(win.fetch), /\[native code\]/);
  assert.equal(typeof win.__abcRead, 'undefined');
  assert.equal(typeof win.__abcNet, 'undefined');
  assert.equal(env.document.getElementById('atb-ind'), null);
  // 状态槽不在 OWN_GLOBALS 里，必须由 scrub 在 reset 之后单独摘掉；
  // 否则 remaining 是空数组、页面里却还留着钩子的状态对象（假干净）。
  assert.equal(typeof win.__abcNetState, 'undefined');
});

test('scrub：状态槽删不掉时必须报 ok:false，而不是假干净', () => {
  const env = makeEnv({ fetch: function fetch() {}, globals: { __abcRead() {} } });
  const win = env.sandbox;

  // 把槽位钉成不可配置：delete 只会静默失败（脚本是 sloppy mode，不抛错）。
  // 值必须是真实 state 结构，否则读取侧的值结构校验会先判它不是我们的槽位。
  Object.defineProperty(win, '__abcNetState', {
    value: netState({ stuck: true }),
    writable: false,
    enumerable: false,
    configurable: false
  });
  // reset 故意不删槽位，模拟旧版 __abcNet / 页面上别的实现把状态留在原处。
  win.__abcNet = function () {
    return { ok: true, action: 'reset' };
  };

  const res = env.load()({ action: 'scrub' });

  assert.equal(res.ok, false);
  assert.ok(res.remaining.includes('__abcNetState'), `remaining=${res.remaining.join(',')}`);
  assert.match(res.note, /仍有痕迹/);
  // 其余清理照常完成：函数还原了，普通全局该删的删了。
  assert.equal(res.restored.join(','), 'fetch/xhr');
  assert.ok(res.removed.includes('__abcRead'), `removed=${res.removed.join(',')}`);

  // 证明这条 stub 确实删不掉，否则上面的 ok:false 可能是别的原因。
  assert.equal(Reflect.deleteProperty(win, '__abcNetState'), false);
  assert.equal(Object.getOwnPropertyDescriptor(win, '__abcNetState').configurable, false);
});

test('scrub 顺序：reset 被调用时 __abcNet 仍然存在（先还原再删全局）', () => {
  const order = [];
  const env = makeEnv({ globals: { __abcNetState: netState() } });
  const win = env.sandbox;
  win.__abcNet = function () {
    order.push('reset');
    order.push(typeof win.__abcNet === 'function' ? 'net-alive' : 'net-gone');
    order.push(typeof win.__abcNetState === 'undefined' ? 'state-gone' : 'state-alive');
    return { ok: true };
  };

  const res = env.load()({ action: 'scrub' });

  assert.equal(order.slice(0, 3).join(','), 'reset,net-alive,state-alive');
  assert.equal(res.restored.join(','), 'fetch/xhr');
  // reset 之后才轮到删除：此刻 __abcNet 已经没了。
  assert.equal(typeof win.__abcNet, 'undefined');
  assert.equal(res.remaining.length, 0);
});

test('新版状态槽（__atb_net_ 前缀）同样算本工具痕迹', async () => {
  // page_net.js 5.0 起不再写 window.__abcNetState，改成
  // Object.defineProperty 挂的隐藏槽 window['__atb_net_<random>']：
  // enumerable:false / writable:false / configurable:true。
  // 只认老名字的话，升级后审计会得出「页面干净」的假结论，所以这里按真实属性
  // 描述构造槽位——用 Object.keys 扫的实现会漏，必须用 getOwnPropertyNames。
  const env = makeEnv({ fetch: function fetch() {} });
  Object.defineProperty(env.sandbox, '__atb_net_9f3a1c7d', {
    value: netState({ xhrGuarded: true }),
    writable: false,
    enumerable: false,
    configurable: true
  });

  const res = await env.load()({ action: 'audit' });
  const got = ids(res);

  assert.ok(got.includes('own_net_hook'), `实际有 ${got.join(',')}`);
  assert.equal(res.findings.find((item) => item.id === 'own_net_hook').scope, 'self');
  assert.match(res.findings.find((item) => item.id === 'own_net_hook').detail, /__atb_net_9f3a1c7d/);
  assert.equal(got.includes('patched_network'), false);
  assert.equal(res.self_fixable, 1);
});

test('假槽位不算本工具痕迹，scrub 也不碰别人的属性', async () => {
  // 判据是「名字形状 + 不可枚举 + 值结构」三检，四个假槽位各只破一条，
  // 这样任何一条判据被删掉都会有用例变红（只放一个「什么都错」的 fake 是锁不住的：
  // 它会被另外两条先挡掉，看着绿其实没覆盖）。
  //   __atb_net_fake       —— 普通赋值：可枚举 + 名字 4 位 + 值不是 state（真实场景：页面随手写的）
  //   __atb_net_abcd1234   —— 只破「不可枚举」：名字 8 位、值是完整 state
  //   __atb_net_abcdefgh   —— 只破「值结构」：名字 8 位、不可枚举，但值是 {}
  //   __atb_net_fake2      —— 只破「名字形状」：不可枚举、值是完整 state，但名字 5 位
  // 只按前缀匹配的实现（旧版）会把它们当成本工具的槽位：audit 误报 own_net_hook，
  // scrub 更糟——走 else-if 分支 delete 掉页面的属性还报 restored: ['state-only']。
  const env = makeEnv({ globals: { '__atb_net_fake': { xhrGuarded: true } } });
  const win = env.sandbox;
  // 只破「不可枚举」：其余全对。
  Object.defineProperty(win, '__atb_net_abcd1234', {
    value: netState(),
    writable: false,
    enumerable: true,
    configurable: true
  });
  // 只破「值结构」：名字形状对、不可枚举，但值不是 page_net 的 state。
  Object.defineProperty(win, '__atb_net_abcdefgh', {
    value: {},
    writable: false,
    enumerable: false,
    configurable: true
  });
  // 只破「名字形状」：不可枚举、值是对的 state，但名字 5 位。
  Object.defineProperty(win, '__atb_net_fake2', {
    value: netState(),
    writable: false,
    enumerable: false,
    configurable: true
  });
  const fakes = ['__atb_net_fake', '__atb_net_abcd1234', '__atb_net_abcdefgh', '__atb_net_fake2'];

  const audit = await env.load()({ action: 'audit' });
  assert.equal(audit.findings.length, 0, `误报：${ids(audit).join(',')}`);
  assert.equal(ids(audit).includes('own_net_hook'), false);

  const scrub = env.load()({ action: 'scrub' });
  assert.equal(scrub.restored.includes('state-only'), false, `restored=${scrub.restored.join(',')}`);
  assert.equal(scrub.remaining.length, 0);
  assert.equal(scrub.ok, true);
  // 别人的属性原地不动。
  for (const name of fakes) {
    assert.equal(Object.prototype.hasOwnProperty.call(win, name), true, `scrub 删了 ${name}`);
  }
});

test('伪造的旧名 __abcNetState 也不算本工具痕迹，scrub 不删它', async () => {
  // 旧名分支现在同样要过 looksLikeNetState。页面随手写个 __abcNetState = { hooked: true }
  // （或者干脆 undefined）不能被当成本工具的槽位再 delete 掉——那是毁页面数据。
  // 说明：故意不覆盖「名字形状、不可枚举、值结构三者全对」的伪造槽位——那种情况
  // 从页面侧已经无法和真槽位区分，删掉是既定取舍，写出来会和判据打架。
  const cases = [
    { label: '值形状不对', value: { hooked: true } },
    { label: '值是 undefined', value: undefined },
    { label: '值是字符串', value: 'hooked' },
    { label: '值是 null', value: null }
  ];

  for (const item of cases) {
    const env = makeEnv();
    const win = env.sandbox;
    win.__abcNetState = item.value; // 普通赋值：own property、可枚举、可删

    const audit = await env.load()({ action: 'audit' });
    assert.equal(audit.findings.length, 0, `${item.label}：误报 ${ids(audit).join(',')}`);

    const scrub = env.load()({ action: 'scrub' });
    assert.equal(scrub.restored.includes('state-only'), false, `${item.label}：restored=${scrub.restored.join(',')}`);
    assert.equal(scrub.ok, true, `${item.label}：remaining=${scrub.remaining.join(',')}`);
    assert.equal(
      Object.prototype.hasOwnProperty.call(win, '__abcNetState'),
      true,
      `${item.label}：scrub 把页面的属性删了`
    );
  }
});

test('getter 状态槽：audit 全程不得触发页面的 getter', async () => {
  // 页面可以把 __abcNetState 定义成访问器：`window.__abcNetState` 这种直读会执行它的
  // getter（在我们的注入上下文里跑页面代码），而且 getter 返回值合法时我们还会把
  // 它当成本工具的槽位。正确读法是只看描述符的 value 槽位，访问器一律当「不存在」。
  const env = makeEnv();
  let hits = 0;
  Object.defineProperty(env.sandbox, '__abcNetState', {
    enumerable: false,
    configurable: true,
    get() {
      hits += 1;
      return netState({ seq: 1 }); // 形状合法：一旦被读就会被认成我们的槽位
    }
  });

  const res = await env.load()({ action: 'audit' });

  assert.equal(hits, 0, 'audit 触发了页面的 __abcNetState getter');
  assert.equal(ids(res).includes('own_net_hook'), false);
  assert.equal(res.findings.length, 0, `误报：${ids(res).join(',')}`);
  assert.equal(res.score, 100);
});

test('getter 入口 __abcNet：scrub 全程不得触发页面的 getter', () => {
  // `typeof window.__abcNet === 'function'` 同样会读值、同样会踩 getter。
  // 这里只锁「getter 没被触发」这个行为，不断言删除结果——可配置的访问器删得掉、
  // 不可配置的删不掉，那是另一个维度的事。
  const env = makeEnv();
  const win = env.sandbox;
  let hits = 0;
  Object.defineProperty(win, '__abcNetState', {
    value: netState({ seq: 1 }),
    writable: false,
    enumerable: false,
    configurable: true
  });
  Object.defineProperty(win, '__abcNet', {
    enumerable: false,
    configurable: true,
    get() {
      hits += 1;
      return function () {
        return { ok: true, action: 'reset' };
      };
    }
  });

  const res = env.load()({ action: 'scrub' });

  assert.equal(hits, 0, 'scrub 触发了页面的 __abcNet getter');
  // 拿不到入口 = 不可能走 reset，只可能走「只摘状态槽」那条分支。
  assert.equal(res.restored.includes('fetch/xhr'), false, `restored=${res.restored.join(',')}`);
  assert.equal(res.restored.join(','), 'state-only');
  assert.equal(res.ok, true);
});

test('未知 action：同步返回 ok:false', () => {
  const env = makeEnv();
  const stealth = env.load();

  const res = stealth({ action: 'nope' });

  assert.equal(typeof res.then, 'undefined'); // 不是 Promise，是同步错误
  assert.equal(res.ok, false);
  assert.equal(typeof res.error, 'string');
  assert.match(res.error, /未知 action/);
  assert.match(res.error, /nope/);
});
