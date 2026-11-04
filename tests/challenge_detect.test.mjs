// page_challenge.js 的判定契约：特别是「组件在 closed shadow root 里」这种形态。
//
// 背景（真机现象，2026-11-03）：CF 的托管挑战会把 Turnstile 的 iframe 放进页面的
// closed shadow root。页面 JS 的 querySelectorAll('iframe') 恒为 0，
// #challenge-stage 也不存在，但组件其实已经渲染好、只需要点一下。
// 旧实现把这种页面判成「站点没下发组件」（stalled: turnstile_not_rendered）并放弃，
// 于是永远过不去。这里用 vm 造一个同样形状的页面锁死判定。
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const HERE = dirname(fileURLToPath(import.meta.url));
const SOURCE = readFileSync(join(HERE, '..', 'assets', 'page_challenge.js'), 'utf8');

/// 造一个够用的「页面」：只实现 detect() 真正用到的那几个 API。
///
/// `present` 是「页面上存在的选择器」集合。querySelector / querySelectorAll 都按它
/// 判断命中，避免为了一个判定去实现整套 CSS 选择器。
function makePage(options = {}) {
  const cfg = Object.assign(
    {
      title: '请稍候…',
      href: 'https://example.com/files',
      readyState: 'complete',
      present: [],
      scripts: [],
      iframeSrc: [],
      bodyText: '',
      turnstile: true,
      chlOpt: null,
      visibility: 'visible',
      outer: [1280, 800],
      inner: [1264, 720],
      stalledFor: 0,
    },
    options
  );

  const present = new Set(cfg.present);
  const stubNode = () => ({
    id: '',
    value: '',
    getAttribute: () => 'cf-turnstile-response',
  });
  const nodes = (selector) => {
    if (selector === 'iframe') return cfg.iframeSrc.map((src) => ({ src: src }));
    if (selector === 'input[type="hidden"]') return [];
    // present 里放的是「特征片段」，选择器里包含它就算命中：
    // 组合选择器（`[name="cf-turnstile-response"], [id*="cf-chl-widget"]...`）
    // 也能被正确判出来，不用为测试去实现整套 CSS 匹配。
    const hit = [...present].some((fragment) => selector.includes(fragment));
    return hit ? [stubNode()] : [];
  };
  const document = {
    title: cfg.title,
    readyState: cfg.readyState,
    visibilityState: cfg.visibility,
    hidden: cfg.visibility === 'hidden',
    hasFocus: () => true,
    scripts: cfg.scripts.map((src) => ({ src: src })),
    body: { textContent: cfg.bodyText },
    documentElement: {},
    querySelector(selector) {
      return nodes(selector)[0] || null;
    },
    querySelectorAll(selector) {
      return nodes(selector);
    },
  };
  const window = {
    location: { href: cfg.href },
    innerWidth: cfg.inner[0],
    innerHeight: cfg.inner[1],
    outerWidth: cfg.outer[0],
    outerHeight: cfg.outer[1],
    performance: { now: () => Date.now() },
    setTimeout,
    clearTimeout,
    turnstile: cfg.turnstile ? { render() {}, getResponse() {} } : undefined,
    _cf_chl_opt: cfg.chlOpt || undefined,
    MutationObserver: class {
      observe() {}
      disconnect() {}
    },
  };
  window.window = window;
  window.document = document;
  window.location = window.location;
  return { window, document };
}

function run(options) {
  const { window } = makePage(options);
  const sandbox = Object.assign({}, window, {
    document: window.document,
    location: window.location,
    performance: window.performance,
    MutationObserver: window.MutationObserver,
    NodeFilter: { SHOW_TEXT: 4 },
    console,
    // window 与 globalThis 必须是同一个对象：脚本里两种写法都有。
    globalThis: window,
  });
  sandbox.window = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(SOURCE, sandbox);
  // stalledMs 现在由 daemon 传进来（脚本不再往 window 写时间戳，那是可被站点读到的指纹）
  const stalledFor = (options && options.stalledFor) || 0;
  const opts = stalledFor > 0 ? `{ action: "status", stalledMs: ${stalledFor} }` : '{ action: "status" }';
  const result = vm.runInContext(`__abcChallenge(${opts})`, sandbox);
  return JSON.parse(JSON.stringify(result));
}

const CF_SCRIPTS = [
  'https://example.com/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1?ray=abc',
  'https://challenges.cloudflare.com/turnstile/v0/b/xxx/api.js?render=explicit',
];

test('closed shadow root 里的 Turnstile：判为可交互，且不判 stalled', () => {
  const out = run({
    title: '请稍候…',
    scripts: CF_SCRIPTS,
    present: ['[name="cf-turnstile-response"]'],
    // 关键形状：页面 JS 一个 iframe 都看不到、也没有 #challenge-stage
    iframeSrc: [],
    stalledFor: 12000,
  });
  assert.equal(out.detected, true);
  assert.equal(out.turnstile.api, true);
  assert.equal(out.turnstile.iframes, 0);
  assert.equal(out.turnstile.shadow_widget, true);
  // 有组件就必须点：interactive=true 才会走到点击求解
  assert.equal(out.interactive, true);
  assert.equal(out.stalled, false);
  assert.ok(out.hints.some((hint) => hint.includes('closed shadow root')));
});

test('确实没下发组件：仍然报 stalled，交给上层换环境', () => {
  const out = run({
    title: '请稍候…',
    scripts: CF_SCRIPTS,
    present: [],
    iframeSrc: [],
    stalledFor: 12000,
  });
  assert.equal(out.detected, true);
  assert.equal(out.turnstile.shadow_widget, false);
  assert.equal(out.interactive, false);
  assert.equal(out.stalled, true);
});

test('可见的 turnstile 组件（普通站点）不受影响', () => {
  const out = run({
    title: '登录',
    href: 'https://example.com/login',
    scripts: ['https://challenges.cloudflare.com/turnstile/v0/api.js'],
    present: ['.cf-turnstile'],
  });
  assert.equal(out.detected, true);
  assert.equal(out.kind, 'widget');
  assert.equal(out.blocking, false);
  assert.equal(out.interactive, true);
});

test('普通页面不误判', () => {
  const out = run({
    title: 'Example Domain',
    href: 'https://example.com/',
    scripts: ['https://example.com/app.js'],
    present: [],
  });
  assert.equal(out.detected, false);
  assert.equal(out.kind, 'unknown');
  assert.equal(out.stalled, false);
  assert.ok(out.hints.every((hint) => !hint.includes('closed shadow root')));
});
