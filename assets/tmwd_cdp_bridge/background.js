// background.js - Cookie + CDP Bridge
// Derived from GenericAgent's tmwd_cdp_bridge / TMWebDriver (MIT, Copyright (c) 2025 lsdefine).
// See SOURCES.md and NOTICE for the upstream license.
//
// manifest.json 的 minimum_chrome_version = 120 的依据（manifest 是纯 JSON，不能写注释，
// 故把推导过程记在这里）：
//   - chrome.scripting.executeScript 的 world:'MAIN'（runUserCodeInTab / persist 注入）需要 111+；
//   - chrome.storage.session（Fetch 拦截状态的冷启动恢复）需要 102+；
//   - chrome.alarms 的 30s 最小粒度 clamp 语义按 120+ 生效（scheduleProbe / scheduleKeepalive
//     的节奏依赖这个下限）；
//   - 另外 chrome.cookies.getAll({partitionKey}) 需要 119+，已被 120 覆盖。
// 取三者（111 / 102 / 120）的最大值 = 120。
// 商店构建开关。
//
// Chrome 应用商店的 MV3 政策要求扩展逻辑自包含：不得执行运行期从外部取到的代码，
// 只有 Debugger API 与 User Scripts API 属于豁免通道。`exec` / `inject` 会把本机 CLI
// 下发的 JS 字符串交给 eval 执行，因此**商店包关闭这两个命令**，其余命令全部保留。
//
// 仓库源码与 GitHub 发行版保持 false（功能完整）；构建商店 zip 时由构建脚本改写为 true，
// 两者除这一行外完全相同。
const STORE_BUILD = false;

const LEGACY_CSP_RULE_ID = 9999;

async function removeLegacyCspRule() {
  try {
    // Dynamic rules survive extension upgrades, so explicitly migrate existing installs.
    await chrome.declarativeNetRequest.updateDynamicRules({ removeRuleIds: [LEGACY_CSP_RULE_ID] });
  } catch (e) {
    console.warn('[TMWD] Failed to remove legacy CSP rule:', e.message);
  }
}

removeLegacyCspRule();
chrome.runtime.onInstalled.addListener(() => {
  console.log('CDP Bridge installed');
  removeLegacyCspRule();
});

let lastCommandAt = 0;
const DEFAULT_WS_PORT = 18765;
const CLI_API_PORT = 18767;
let wsPort = DEFAULT_WS_PORT;
// 可选的 WebSocket 鉴权 token（对应 daemon 的 <用户配置目录>/ws-trust.json）。
// 空串 = 不启用：此时连接 URL 与历史版本一字不差，绝不改变默认行为。
let wsToken = '';
// 最后一次连接失败的原因（同时落 chrome.storage.local 的 lastConnectError，popup 读它做诊断）。
// 只在内存里做去重指纹，避免 probe 每秒往 storage 写一次。
let lastConnectError = null;
let wsConnectErrorHydrated = false;
// browserId 必须落 chrome.storage.local：MV3 的 Service Worker 随时会被回收，
// 每次冷启动都会重新求值本模块。以前它是模块级 const，重启后就变成新 UUID，
// 于是 session_key（browser_id:profile_id:tab_id）漂移，daemon 侧的 @e 引用、
// net 配置、--tab 寻址全部失联。现在与 profileId 一样从 storage 读取/生成后落盘。
let browserId = null;
const extensionVersion = chrome.runtime.getManifest().version;
let profileId = null;
let profileLabel = null;
// 单飞：并发调用只跑一次 loadClientIdentity，避免两个首次启动路径各自生成 id 后再互相覆盖。
let clientIdentityPromise = null;

async function loadClientIdentity() {
  const data = await chrome.storage.local.get({ browserId: null, profileId: null, profileLabel: null });
  const patch = {};
  browserId = data.browserId || `browser-${crypto.randomUUID()}`;
  profileId = data.profileId || `profile-${crypto.randomUUID()}`;
  profileLabel = data.profileLabel || null;
  if (!data.browserId) patch.browserId = browserId;
  if (!data.profileId) patch.profileId = profileId;
  // 一次 set 写回，避免两次写之间被 SW 回收导致只剩一半落盘。
  if (Object.keys(patch).length) await chrome.storage.local.set(patch);
  return { browserId, profileId, profileLabel };
}

/// 所有需要身份的地方都等这个：保证 browserId/profileId 一定已就绪。
function ensureClientIdentity() {
  if (!clientIdentityPromise) {
    clientIdentityPromise = loadClientIdentity().catch((e) => {
      // 读失败不要永久缓存 reject，下一次调用重试。
      clientIdentityPromise = null;
      throw e;
    });
  }
  return clientIdentityPromise;
}

function withClientIdentity(payload) {
  return Object.assign({
    browser_id: browserId,
    profile_id: profileId || 'profile-pending',
    profile_label: profileLabel || undefined
  }, payload);
}

/**
 * 注入自检：分别验证「MAIN world 纯函数」「MAIN world eval」「ISOLATED world eval」。
 *
 * exec 走的是 MAIN world eval；失败后会自动回退 CDP，而 CDP 在 Chrome 156 上
 * 可能被别的扩展 iframe 挡住，最终报的是回退失败——真正的原因被掩盖。
 * 这个命令把三条路径的真实错误分别暴露出来。
 */
async function handleInjectProbe(msg, sender) {
  let tabId;
  try {
    tabId = resolveTabId(msg, sender);
  } catch (e) {
    return { ok: false, error: e.message };
  }
  const out = { tabId };
  const probe = async (label, options) => {
    try {
      const result = await chrome.scripting.executeScript(options);
      out[label] = { ok: true, value: result?.[0]?.result ?? null };
    } catch (e) {
      out[label] = { ok: false, error: e.message };
    }
  };
  await probe('mainPlain', { target: { tabId }, world: 'MAIN', func: () => 1 + 1 });
  await probe('mainEval', {
    target: { tabId },
    world: 'MAIN',
    func: async (s) => await eval(s),
    args: ['(async () => 2 + 2)()']
  });
  await probe('isolatedEval', {
    target: { tabId },
    func: async (s) => await eval(s),
    args: ['(async () => 3 + 3)()']
  });
  if (msg.code) {
    // 复现 exec 的完整注入路径（buildPageScript），用来定位返回值为什么是 null。
    await probe('execPath', {
      target: { tabId },
      world: 'MAIN',
      func: async (s) => await eval(s),
      args: [buildPageScript(String(msg.code))]
    });
    await probe('execPathPlain', {
      target: { tabId },
      world: 'MAIN',
      func: async (s) => await eval(s),
      args: [String(msg.code)]
    });
  }
  return { ok: true, data: out };
}

/// 把 tabId/frameId/allFrames 归一成一个 executeScript target。
function buildScriptTarget(tabId, msg) {
  const target = { tabId };
  if (msg.allFrames === true || msg.all_frames === true) {
    target.allFrames = true;
    return target;
  }
  const raw = msg.frameId !== undefined ? msg.frameId : msg.frame_id;
  if (raw !== undefined && raw !== null && raw !== '') {
    const frameId = Number(raw);
    if (Number.isInteger(frameId) && frameId >= 0) target.frameIds = [frameId];
  }
  return target;
}

// ============================================================================
// persist match pattern 口径 —— **必须与 persist_boot.js 逐字等价**
// ----------------------------------------------------------------------------
// 两处服务的是同一份 `persistInjections` 配置：
//   * persist_boot.js：document_start 在页面里（ISOLATED world）做第一次匹配；
//   * 本文件：动态注册 content script 失败的浏览器上，退回
//     webNavigation.onCommitted 之后重放。
// 只要两边口径不同，用户就会看到「有些页面 document_start 生效、有些页面要等导航后」，
// 而且很难自查。所以两份实现刻意写成同一份算法（含注释），改一处必须改另一处。
//
// 语义 = Chrome match pattern 的「scheme / host / path」三段匹配，但因为 match
// pattern 本身不允许写端口，而实际 URL 常带端口（本地服务、内网），所以这里：
//   1. `<all_urls>`          → 只放行 http/https（`file:` 必须显式声明）
//   2. `<scheme>://host/path`
//        - scheme 为 `*` 时仅放行 http/https；其余按字面相等比较（file/ftp…）
//        - host 只比 hostname（**天然忽略端口**）；写 `:端口` 也会被剥掉后忽略
//        - host 里的 `*` 匹配任意字符但不跨 `/`
//        - path 里的 `*` 匹配任意字符（含 `/`），整体 `^...$`
//   3. 不含 `://` 的写法（如 `*/api/*`）→ 退回「整条 URL 做 glob」
//   4. patterns 配置损坏（存在但不是非空字符串数组）→ **不注入**，只 console.warn；
//      只有 patterns 缺失（undefined/null）才退回 `<all_urls>`（见 normalizePersistPatterns）
//
// 与 CDP 侧（deriveUrlPattern / buildFetchPatterns）的关系见那里的大段注释：
// CDP 的 urlPattern 只负责**缩小暂停范围**，命中判定仍由 interceptRuleMatches 做，
// 所以两者允许不完全相同，但 CDP 侧必须至少覆盖本函数能命中的 URL。
// ============================================================================
function normalizePersistPatterns(patterns) {
  // 区分「没配」与「配了但坏了」：
  //   undefined / null        → 没配，沿用历史的 `<all_urls>`（兼容老条目）
  //   非空字符串数组           → 过滤掉非字符串项后使用
  //   其它（字符串 / 空数组 / 对象 / 全是非字符串项）→ 返回 null 表示「不注入」
  // 以前的写法是 `Array.isArray(patterns) && patterns.length ? … : ['<all_urls>']`，
  // 配置一坏就变成「全站注入」——对用户自建的持久注入属于 fail-open。
  if (patterns === undefined || patterns === null) return ['<all_urls>'];
  if (!Array.isArray(patterns)) return null;
  const list = patterns.filter((item) => typeof item === 'string' && item.trim() !== '');
  return list.length ? list : null;
}

function describePersistPatterns(patterns) {
  if (Array.isArray(patterns)) {
    const kinds = Array.from(new Set(patterns.map((item) => (item === null ? 'null' : typeof item))));
    return `array(length=${patterns.length}, itemTypes=${kinds.join('|') || 'none'})`;
  }
  return typeof patterns;
}

function matchesPersistPattern(patterns, url) {
  const list = normalizePersistPatterns(patterns);
  if (!list) {
    console.warn('[TMWD] 持久注入的 matches 配置损坏，已跳过注入（不会退回 <all_urls>）:', describePersistPatterns(patterns));
    return false;
  }
  const escape = (text) => text.replace(/[.+?^${}()|[\]\\]/g, '\\$&');
  let parsed = null;
  try {
    parsed = new URL(url);
  } catch (_) {
    parsed = null;
  }
  return list.some((pattern) => {
    if (pattern === '<all_urls>') return /^https?:/i.test(url);
    if (!parsed) return false;
    const at = String(pattern).indexOf('://');
    if (at <= 0) {
      try {
        return new RegExp('^' + escape(pattern).replace(/\*/g, '.*') + '$', 'i').test(url);
      } catch (_) {
        return false;
      }
    }
    const scheme = pattern.slice(0, at).toLowerCase();
    const rest = pattern.slice(at + 3);
    const slash = rest.indexOf('/');
    // 端口写在 host 里时先剥掉：persist 只比 hostname，端口不该影响命中。
    const hostPattern = (slash >= 0 ? rest.slice(0, slash) : rest).toLowerCase().replace(/:\d+$/, '');
    const pathPattern = slash >= 0 ? rest.slice(slash) : '/';
    const protocol = String(parsed.protocol || '').replace(':', '').toLowerCase();
    if (scheme !== '*' && scheme !== protocol) return false;
    if (scheme === '*' && !/^https?$/.test(protocol)) return false;
    const hostRe = new RegExp('^' + escape(hostPattern).replace(/\*/g, '[^/]*') + '$', 'i');
    if (!hostRe.test(parsed.hostname)) return false;
    const pathRe = new RegExp('^' + escape(pathPattern).replace(/\*/g, '.*') + '$', 'i');
    return pathRe.test(parsed.pathname || '/');
  });
}

/// 无 eval 通道：页面 CSP 禁止 eval 时，用「注入文件 + 调用全局函数」执行固定操作。
/// 文件与 daemon 侧的 page_*.js 同源（tools/sync_extension 会一起复制）：
///   ops/page_read.js     -> window.__abcRead(opts)
///   ops/page_find.js     -> window.__abcFind(spec)
///   ops/page_snapshot.js -> window.__abcSnapshot(opts)
///   ops/page_action.js   -> window.__abcAct(op, target, params)
const PAGE_OP_FILES = {
  read: { file: 'ops/page_read.js', global: '__abcRead' },
  find: { file: 'ops/page_find.js', global: '__abcFind' },
  snapshot: { file: 'ops/page_snapshot.js', global: '__abcSnapshot' },
  action: { file: 'ops/page_action.js', global: '__abcAct' },
  wait: { file: 'ops/page_wait.js', global: '__abcWait' },
  net: { file: 'ops/page_net.js', global: '__abcNet' },
  challenge: { file: 'ops/page_challenge.js', global: '__abcChallenge' }
};

async function handlePageOp(msg) {
  const tabId = Number(msg.tabId);
  if (!Number.isInteger(tabId) || tabId <= 0) return { ok: false, error: 'tabId is required' };
  const op = String(msg.op || '');
  const entry = PAGE_OP_FILES[op];
  if (!entry) return { ok: false, error: 'unknown pageOp: ' + op };
  const world = msg.world === 'ISOLATED' ? 'ISOLATED' : 'MAIN';
  const target = buildScriptTarget(tabId, msg);
  const args = msg.args === undefined ? {} : msg.args;
  const invoke = async () => {
    const result = await chrome.scripting.executeScript({
      target,
      world,
      func: (name, payload) => {
        if (name === '__abcAct') {
          if (typeof window.__abcAct !== 'function') return { __needLoad: true };
          const p = payload || {};
          return window.__abcAct(p.op, p.target, p.params);
        }
        const fn = window[name];
        if (typeof fn !== 'function') return { __needLoad: true };
        return fn(payload || {});
      },
      args: [entry.global, args]
    });
    return result && result[0] ? result[0].result : null;
  };
  try {
    // 第一次调用只做探测：页面里已加载过就直接执行，省掉一次注入。
    let value = await invoke();
    if (value && typeof value === 'object' && value.__needLoad) {
      await chrome.scripting.executeScript({ target, world, files: [entry.file] });
      value = await invoke();
    }
    if (value && typeof value === 'object' && value.__needLoad) {
      return { ok: false, error: 'pageOp: 脚本注入被页面拒绝（' + entry.file + '）' };
    }
    return { ok: true, data: { op, value } };
  } catch (e) {
    return { ok: false, error: 'pageOp failed: ' + (e && e.message ? e.message : String(e)) };
  }
}

/// 在指定目标里执行用户代码，语义与 exec 一致（支持 return / 裸表达式 / await）。
async function runUserCodeInTab(tabId, code, world, target) {
  const result = await chrome.scripting.executeScript({
    target: target || { tabId },
    world: world === 'ISOLATED' ? 'ISOLATED' : 'MAIN',
    func: async (s) => await eval(s),
    args: [buildPageScript(code)]
  });
  const raw = result && result[0] ? result[0].result : null;
  if (raw && typeof raw === 'object' && 'ok' in raw) {
    if (raw.ok === false) {
      return { ok: false, error: (raw.error && raw.error.message) || 'inject failed' };
    }
    return { ok: true, value: raw.data === undefined ? null : raw.data };
  }
  return { ok: true, value: raw === undefined ? null : raw };
}

/// persistInjections 的读-改-写必须串行。
///
/// 之前每个 persist / remove 都是独立的 `get → 改 → set`：batch 里两条 persist，
/// 或 daemon 并发下发两条时，后写会把前写整份覆盖，条目静默丢失（用户只会发现
/// 「我明明加过，过一会就没了」）。这个 key 只有 background.js 自己写，所以一条
/// promise 链就够，不需要引入锁库。
let persistStoreQueue = Promise.resolve();

function withPersistStoreLock(task) {
  const run = persistStoreQueue.then(task, task);
  // 锁链条自身不能被 reject 卡死：业务错误照常抛给调用方，链条上只留已 settle 的尾巴。
  persistStoreQueue = run.then(() => null, () => null);
  return run;
}

async function readPersistInjections() {
  const store = await chrome.storage.local.get({ persistInjections: {} });
  const map = store.persistInjections;
  return map && typeof map === 'object' ? map : {};
}

/// 脚本注入：一次性执行（run）或按名字持久化（persist/remove/list）。
///
/// 持久化不是 registerContentScripts：MV3 只允许注册文件、不允许内联代码，
/// 所以改成「存代码 + 导航时自动重放」，代价是 document_start 的几毫秒窗口。
async function handleInject(msg) {
  const action = String(msg.action || 'run');
  const world = msg.world === 'ISOLATED' ? 'ISOLATED' : 'MAIN';

  if (STORE_BUILD && action !== 'list' && action !== 'remove') {
    return {
      ok: false,
      error:
        'exec / inject 不包含在 Chrome 应用商店版本中：该版本不执行运行期下发的代码。' +
        '改用 read / find / click / fill / snapshot 等固定命令，或从 GitHub 发行版安装完整扩展'
    };
  }

  if (action === 'list') {
    // 只读：不进锁，避免被一次正在进行的写挡住。
    const map = await readPersistInjections();
    return {
      ok: true,
      data: {
        action,
        count: Object.keys(map).length,
        items: Object.entries(map).map(([name, entry]) => ({
          name,
          world: entry.world || 'MAIN',
          matches: entry.matches || ['<all_urls>'],
          code_length: String(entry.code || '').length,
          code_preview: String(entry.code || '').slice(0, 120)
        }))
      }
    };
  }

  if (action === 'remove') {
    const name = String(msg.name || '').trim();
    if (!name) return { ok: false, error: 'name is required' };
    const saved = await withPersistStoreLock(async () => {
      const map = await readPersistInjections();
      const existed = !!map[name];
      delete map[name];
      await chrome.storage.local.set({ persistInjections: map });
      return { existed, count: Object.keys(map).length };
    });
    if (saved.count === 0) await ensurePersistBoot(false);
    return { ok: true, data: { action, name, removed: saved.existed, count: saved.count } };
  }

  if (action === 'persist') {
    const name = String(msg.name || '').trim();
    if (!name) return { ok: false, error: 'name is required' };
    const code = String(msg.code || '');
    if (!code) return { ok: false, error: 'code is required' };
    const matches = Array.isArray(msg.matches) && msg.matches.length ? msg.matches : ['<all_urls>'];
    // 读-改-写整段进锁；下面的 runUserCodeInTab 留在锁外，避免一条慢注入堵住其它 persist。
    const saved = await withPersistStoreLock(async () => {
      const map = await readPersistInjections();
      map[name] = { code, world, matches, createdAt: Date.now() };
      await chrome.storage.local.set({ persistInjections: map });
      return { count: Object.keys(map).length };
    });
    await ensurePersistBoot(true);
    let applied = false;
    try {
      const tabId = Number(msg.tabId);
      if (Number.isInteger(tabId) && tabId > 0) {
        const outcome = await runUserCodeInTab(tabId, code, world, buildScriptTarget(tabId, msg));
        applied = outcome.ok;
        if (!outcome.ok) console.log('[TMWD] persist apply failed:', outcome.error);
      }
    } catch (e) {
      console.log('[TMWD] persist apply failed:', e.message);
    }
    return { ok: true, data: { action, name, world, matches, applied, count: saved.count } };
  }

  // run：一次性执行
  const tabId = Number(msg.tabId);
  if (!Number.isInteger(tabId) || tabId <= 0) return { ok: false, error: 'tabId is required' };
  const code = String(msg.code || '');
  if (!code) return { ok: false, error: 'code is required' };
  const started = Date.now();
  try {
    const outcome = await runUserCodeInTab(tabId, code, world, buildScriptTarget(tabId, msg));
    if (!outcome.ok) return { ok: false, error: outcome.error };
    return {
      ok: true,
      data: {
        action: 'run',
        world,
        value: outcome.value,
        elapsed_ms: Date.now() - started
      }
    };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

const PERSIST_BOOT_ID = 'abc-persist-boot';
let persistBootReady = false;

/// 让 persist_boot.js 在 document_start 运行，这样持久注入能更早落到页面里。
/// 注册失败（旧版 Chrome / 权限受限）时退回 webNavigation.onCommitted 重放。
async function ensurePersistBoot(enable) {
  try {
    const existing = await chrome.scripting.getRegisteredContentScripts({ ids: [PERSIST_BOOT_ID] });
    if (enable) {
      if (existing && existing.length) {
        persistBootReady = true;
        return true;
      }
      await chrome.scripting.registerContentScripts([{
        id: PERSIST_BOOT_ID,
        js: ['persist_boot.js'],
        matches: ['<all_urls>'],
        runAt: 'document_start',
        allFrames: false,
        world: 'ISOLATED',
        persistAcrossSessions: true
      }]);
      persistBootReady = true;
      return true;
    }
    if (existing && existing.length) {
      await chrome.scripting.unregisterContentScripts({ ids: [PERSIST_BOOT_ID] });
    }
    persistBootReady = false;
    return true;
  } catch (e) {
    console.log('[TMWD] persist boot toggle failed:', e.message);
    persistBootReady = false;
    return false;
  }
}

async function syncPersistBoot() {
  const store = await chrome.storage.local.get({ persistInjections: {} });
  const count = Object.keys(store.persistInjections || {}).length;
  await ensurePersistBoot(count > 0);
}

// ---------------------------------------------------------------------------
// 持久注入「同一次页面导航只执行一次」的去重
//
// persistBootReady 是内存标志：SW 冷启动窗口内它是 false，于是 webNavigation.onCommitted
// 会重放一次，而 persist_boot.js 的 document_start 注入同时也在跑 → 用户自定义的
// persist 代码执行两遍（net hook 不受影响，page_net.js 有自己的幂等保护）。
//
// 这里只防「重复执行」，**不改变注入时机**：两条路径谁先到谁执行，另一条在 1.5s
// 窗口内被丢弃。键 = tabId + frameId + 去掉 hash 的 URL，所以「换一份配置后再注入
// 同一个页面」这种合法重复不会被误挡（它发生在窗口之外）。
//
// 优先存 chrome.storage.session：SW 被回收后去重表还在，正好覆盖「冷启动窗口内两条
// 注入路径都到齐」这个唯一场景；没有 session 命名空间时退回内存 Map（同一 SW 实例内
// 同样能挡住重复）。
// ---------------------------------------------------------------------------
const PERSIST_DEDUPE_KEY = 'persistInjectDedupe';
const PERSIST_DEDUPE_WINDOW_MS = 1500;
const PERSIST_DEDUPE_MAX_AGE_MS = 60 * 1000;
const persistDedupeMemory = new Map();

function persistDedupeArea() {
  return chrome.storage?.session || null;
}

function persistDedupeKeyOf(tabId, frameId, url) {
  const frame = Number(frameId);
  return `${tabId}|${Number.isInteger(frame) ? frame : 0}|${String(url || '').split('#')[0]}`;
}

/// 返回 true = 这次可以执行。存储不可用/读失败时一律放行：
/// 宁可多执行一次，也不能因为去重把用户的注入整条挡掉。
async function claimPersistInjection(tabId, frameId, url) {
  if (!Number.isInteger(tabId)) return true;
  const key = persistDedupeKeyOf(tabId, frameId, url);
  const now = Date.now();
  const area = persistDedupeArea();
  if (!area) {
    for (const [k, ts] of persistDedupeMemory) {
      if (now - ts > PERSIST_DEDUPE_MAX_AGE_MS) persistDedupeMemory.delete(k);
    }
    const last = persistDedupeMemory.get(key);
    if (last !== undefined && now - last < PERSIST_DEDUPE_WINDOW_MS) return false;
    persistDedupeMemory.set(key, now);
    return true;
  }
  try {
    const stored = await area.get({ [PERSIST_DEDUPE_KEY]: {} });
    const raw = stored[PERSIST_DEDUPE_KEY];
    const map = raw && typeof raw === 'object' ? raw : {};
    const last = Number(map[key]);
    if (Number.isFinite(last) && now - last < PERSIST_DEDUPE_WINDOW_MS) return false;
    map[key] = now;
    for (const k of Object.keys(map)) {
      if (now - Number(map[k]) > PERSIST_DEDUPE_MAX_AGE_MS) delete map[k];
    }
    await area.set({ [PERSIST_DEDUPE_KEY]: map });
    return true;
  } catch (e) {
    console.warn('[TMWD] 持久注入去重表不可用，本次不加去重:', e && e.message);
    return true;
  }
}

/// document_start 的 boot 脚本发来的请求：立刻在 MAIN world 执行对应代码。
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (!message || message.type !== 'persist-apply') return false;
  const tabId = sender?.tab?.id;
  if (!Number.isInteger(tabId)) return false;
  (async () => {
    const applied = [];
    // 与 onCommitted 的重放共用同一个去重键：同一次导航最多执行一遍。
    if (!(await claimPersistInjection(tabId, sender.frameId, sender.url || sender.tab?.url))) {
      sendResponse({ ok: true, applied, deduped: true });
      return;
    }
    for (const entry of message.entries || []) {
      try {
        const outcome = await runUserCodeInTab(tabId, entry.code, entry.world || 'MAIN', { tabId });
        applied.push({ name: entry.name, ok: outcome.ok, error: outcome.error || null });
      } catch (e) {
        applied.push({ name: entry.name, ok: false, error: e.message });
      }
    }
    sendResponse({ ok: true, applied });
  })();
  return true;
});

function summarizeDownload(item) {
  if (!item) return null;
  return {
    id: item.id,
    filename: item.filename,
    url: item.url,
    final_url: item.finalUrl,
    state: item.state,
    paused: !!item.paused,
    can_resume: !!item.canResume,
    bytes_received: item.bytesReceived,
    total_bytes: item.totalBytes,
    mime: item.mime,
    start_time: item.startTime,
    end_time: item.endTime,
    error: item.error || null,
    exists: item.exists
  };
}

/// 从 URL 末段推导下载文件名（去 query / hash，解百分号转义）。
/// 推导不出来就返回空串，让 Chrome 按 Content-Disposition 决定，
/// 而不是塞一个没有扩展名的字面量（例如 `download` 这种必然要用户手动改名的文件）。
function deriveDownloadFilename(url) {
  let parsed;
  try {
    parsed = new URL(String(url));
  } catch (_) {
    return '';
  }
  if (!/^(https?|ftp|file):$/i.test(parsed.protocol)) return '';
  const segments = String(parsed.pathname || '').split('/').filter(Boolean);
  if (!segments.length) return '';
  let name = segments[segments.length - 1];
  try { name = decodeURIComponent(name); } catch (_) { /* 保留原样 */ }
  // Chrome 会拒绝这些字符，换成下划线而不是让整条下载失败。
  name = name.replace(/[\\/:*?"<>|\x00-\x1f]/g, '_').trim();
  if (!name || name === '.' || name === '..') return '';
  // 末段没有扩展名（如 /api/download?id=1）时几乎都不是真正的文件名，交给 Chrome。
  if (!/\.[A-Za-z0-9]{1,8}$/.test(name)) return '';
  return name;
}

/// 下载监听：list / start / cancel。
/// wait 由 daemon 侧轮询，避免扩展 Service Worker 在长下载里被回收。
async function handleDownloads(msg) {
  try {
    if (!chrome.downloads) return { ok: false, error: 'downloads API unavailable' };
    const action = String(msg.action || 'list');
    if (action === 'start') {
      const url = String(msg.url || '').trim();
      if (!url) return { ok: false, error: 'url is required' };
      const options = { url, saveAs: false, conflictAction: 'uniquify' };
      // 文件名优先级：显式 --filename > 从 URL 末段推导。
      // 以前没给 filename 时直接写死 'download'，于是 `download <url> --dir sub` 会
      // 得到一个没有扩展名的字面量 download 文件，用户还得手动改名。
      const explicitName = msg.filename ? String(msg.filename) : '';
      const fileName = explicitName || deriveDownloadFilename(url);
      const dir = msg.dir ? String(msg.dir).replace(/^[\\/]+/, '').replace(/[\\/]+$/, '') : '';
      if (fileName) {
        // --dir 只影响目录前缀；目录为空时就是纯文件名。
        options.filename = dir ? `${dir}/${fileName}` : fileName;
      } else if (dir) {
        // 推导不出文件名就不能只传目录（Chrome 会拒绝以 '/' 结尾的 filename），
        // 这时宁可整条 filename 都不传，让 Chrome 按 Content-Disposition 决定。
        console.warn('[TMWD] downloads: 无法从 URL 推导文件名，已忽略 --dir，交由 Chrome 决定');
      }
      const id = await chrome.downloads.download(options);
      const [item] = await chrome.downloads.search({ id });
      return { ok: true, data: { action, id, item: summarizeDownload(item) } };
    }
    if (action === 'cancel') {
      const id = Number(msg.id);
      if (!Number.isInteger(id)) return { ok: false, error: 'id is required' };
      await chrome.downloads.cancel(id);
      return { ok: true, data: { action, id, cancelled: true } };
    }
    if (action === 'erase') {
      const hasId = msg.id !== undefined && msg.id !== null && msg.id !== '';
      const hasState = !!msg.state;
      // chrome.downloads.erase({}) 会清空浏览器的**全部**下载历史，这是不可撤销的破坏性操作。
      // 缺省 id/state 时必须由调用方显式声明 confirmAll:true，否则直接拒绝。
      if (!hasId && !hasState && msg.confirmAll !== true) {
        return {
          ok: false,
          error: 'erase 需要指定 id 或 state；如果要清空全部下载历史，请显式传 confirmAll:true'
        };
      }
      const query = {};
      if (hasId) query.id = Number(msg.id);
      if (hasState) query.state = String(msg.state);
      const erased = await chrome.downloads.erase(query);
      return { ok: true, data: { action, erased, all: !hasId && !hasState } };
    }
    const query = {};
    if (msg.id !== undefined && msg.id !== null && msg.id !== '') query.id = Number(msg.id);
    if (msg.state) query.state = String(msg.state);
    if (msg.url) query.url = String(msg.url);
    if (msg.query) query.filenameRegex = String(msg.query);
    const items = await chrome.downloads.search(query);
    items.sort((a, b) => String(b.startTime || '').localeCompare(String(a.startTime || '')));
    const limit = Math.max(1, Math.min(Number(msg.limit || 20), 200));
    return {
      ok: true,
      data: {
        action: 'list',
        count: items.length,
        limit,
        items: items.slice(0, limit).map(summarizeDownload)
      }
    };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function handleFrames(msg, sender) {
  try {
    const tabId = resolveTabId(msg, sender);
    if (!chrome.webNavigation?.getAllFrames) {
      return { ok: false, error: 'webNavigation API 不可用（manifest 需要 "webNavigation" 权限）' };
    }
    const frames = await chrome.webNavigation.getAllFrames({ tabId });
    return {
      ok: true,
      data: {
        tabId,
        count: frames ? frames.length : 0,
        frames: (frames || []).map((frame) => ({
          frame_id: frame.frameId,
          parent_frame_id: frame.parentFrameId,
          url: frame.url
        }))
      }
    };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function handleExtMessage(msg, sender) {
  if (msg.cmd === 'status') return await handleStatus();
  if (msg.cmd === 'setPort') return await handleSetPort(msg);
  if (msg.cmd === 'setProfileLabel') return await handleSetProfileLabel(msg);
  if (msg.cmd === 'setWsToken') return await handleSetWsToken(msg);
  lastCommandAt = Date.now();
  if (msg.cmd === 'cookies') return await handleCookies(msg, sender);
  if (msg.cmd === 'cdp') return await handleCDP(msg, sender);
  if (msg.cmd === 'batch') return await handleBatch(msg, sender);
  if (msg.cmd === 'openTab') return await handleOpenTab(msg);
  if (msg.cmd === 'closeTab') return await handleCloseTab(msg, sender);
  if (msg.cmd === 'networkStart') return await handleNetworkStart(msg, sender);
  if (msg.cmd === 'networkList') return await handleNetworkList(msg, sender);
  if (msg.cmd === 'networkDetail') return await handleNetworkDetail(msg, sender);
  if (msg.cmd === 'networkClear') return await handleNetworkClear(msg, sender);
  if (msg.cmd === 'networkStop') return await handleNetworkStop(msg, sender);
  if (msg.cmd === 'consoleStart') return await handleConsoleStart(msg, sender);
  if (msg.cmd === 'consoleList') return await handleConsoleList(msg, sender);
  if (msg.cmd === 'consoleClear') return await handleConsoleClear(msg, sender);
  if (msg.cmd === 'consoleStop') return await handleConsoleStop(msg, sender);
  if (msg.cmd === 'debugClearAll') return await handleDebugClearAll();
  if (msg.cmd === 'tabs') {
    try {
      if (msg.method === 'switch') {
        const tab = await chrome.tabs.update(msg.tabId, { active: true });
        // 默认只切换 Chrome 内部 active tab，不抢占系统前台窗口。
        if (msg.allowFocus === true && tab.windowId) await chrome.windows.update(tab.windowId, { focused: true });
        return { ok: true };
      } else {
        const tabs = await queryScriptableTabs();
        const data = tabs.map(t => ({ id: t.id, url: t.url, title: t.title, active: t.active, windowId: t.windowId }));
        return { ok: true, data };
      }
    } catch (e) { return { ok: false, error: e.message }; }
  }
  if (msg.cmd === 'inject') {
    return await handleInject(msg);
  }
  if (msg.cmd === 'frames') {
    return await handleFrames(msg, sender);
  }
  if (msg.cmd === 'downloads') {
    return await handleDownloads(msg);
  }
  if (msg.cmd === 'pageOp') {
    return await handlePageOp(msg);
  }
  if (msg.cmd === 'netIntercept') {
    return await handleNetIntercept(msg, sender);
  }
  if (msg.cmd === 'injectProbe') {
    return await handleInjectProbe(msg, sender);
  }
  if (msg.cmd === 'selfCheck') {
    return await handleSelfCheck();
  }
  if (msg.cmd === 'management') {
    try {
      if (msg.method === 'list') {
        const all = await chrome.management.getAll();
        return { ok: true, data: all.map(e => ({ id: e.id, name: e.name, enabled: e.enabled, type: e.type, version: e.version })) };
      }
      if (msg.method === 'reload') {
        chrome.alarms.create('tmwd-self-reload', { when: Date.now() + 200 });
        return { ok: true };
      }
      if (msg.method === 'disable') {
        await chrome.management.setEnabled(msg.extId, false);
        return { ok: true };
      }
      if (msg.method === 'enable') {
        await chrome.management.setEnabled(msg.extId, true);
        return { ok: true };
      }
      return { ok: false, error: 'Unknown method: ' + msg.method };
    } catch (e) { return { ok: false, error: e.message }; }
  }
  if (msg.cmd === 'contentSettings') {
    try {
      const type = msg.type || 'automaticDownloads';
      const setting = msg.setting || 'allow';
      const pattern = msg.pattern || '<all_urls>';
      await chrome.contentSettings[type].set({
        primaryPattern: pattern,
        setting: setting
      });
      return { ok: true };
    } catch (e) { return { ok: false, error: e.message }; }
  }
  return { ok: false, error: 'Unknown cmd: ' + msg.cmd };
}

async function handleStatus() {
  // status 可能在 WS 连上之前就被 popup / 角标调用，先把身份读出来，
  // 否则会返回 browserId: null（popup 里显示 '-'）。
  try { await ensureClientIdentity(); } catch (_) {}
  // token 只回「有没有配」，绝不回原文：status 会被 popup 与页面角标读到。
  const wsError = await hydrateWsConnectError();
  return {
    ok: true,
    data: {
      wsConnected: !!ws && ws.readyState === WebSocket.OPEN,
      wsUrl: getWsUrl(),
      wsPort,
      wsTokenSet: !!wsToken,
      lastConnectError: publicWsConnectError(wsError),
      lastCommandAt,
      browserId,
      profileId,
      profileLabel,
      extensionId: chrome.runtime?.id,
      extensionVersion,
      fileSchemeAccess: await isAllowedFileSchemeAccess()
    }
  };
}

/// 设置/清空 WS token。落 storage 后由 storage.onChanged 统一触发重连（外部写入同理）；
/// 这里额外显式重连一次，避免「onChanged 事件丢了 → 存了 token 却不生效」这种最难查的状态。
async function handleSetWsToken(msg) {
  const raw = msg.token === undefined || msg.token === null ? '' : String(msg.token);
  const next = raw.trim();
  // token 会被拼进 URL 查询串：只接受可见 ASCII，避免控制字符/换行导致握手异常难查。
  if (next && !/^[\x21-\x7e]{1,512}$/.test(next)) {
    return { ok: false, error: 'token 只能是可见 ASCII 字符（1-512 个），不能含空格或换行' };
  }
  await chrome.storage.local.set({ wsToken: next });
  wsToken = next;
  reconnectWS();
  return { ok: true, data: { wsTokenSet: !!wsToken, wsUrl: getWsUrl() } };
}

async function handleSetPort(msg) {
  const port = normalizePort(msg.port);
  await saveWsPort(port);
  reconnectWS();
  return {
    ok: true,
    data: {
      wsPort,
      wsUrl: getWsUrl(),
      wsConnected: !!ws && ws.readyState === WebSocket.OPEN
    }
  };
}

function normalizeProfileLabel(label) {
  const raw = String(label || '').trim().replace(/\s+/g, ' ');
  if (!raw) return null;
  if (raw.length > 40) throw new Error('Profile Label 长度不能超过 40 个字符');
  // label 是给人看的名字，不是标识符：只允许 [A-Za-z0-9_.-] 会让中文/带空格的名字
  // 在 popup 点「保存」时直接抛错（的已确认触发路径），而 daemon 侧对 label
  // 没有字符集限制。这里放宽到「任意可打印字符（允许空格）」，只拒绝控制字符和
  // 零宽字符——它们会让两个看起来相同的 label 在比较时不相等，是最难查的一类脏数据。
  if (/[\u0000-\u001f\u007f\u200b-\u200f\u2028\u2029\ufeff]/.test(raw)) {
    throw new Error('Profile Label 不能包含控制字符或零宽字符');
  }
  return raw;
}

async function handleSetProfileLabel(msg) {
  const label = normalizeProfileLabel(msg.label);
  // 先确保身份已就绪，否则返回值里的 browserId/profileId 会是 null。
  try { await ensureClientIdentity(); } catch (_) {}
  profileLabel = label;
  await chrome.storage.local.set({ profileLabel });
  await sendTabsUpdate();
  return {
    ok: true,
    data: {
      browserId,
      profileId,
      profileLabel
    }
  };
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  handleExtMessage(msg, sender)
    .then(sendResponse)
    // handleExtMessage 里即使有 try/catch 也仍可能抛（例如 sendTabsUpdate 的异常），
    // 之前没有 .catch → sendResponse 永不执行，popup 点「保存」毫无反应、只等通道超时。
    .catch((err) => {
      const message = String((err && err.message) || err);
      console.error('[TMWD] handleExtMessage failed:', message);
      try { sendResponse({ ok: false, error: message }); } catch (_) { /* 通道已关闭 */ }
    });
  return true;
});

async function handleCookies(msg, sender) {
  try {
    let url = msg.url || sender.tab?.url;
    if (!url && msg.tabId) {
      const tab = await chrome.tabs.get(msg.tabId);
      url = tab.url;
    }
    // chrome://、about:blank、view-source:、扩展页、chrome-extension:// 都没有 cookie，
    // 也没有「origin」。以前直接 url.match(...)[0] → TypeError: Cannot read properties
    // of null，报错与真实原因（这个页面不支持）无关，用户只能猜。
    let parsed = null;
    try { parsed = new URL(String(url || '')); } catch (_) { parsed = null; }
    if (!parsed || !/^https?:$/i.test(parsed.protocol)) {
      return { ok: false, error: '该页面不支持读取 cookie（仅 http/https）' };
    }
    const origin = parsed.origin;
    const all = await chrome.cookies.getAll({ url });
    const part = await chrome.cookies.getAll({ url, partitionKey: { topLevelSite: origin } }).catch(() => []);
    const merged = [...all];
    for (const c of part) {
      if (!merged.some(x => x.name === c.name && x.domain === c.domain)) merged.push(c);
    }
    return { ok: true, data: merged };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}


const debugSessions = new Map();
const DEBUG_DETACH_DELAY_MS = 30000;
const DEBUG_DETACH_ALARM_PREFIX = 'tmwd-debug-detach-';

function debugDetachAlarmName(tabId) {
  return `${DEBUG_DETACH_ALARM_PREFIX}${tabId}`;
}

/**
 * 「这个 tab 是否还有必须保持 debugger attach 的用途」——全扩展唯一的判定入口。
 *
 * attach 是 per-tab 的共享资源，被三种用途共用：
 *   1. network 监听（Network.enable）
 *   2. console 监听（Runtime/Log.enable）
 *   3. 正在生效的 Fetch 拦截（intercept.fetchEnabled）
 * 只要还有任何一个用途在线，就不能 detach；全部释放后才允许拆。
 *
 * 注意 `session.intercept` 本身**不算用途**：规则被删空后 Fetch 已经 disable，
 * 剩下的只有 rules/stats/log 这些数据（保留它们是为了 detach 之后 net log 还能读到）。
 * 把它算成用途会导致「规则删光但 debugger 永远 attach」——标签页顶部一直挂
 * 「正在调试」横条，还会让别的扩展无法 attach 该标签。
 */
function debugSessionInUse(session) {
  if (!session || !session.attached) return false;
  if (session.network || session.console) return true;
  return !!(session.intercept && session.intercept.fetchEnabled);
}

function scheduleDebugDetach(session, reason = 'idle') {
  // 未 attach，或还有用途（network/console/Fetch）→ 什么都不做。
  // detach 一律延迟到 alarm 里执行，避免「unroute 后立刻 route」这类连续命令反复 attach/detach 抖动。
  if (!session || !session.attached) return;
  if (debugSessionInUse(session)) return;
  session.detachReason = reason;
  chrome.alarms?.create?.(debugDetachAlarmName(session.tabId), { delayInMinutes: DEBUG_DETACH_DELAY_MS / 60000 });
}

function resolveTabId(msg, sender) {
  const tabId = Number(msg.tabId || sender.tab?.id);
  if (!Number.isInteger(tabId) || tabId <= 0) throw new Error('no tabId');
  return tabId;
}

function getDebugSession(tabId) {
  let session = debugSessions.get(tabId);
  if (!session) {
    session = {
      tabId,
      attached: false,
      network: false,
      console: false,
      requests: new Map(),
      requestOrder: [],
      logs: [],
      intercept: null,
      detachReason: null
    };
    debugSessions.set(tabId, session);
  }
  return session;
}

function isDebuggerAlreadyAttachedError(error) {
  return /Another debugger is already attached/i.test(String(error?.message || error || ''));
}

async function isDebuggerTargetAttached(tabId) {
  try {
    const targets = await chrome.debugger.getTargets();
    const target = targets.find(t => t.tabId === tabId);
    return !!target?.attached;
  } catch (e) {
    console.log('[TMWD-DEBUG] getTargets failed:', e.message);
    return false;
  }
}

async function attachDebuggerWithRecovery(tabId) {
  try {
    await chrome.debugger.attach({ tabId }, '1.3');
    return;
  } catch (e) {
    // crbug.com/40753571：页面里只要嵌了「别的扩展」的 iframe，Chrome 就拒绝 attach，
    // 报错文案却完全看不出原因。这里补一句可执行的提示。
    if (/chrome-extension:\/\/ URL of different extension/i.test(String(e?.message || ''))) {
      throw new Error(`${e.message}（该页面内嵌了其他扩展的 iframe，Chrome 不允许同时 attach；请暂时关闭相关扩展，或改用 exec / click 等 DOM 操作）`);
    }
    if (!isDebuggerAlreadyAttachedError(e)) throw e;

    // daemon 非 graceful 退出时，Chrome 可能还保留本扩展的旧 attach。
    // 这里只在 Chrome 明确报告该 tab 已 attached 后做一次 detach+retry，避免正常路径反复抖动。
    const attached = await isDebuggerTargetAttached(tabId);
    if (!attached) throw e;
    try {
      await chrome.debugger.detach({ tabId });
    } catch (detachError) {
      console.log('[TMWD-DEBUG] stale detach failed:', tabId, detachError.message);
      throw e;
    }
    await chrome.debugger.attach({ tabId }, '1.3');
  }
}

async function ensureDebugAttached(session) {
  // chrome.debugger 缺失时不能静默崩在这里（报「Cannot read properties of undefined」），
  // 要给出「为什么不可用」的可见错误，同时让调用方降级到 DOM 通道。
  if (!chrome.debugger?.attach) {
    throw new Error('chrome.debugger API 不可用：请确认 manifest 声明了 "debugger" 权限并且扩展已正确加载；本命令需要降级到 DOM 通道');
  }
  await chrome.alarms.clear(debugDetachAlarmName(session.tabId)).catch(() => null);
  session.detachReason = null;
  if (session.attached) return;
  await attachDebuggerWithRecovery(session.tabId);
  session.attached = true;
  // 后台标签页会被 Chrome 降级/冻结，长任务容易中途失联。
  // 已 attach debugger 时显式声明页面仍处于 active 生命周期，保证脚本与事件继续运转。
  try {
    await chrome.debugger.sendCommand({ tabId: session.tabId }, 'Page.setWebLifecycleState', { state: 'active' });
  } catch (_) {}
}

async function detachDebugSession(session, reason = 'manual') {
  if (!session) return;
  await chrome.alarms.clear(debugDetachAlarmName(session.tabId)).catch(() => null);
  if (session.attached) {
    try { await chrome.debugger.detach({ tabId: session.tabId }); } catch (e) {
      console.log('[TMWD-DEBUG] detach failed:', session.tabId, reason, e.message);
    }
  }
  session.attached = false;
  session.network = false;
  session.console = false;
  markInterceptDetached(session);
  session.detachReason = null;
  // detach 之后 Fetch 一定不可用：把「这个 tab 曾经启用拦截」的记录也清掉，
  // 否则 SW 重启后的 restoreDebugSessions 会对着一个已 detach 的 tab 白试一次。
  await persistDebugInterceptState();
}

/// detach 之后 Fetch 一定已经不可用，但 rules/stats/log 这些**数据**要留着：
/// 规则被删空自动 detach 后，`net log` / `status` 仍然要能读到刚才的拦截记录。
/// 只把 fetchEnabled 置 false（= 不再算作「用途」），不要整个清空。
function markInterceptDetached(session) {
  if (!session || !session.intercept) return;
  session.intercept.fetchEnabled = false;
}

async function detachDebugIfIdle(session) {
  // 判定逻辑全部收敛在 scheduleDebugDetach / debugSessionInUse 里，这里只表达「现在没在忙」。
  scheduleDebugDetach(session, 'idle');
}

async function detachAllDebugSessions(reason = 'cleanup') {
  const sessions = [...debugSessions.values()];
  for (const session of sessions) {
    session.requests.clear();
    session.requestOrder = [];
    session.logs = [];
    await detachDebugSession(session, reason);
  }
  debugSessions.clear();
  // 全部 detach 后内存里没有可恢复的拦截状态了，顺手清掉持久化记录。
  await persistDebugInterceptState();
}

// ---------------------------------------------------------------------------
// Fetch 拦截状态的冷启动恢复
//
// debugSessions 是纯内存 Map，MV3 的 Service Worker 随时会被回收；但 attach 与
// Fetch.enable 是**浏览器侧**状态。SW 冷启动后新事件照常送达，内存里却没有规则，
// 旧代码直接 `return` → 被暂停的请求永远没人 continue → 页面永久转圈。
//
// 这里把「哪些 tab 正在拦截」落到 storage，SW 启动时对仍然 attached 的 tab 重建
// session 并重新 Fetch.enable。注意 chrome.storage.session 在 MV3 需要 "storage"
// 权限（manifest 已有）；老 Chrome（<102）没有该命名空间时退化到 storage.local。
// 取舍：local 会跨浏览器重启保留，理论上可能在「重启后 tab 还是 attached」的罕见
// 组合下复活过期规则；但恢复只对 getTargets() 里 attached===true 的 tab 执行，
// 且真正的规则内容仍由 daemon 重下发覆盖，所以影响可控。
// ---------------------------------------------------------------------------
const DEBUG_INTERCEPT_STORE_KEY = 'debugInterceptState';

function debugInterceptStoreArea() {
  return chrome.storage?.session || chrome.storage?.local || null;
}

/// 把当前「正在生效」的拦截状态落盘。只持久化 fetchEnabled===true 的会话：
/// stop / detach 之后不应该再被恢复（否则重启会把用户手动停掉的拦截又装回去）。
async function persistDebugInterceptState() {
  const area = debugInterceptStoreArea();
  if (!area) return;
  const snapshot = {};
  for (const [tabId, session] of debugSessions) {
    const state = session.intercept;
    if (!state || !state.fetchEnabled) continue;
    snapshot[tabId] = {
      rules: state.rules,
      responseStage: !!state.responseStage,
      intercept: true,
      fetchEnabled: true
    };
  }
  try {
    await area.set({ [DEBUG_INTERCEPT_STORE_KEY]: snapshot });
  } catch (e) {
    // 配额/权限问题只影响「重启后能否自愈」，不能影响正在进行的拦截。
    console.warn('[TMWD-FETCH] 持久化拦截状态失败:', e && e.message);
  }
}

/// SW 启动时调用：从 storage 读回拦截状态，对仍 attached 的 tab 重建 session 并重新开启 Fetch。
/// 任何一步失败都只 warn，绝不能让 SW 启动路径 reject。
async function restoreDebugSessions() {
  const area = debugInterceptStoreArea();
  if (!area) return;
  let snapshot = {};
  try {
    const stored = await area.get({ [DEBUG_INTERCEPT_STORE_KEY]: {} });
    snapshot = stored[DEBUG_INTERCEPT_STORE_KEY] || {};
  } catch (e) {
    console.warn('[TMWD-FETCH] 读取持久化拦截状态失败:', e && e.message);
    return;
  }
  const tabIds = Object.keys(snapshot)
    .map(Number)
    .filter((tabId) => Number.isInteger(tabId) && tabId > 0);
  if (!tabIds.length) return;
  if (!chrome.debugger?.getTargets) {
    console.warn('[TMWD-FETCH] chrome.debugger.getTargets 不可用，跳过拦截状态恢复');
    return;
  }
  let attachedTabs;
  try {
    const targets = await chrome.debugger.getTargets();
    attachedTabs = new Set((targets || []).filter(t => t && t.attached).map(t => t.tabId));
  } catch (e) {
    console.warn('[TMWD-FETCH] getTargets 失败，跳过拦截状态恢复:', e && e.message);
    return;
  }
  for (const tabId of tabIds) {
    const saved = snapshot[tabId];
    if (!saved || !saved.fetchEnabled) continue;
    // 只有「浏览器侧还 attached」的 tab 才值得恢复；否则 Fetch 已经随 detach 失效。
    if (!attachedTabs.has(tabId)) continue;
    try {
      const session = getDebugSession(tabId);
      session.attached = true;
      const state = interceptStateOf(session);
      state.rules = normalizeInterceptRules(saved.rules);
      state.responseStage = !!saved.responseStage;
      const patterns = buildFetchPatterns(state.rules, state.responseStage);
      if (!patterns.length) {
        state.fetchEnabled = false;
        state.patterns = [];
        continue;
      }
      await chrome.debugger.sendCommand(
        { tabId },
        'Fetch.enable',
        { patterns: toFetchEnablePatterns(patterns) }
      );
      state.fetchEnabled = true;
      state.patterns = patterns;
      console.log('[TMWD-FETCH] 已恢复 tab', tabId, '的拦截规则:', state.rules.length);
    } catch (e) {
      console.warn('[TMWD-FETCH] 恢复 tab', tabId, '的拦截失败:', e && e.message);
    }
  }
  // 恢复失败的条目（或 tab 已不存在）顺手清掉，避免每次冷启动都重试同一份脏数据。
  await persistDebugInterceptState();
}

async function handleNetworkStart(msg, sender) {
  try {
    const tabId = resolveTabId(msg, sender);
    const session = getDebugSession(tabId);
    await ensureDebugAttached(session);
    await chrome.debugger.sendCommand({ tabId }, 'Network.enable', {});
    session.network = true;
    return { ok: true, status: 'started', tabId, count: session.requestOrder.length };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function handleNetworkList(msg, sender) {
  try {
    const tabId = resolveTabId(msg, sender);
    const session = getDebugSession(tabId);
    const filter = String(msg.filter || '').toLowerCase();
    const limit = Math.max(1, Math.min(Number(msg.limit || 100), 1000));
    let items = session.requestOrder.map(id => session.requests.get(id)).filter(Boolean);
    if (filter) {
      items = items.filter(item => [item.url, item.method, item.status, item.mimeType, item.resourceType].some(v => String(v || '').toLowerCase().includes(filter)));
    }
    items = items.slice(-limit).map(summarizeRequest);
    return { ok: true, status: session.network ? 'started' : 'stopped', tabId, count: items.length, requests: items };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function handleNetworkDetail(msg, sender) {
  try {
    const tabId = resolveTabId(msg, sender);
    const requestId = String(msg.requestId || '');
    const session = getDebugSession(tabId);
    const item = session.requests.get(requestId);
    if (!item) return { ok: false, error: 'unknown requestId: ' + requestId };
    const detail = Object.assign({}, item);
    // 区分「未采集」与「取不到」：以前未 attach 时两个字段都不出现，调用方无法判断
    // 到底是没开 debugger 还是响应体已经被丢弃。
    if (!session.attached) {
      detail.body = null;
      detail.bodyError = 'debugger 未 attach，未采集响应体';
    } else if (!item.completed) {
      detail.body = null;
      detail.bodyError = '请求尚未结束，未采集响应体';
    } else if (item.failed) {
      detail.body = null;
      detail.bodyError = '请求失败，没有响应体';
    } else {
      try {
        const body = await chrome.debugger.sendCommand({ tabId }, 'Network.getResponseBody', { requestId });
        detail.body = truncateText(body.body || '', 20000);
        detail.base64Encoded = !!body.base64Encoded;
        detail.bodyTruncated = String(body.body || '').length > 20000;
      } catch (e) {
        detail.bodyError = e.message;
      }
    }
    return { ok: true, tabId, request: detail };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function handleNetworkClear(msg, sender) {
  try {
    const tabId = resolveTabId(msg, sender);
    const session = getDebugSession(tabId);
    session.requests.clear();
    session.requestOrder = [];
    return { ok: true, status: 'cleared', tabId };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function handleNetworkStop(msg, sender) {
  try {
    const tabId = resolveTabId(msg, sender);
    const session = getDebugSession(tabId);
    if (session.attached) {
      try { await chrome.debugger.sendCommand({ tabId }, 'Network.disable', {}); } catch (_) {}
    }
    session.network = false;
    session.requests.clear();
    session.requestOrder = [];
    await detachDebugIfIdle(session);
    return { ok: true, status: 'stopped', cleared: true, tabId };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function handleConsoleStart(msg, sender) {
  try {
    const tabId = resolveTabId(msg, sender);
    const session = getDebugSession(tabId);
    await ensureDebugAttached(session);
    await chrome.debugger.sendCommand({ tabId }, 'Runtime.enable', {});
    await chrome.debugger.sendCommand({ tabId }, 'Log.enable', {}).catch(() => null);
    session.console = true;
    return { ok: true, status: 'started', tabId, count: session.logs.length };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function handleConsoleList(msg, sender) {
  try {
    const tabId = resolveTabId(msg, sender);
    const session = getDebugSession(tabId);
    const level = String(msg.level || '').toLowerCase();
    const limit = Math.max(1, Math.min(Number(msg.limit || 100), 1000));
    let logs = session.logs;
    if (level) logs = logs.filter(item => String(item.level || '').toLowerCase() === level);
    return { ok: true, status: session.console ? 'started' : 'stopped', tabId, count: logs.length, logs: logs.slice(-limit) };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function handleConsoleClear(msg, sender) {
  try {
    const tabId = resolveTabId(msg, sender);
    const session = getDebugSession(tabId);
    session.logs = [];
    return { ok: true, status: 'cleared', tabId };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function handleConsoleStop(msg, sender) {
  try {
    const tabId = resolveTabId(msg, sender);
    const session = getDebugSession(tabId);
    if (session.attached) {
      try { await chrome.debugger.sendCommand({ tabId }, 'Runtime.disable', {}); } catch (_) {}
      try { await chrome.debugger.sendCommand({ tabId }, 'Log.disable', {}); } catch (_) {}
    }
    session.console = false;
    session.logs = [];
    await detachDebugIfIdle(session);
    return { ok: true, status: 'stopped', cleared: true, tabId };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

// --- 网络层拦截（CDP Fetch 域） ---
//
// 与 page_net.js 的页面级 hook 互补：那边包装 fetch/XHR，改不了 responseType=blob，
// 也拦不到 img/script 这类非 JS 发起的请求；这里在网络层出手。
//
// 规则字段（都可选）：
//   match / method                       命中条件（URL 片段或正则；method 支持 *）
//   abort                                请求直接失败
//   status / body / base64Body / headers 伪造响应（不真实发出请求）
//   requestHeaders / requestBody         放行但改请求
//   response                             放行并拿到真实响应后改写 {status, body, base64Body, headers}
function emptyInterceptStats() {
  return { matched: 0, mocked: 0, aborted: 0, rewritten: 0, continued: 0, failed: 0 };
}

/// CDP 的 body 一律是 base64。这里按 UTF-8 编码，不能用 btoa(unescape(encodeURIComponent()))。
function textToBase64(text) {
  const bytes = new TextEncoder().encode(String(text === undefined || text === null ? '' : text));
  let binary = '';
  for (let i = 0; i < bytes.length; i += 1) binary += String.fromCharCode(bytes[i]);
  return btoa(binary);
}

/// 规则里的 body 允许字符串或对象，与 page_net 的 ruleBody 保持一致。
function interceptBodyText(body) {
  if (body === undefined || body === null) return '';
  return typeof body === 'string' ? body : JSON.stringify(body);
}

function headersToEntries(headers) {
  const out = [];
  if (!headers) return out;
  if (Array.isArray(headers)) {
    for (const item of headers) {
      if (!item) continue;
      if (Array.isArray(item)) out.push({ name: String(item[0]), value: String(item[1] === undefined ? '' : item[1]) });
      else if (item.name !== undefined) out.push({ name: String(item.name), value: String(item.value === undefined ? '' : item.value) });
    }
    return out;
  }
  if (typeof headers === 'object') {
    for (const key of Object.keys(headers)) out.push({ name: key, value: String(headers[key]) });
  }
  return out;
}

/// CDP 的 headers 是整体替换，所以必须「基于原头部合并覆盖」后再全量传，否则原有头会丢。
function mergeHeaderEntries(base, ...patches) {
  const entries = headersToEntries(base);
  const merged = {
    order: entries.map(entry => entry.name.toLowerCase()),
    map: new Map()
  };
  for (const entry of entries) {
    const key = entry.name.toLowerCase();
    if (!merged.map.has(key)) merged.map.set(key, entry);
  }
  for (const patch of patches) {
    for (const entry of headersToEntries(patch)) {
      const key = entry.name.toLowerCase();
      // 同名（大小写不敏感）只保留一次，规则里的值覆盖原值。
      merged.map.set(key, { name: key, value: entry.value });
      if (!merged.order.includes(key)) merged.order.push(key);
    }
  }
  return merged.order.filter(key => merged.map.has(key)).map(key => merged.map.get(key));
}

function dropHeaderEntries(entries, names) {
  const banned = new Set(names.map(name => name.toLowerCase()));
  return entries.filter(entry => !banned.has(String(entry.name).toLowerCase()));
}

function interceptHasResponseStage(rules) {
  return (rules || []).some(rule => rule && rule.response && typeof rule.response === 'object');
}

/// 规则的 match 是「子串 / glob / 正则」三合一的宽松写法，而 CDP Fetch 的 urlPattern
/// 只认 `*`（和 `?`）通配。这里把 match 尽力收窄成 urlPattern，原则是
/// **宁可多拦（回退 `*`），绝不漏拦**：漏拦会让规则静默失效，多拦只是多一次 JS 判断。
///
/// 与 persist（matchesPersistPattern / persist_boot.js）的口径对齐：
///   * 无 scheme 的 match（`*/api/*`）→ 前置 `*` 之后 http 与 https 都命中，与 persist 一致；
///   * 端口：persist 只比 hostname（忽略端口），这里推导时也把 `:端口` 从过滤条件里剥掉，
///     避免「规则写了端口 → 换个端口就静默不命中」；
///   * `*` 是唯一的通配符，其余字符按字面量 —— 与 persist 的 glob 语义相同。
///
/// **逐条独立推导**：某条规则推导失败只让它自己退化成 `*`，不会把其它规则一起拖成 `*`。
/// 返回值里的 source 会原样进 `status.patterns`，让用户看得见「是这条正则逼得全量拦截」：
///   exact    —— match 不含 `*`，按子串收窄（`api/mock` → `*api/mock*`）
///   glob     —— match 含 `*`，按 glob 收窄（`*/api/*` → `*/api/*`）
///   fallback —— 无法安全推导，退化成 `*`（正则 / 元字符 / 完整 URL / 非法字符 / `#`）
function deriveUrlPattern(match) {
  const raw = String(match || '').trim();
  const fallback = { urlPattern: '*', source: 'fallback' };
  if (!raw || raw === '*') return fallback;
  // /.../flags 正则写法无法翻译
  if (/^\/.*\/[a-z]*$/i.test(raw)) return fallback;
  // 正则里常见的元字符（含 `.*` 这种 mix 写法）：CDP 会当字面量，翻译必然漏拦。
  if (/[\\^$+()[\]{}|]/.test(raw) || /\.\*/.test(raw)) return fallback;
  // 完整 URL 写法（带 scheme）不参与收窄：`*https://h/x*` 这种「前导通配 + scheme」
  // 在 CDP 里的语义没有明确保证，宁可退化成 `*`。
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(raw) || raw.includes('://')) return fallback;
  // `#` 不会出现在请求 URL 里，带上必然永不命中。
  if (raw.includes('#')) return fallback;
  // 只保留 URL 里本来就常见的字符，其余一律放弃收窄。
  if (!/^[\w\-.~:/?@!$&'%*+,;=]+$/.test(raw)) return fallback;
  // 显式写了端口（`host:8080/api`）时把端口从**过滤条件**里去掉：
  // persist 的三段匹配只比 hostname（天然忽略端口），这里跟着忽略，避免
  // 「规则写了端口 → CDP 只暂停这一个端口 → 换个端口就静默不命中」。
  // 只放宽过滤条件，真正的命中判定仍由 interceptRuleMatches 按原文做。
  const widened = raw.replace(/(^|\/)([A-Za-z0-9._-]+):\d{1,5}(?=\/|$)/, '$1$2');
  if (widened.includes('*')) {
    // glob：两侧补通配，保证「子串位置」也能命中（`api/*` 也应命中 https://h/api/x）。
    const head = widened.startsWith('*') ? '' : '*';
    const tail = widened.endsWith('*') ? '' : '*';
    return { urlPattern: head + widened + tail, source: 'glob' };
  }
  // 纯子串：两侧补通配
  return { urlPattern: '*' + widened + '*', source: 'exact' };
}

/// 按当前规则生成 Fetch 的 patterns（附带 source 供 status 展示）。
/// 返回空数组表示「没有任何请求需要拦」，调用方应当直接关掉 Fetch，
/// 这样「开着 CDP 引擎但没规则」时页面零开销（否则每个请求都要过一次 CDP 往返）。
///
/// 注意：这里返回的对象**不能**直接传给 Fetch.enable —— 多出来的 source/match
/// 字段会被 CDP 当成非法参数。enableFetchInterception 会先 strip 成
/// `{ urlPattern, requestStage }` 再发。
function buildFetchPatterns(rules, withResponseStage) {
  const list = Array.isArray(rules) ? rules : [];
  const seen = new Set();
  const patterns = [];
  const push = (rule, urlPattern, source, requestStage) => {
    const key = `${requestStage}|${urlPattern}`;
    if (seen.has(key)) return;
    seen.add(key);
    patterns.push({ urlPattern, requestStage, source, match: String(rule.match) });
  };
  for (const rule of list) {
    if (!rule || !rule.match) continue;
    const { urlPattern, source } = deriveUrlPattern(rule.match);
    // 只改响应体的规则不需要在请求阶段暂停，少一次往返。
    if (interceptRuleHasRequestEffect(rule)) push(rule, urlPattern, source, 'Request');
    // 响应阶段会让每个命中的请求多一次往返，只在确实有 response 规则时才开。
    if (withResponseStage && rule.response && typeof rule.response === 'object') {
      push(rule, urlPattern, source, 'Response');
    }
  }
  return patterns;
}

/// Fetch.enable 只接受 RequestPattern 的合法字段，多余字段会报 Invalid parameters。
function toFetchEnablePatterns(patterns) {
  return (patterns || []).map(item => ({ urlPattern: item.urlPattern, requestStage: item.requestStage }));
}

/// 哪些规则的 match 逼得链路退化成 `*`（正则 / 元字符等）——给 status 用。
/// 直接从规则算，而不是从 patterns 算：patterns 会按 (stage|urlPattern) 去重，
/// 去重后可能丢掉「某条正则也退化成了 *」这条信息。
function fallbackMatchesOf(rules) {
  const out = [];
  for (const rule of rules || []) {
    if (!rule || !rule.match) continue;
    const match = String(rule.match);
    if (deriveUrlPattern(match).source !== 'fallback') continue;
    if (!out.includes(match)) out.push(match);
  }
  return out;
}

function isInterceptMockRule(rule) {
  if (!rule) return false;
  if (rule.status !== undefined || rule.body !== undefined || rule.base64Body !== undefined) return true;
  // headers 单独出现就是「伪造响应头」；但如果同时有 response，headers 属于响应改写。
  return rule.headers !== undefined && !rule.response;
}

function interceptRuleHasRequestEffect(rule) {
  if (!rule) return false;
  return rule.abort === true
    || isInterceptMockRule(rule)
    || rule.requestHeaders !== undefined
    || rule.requestBody !== undefined;
}

/// glob → RegExp：只把 `*` 当通配，其余字符全部按字面量。与 persist 的匹配口径一致。
function globToRegExp(glob) {
  const escaped = String(glob).replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*');
  return new RegExp('^' + escaped + '$', 'i');
}

/// 单条规则的 match 判定，三种写法（与 persist 的 glob 口径统一）：
///   1. 含 `*` → 按 glob 整体匹配（`*/api/*` 这类写法终于能命中）；
///   2. 纯子串 → includes 命中（向下兼容老规则 `api/mock`）；
///   3. `/re/flags` 或裸正则 → RegExp 命中（保留历史行为，写错也不抛）。
function interceptRuleMatches(match, target) {
  const pattern = String(match || '');
  if (!pattern) return false;
  if (pattern.includes('*')) {
    try {
      if (globToRegExp(pattern).test(target)) return true;
    } catch (_) { /* 落到下面的字面量兜底 */ }
    const literal = pattern.replace(/\*/g, '');
    if (!literal) return true;
    return target.includes(literal);
  }
  if (target.includes(pattern)) return true;
  // /re/flags：先剥掉两侧斜杠，否则正则会去匹配字面量斜杠，等于永远不命中。
  if (pattern.length > 2 && pattern.startsWith('/') && /\/[a-z]*$/i.test(pattern)) {
    const end = pattern.lastIndexOf('/');
    try { return new RegExp(pattern.slice(1, end), pattern.slice(end + 1)).test(target); } catch (_) { return false; }
  }
  try { return new RegExp(pattern).test(target); } catch (_) { return false; }
}

function matchInterceptRule(rules, url, method) {
  const target = String(url || '');
  const verb = String(method || 'GET').toUpperCase();
  for (const rule of rules || []) {
    if (!rule || !rule.match) continue;
    if (rule.method && rule.method !== '*' && String(rule.method).toUpperCase() !== verb) continue;
    if (interceptRuleMatches(rule.match, target)) return rule;
  }
  return null;
}

function normalizeInterceptRules(rules) {
  const list = Array.isArray(rules) ? rules : [];
  return list
    .filter(rule => rule && typeof rule === 'object' && rule.match)
    .slice(0, 200)
    .map(rule => Object.assign({}, rule, { match: rule.match }));
}

function interceptStateOf(session) {
  if (!session.intercept) {
    session.intercept = {
      rules: [],
      patterns: [],
      fetchEnabled: false,
      responseStage: false,
      debugPass: INTERCEPT_LOG_PASS_DEFAULT,
      stats: emptyInterceptStats(),
      log: []
    };
  }
  return session.intercept;
}

const INTERCEPT_LOG_LIMIT = 200;
// status 只回最近 100 条，否则大日志会把消息通道拖慢。
const INTERCEPT_LOG_STATUS_LIMIT = 100;
// 放行（pass）是绝大多数请求，默认不记日志：200 条环形缓冲几秒就会被它挤满，
// 真正有价值的 mock/rewrite 记录反而全部丢失。排查时用 netIntercept 的 debugPass 打开。
const INTERCEPT_LOG_PASS_DEFAULT = false;

function interceptLogOf(state) {
  if (!state) return [];
  if (!Array.isArray(state.log)) state.log = [];
  return state.log;
}

/// 环形日志：每条记录一个真正被处理过的请求（mock/abort/continue/rewrite/pass）。
/// 字段固定，daemon 侧要和 page_net 的记录合并排序，缺字段会让两边的格式对不上。
function recordInterceptLog(state, entry) {
  if (!state || !entry) return;
  // pass 默认不落盘，避免把 mock/rewrite 记录挤出环形缓冲。
  if (entry.action === 'pass' && !state.debugPass) return;
  const log = interceptLogOf(state);
  const item = {
    at: Date.now(),
    requestId: entry.requestId,
    url: truncateText(entry.url, 300),
    method: entry.method,
    stage: entry.stage,
    action: entry.action
  };
  if (entry.status !== undefined && entry.status !== null) item.status = Number(entry.status);
  if (entry.match !== undefined && entry.match !== null) item.match = truncateText(entry.match, 100);
  if (entry.resourceType) item.resourceType = String(entry.resourceType);
  // 只留一小段预览：够看出「改成了什么」，又不至于把日志撑爆。
  if (entry.preview !== undefined && entry.preview !== null) item.preview = truncateText(entry.preview, 160);
  log.push(item);
  if (log.length > INTERCEPT_LOG_LIMIT) log.splice(0, log.length - INTERCEPT_LOG_LIMIT);
}

function bumpInterceptStat(session, key) {
  if (!session.intercept || !session.intercept.stats) return;
  if (session.intercept.stats[key] === undefined) session.intercept.stats[key] = 0;
  session.intercept.stats[key] += 1;
}

/// 所有 Fetch.* 调用都走这里：请求可能已经被处理/取消，报错只记 failed，不抛。
async function sendFetchCommand(session, method, params, statKey) {
  try {
    const result = await chrome.debugger.sendCommand({ tabId: session.tabId }, method, params);
    if (statKey) bumpInterceptStat(session, statKey);
    return { ok: true, result };
  } catch (e) {
    bumpInterceptStat(session, 'failed');
    return { ok: false, error: String((e && e.message) || e) };
  }
}

/// continueResponse 是较新的方法；老版本 Chrome 没有它，只能退回 continueRequest。
/// **无论 continueResponse 报什么错都必须再放行一次**：被 Fetch.enable 暂停的请求
/// 如果没有被 continue/fail/fulfill 显式处置，网络栈会一直挂着，页面表现为永久转圈且无报错。
/// 宁可重复发一次 continueRequest（第二次会被 CDP 忽略），也不要把请求挂死。
async function continueFetchResponse(session, requestId, guard) {
  const sent = await sendFetchCommand(session, 'Fetch.continueResponse', { requestId }, 'continued');
  if (sent.ok) {
    if (guard) guard.settled = true;
    return true;
  }
  if (!isFetchMethodMissingError(sent.error)) {
    console.warn('[TMWD-FETCH] Fetch.continueResponse 失败，回退 Fetch.continueRequest:', sent.error);
  }
  const fallback = await sendFetchCommand(session, 'Fetch.continueRequest', { requestId }, 'continued');
  if (fallback.ok && guard) guard.settled = true;
  return fallback.ok;
}

/// 老版本 Chrome 缺 continueResponse 时的典型报错（方法不存在），其余错误都算「意外」。
function isFetchMethodMissingError(message) {
  return /wasn't found|isn't found|not found|not supported|Unknown method/i.test(String(message || ''));
}

/// 一个被暂停的请求「已经被处置过」的标记。
/// 真正的释放动作由 handleFetchPaused 的 finally 兜底，任何漏掉的分支都会被补一次 continue。
function createFetchGuard() {
  return { settled: false };
}

/// 处置类命令（continueRequest / continueResponse / failRequest / fulfillRequest）专用：
/// 成功即视为该请求已放行，finally 里就不会再补一次。
async function settleFetch(session, guard, method, params, statKey) {
  const out = await sendFetchCommand(session, method, params, statKey);
  if (out.ok && guard) guard.settled = true;
  return out;
}

/// 兜底放行一个被暂停的请求：先按**事件所在阶段**选方法，失败再降级到另一个。
///
/// continueRequest 只对 Request 阶段有效、continueResponse 只对 Response 阶段有效，
/// 用错方法时 CDP 会拒绝（错误还常被吞进 stats.failed），于是请求永远挂着 —— 页面
/// 表现为白屏/转圈且没有任何报错。所以两条路径都要试，都失败才算失败。
///
/// 阶段判定依据与 handleFetchPaused 一致：Fetch.requestPaused 的 params 里只要带了
/// responseStatusCode / responseErrorReason 就是 Response 阶段（isFetchResponseStage）。
async function releasePausedRequest(session, requestId, params) {
  if (!session || !requestId) return false;
  const responseStage = isFetchResponseStage(params || {});
  const preferred = responseStage ? 'Fetch.continueResponse' : 'Fetch.continueRequest';
  const fallbackMethod = responseStage ? 'Fetch.continueRequest' : 'Fetch.continueResponse';
  const sent = await sendFetchCommand(session, preferred, { requestId });
  if (sent.ok) return true;
  if (!isFetchMethodMissingError(sent.error)) {
    console.warn('[TMWD-FETCH] 兜底放行首选失败，降级重试:', preferred, '→', fallbackMethod, sent.error);
  }
  const fallback = await sendFetchCommand(session, fallbackMethod, { requestId });
  if (fallback.ok) return true;
  // 两次都失败：请求很可能已经结束（Invalid InterceptionId），但绝不能静默。
  // 这两次失败已由 sendFetchCommand 计入 stats.failed，这里再打一条 error 留痕。
  console.error('[TMWD-FETCH] 兜底放行两次都失败，请求可能已挂起:', requestId,
    preferred, sent.error, '|', fallbackMethod, fallback.error);
  return false;
}

async function enableFetchInterception(session, rules) {
  const state = interceptStateOf(session);
  const wantResponseStage = interceptHasResponseStage(rules);
  const patterns = buildFetchPatterns(rules, wantResponseStage);
  // 没有任何请求需要拦（规则为空 / 规则都不需要 CDP）→ 彻底关掉 Fetch，
  // 这样「开着 CDP 引擎但没规则」时页面零额外开销。
  if (!patterns.length) {
    if (state.fetchEnabled) {
      try { await chrome.debugger.sendCommand({ tabId: session.tabId }, 'Fetch.disable', {}); } catch (_) {}
      state.fetchEnabled = false;
    }
    state.responseStage = false;
    state.patterns = [];
    // Fetch 关了就不再算「用途」：如果 network/console 也没开，安排一次延迟 detach，
    // 免得标签页顶部一直挂「正在调试」横条、并挡住其它扩展 attach。
    await persistDebugInterceptState();
    await detachDebugIfIdle(session);
    return patterns;
  }
  if (state.fetchEnabled && sameFetchPatterns(state.patterns, patterns)) {
    state.patterns = patterns;
    state.responseStage = wantResponseStage;
    await persistDebugInterceptState();
    return patterns;
  }
  // patterns 不能热更新：阶段/范围变了只能 disable + enable。
  if (state.fetchEnabled) {
    try { await chrome.debugger.sendCommand({ tabId: session.tabId }, 'Fetch.disable', {}); } catch (_) {}
    state.fetchEnabled = false;
  }
  const fallbackMatches = fallbackMatchesOf(rules);
  if (fallbackMatches.length) {
    // 只影响「过滤范围」不影响命中：这些规则无法安全翻译，只能全量暂停后再用 JS 判。
    console.warn('[TMWD-FETCH] 以下规则无法收窄 urlPattern，已退化为 *（全量拦截）:', fallbackMatches.join(' | '));
  }
  // Fetch.enable 失败时旧规则已经随上面的 disable 一起丢掉了：绝不能保留
  // rules=N / enabled:false 这种自相矛盾的状态——调用方会以为「规则还在，只是没启用」，
  // 实际拦截已经全部失效。这里把状态清成诚实的「什么都没有」，并把错误抛给上层。
  try {
    await chrome.debugger.sendCommand({ tabId: session.tabId }, 'Fetch.enable', { patterns: toFetchEnablePatterns(patterns) });
  } catch (e) {
    state.fetchEnabled = false;
    state.responseStage = false;
    state.patterns = [];
    state.rules = [];
    console.error('[TMWD-FETCH] Fetch.enable 失败，拦截已完全关闭:', e && e.message);
    await persistDebugInterceptState();
    await detachDebugIfIdle(session);
    throw new Error('fetch-enable-failed: ' + ((e && e.message) || String(e)));
  }
  state.fetchEnabled = true;
  state.responseStage = wantResponseStage;
  state.patterns = patterns;
  // 落盘：SW 被回收后 restoreDebugSessions 靠这份快照重建 Fetch 拦截。
  await persistDebugInterceptState();
  return patterns;
}

function sameFetchPatterns(a, b) {
  const left = Array.isArray(a) ? a : [];
  const right = Array.isArray(b) ? b : [];
  if (left.length !== right.length) return false;
  const key = (item) => `${item.requestStage} ${item.urlPattern}`;
  const set = new Set(left.map(key));
  return right.every((item) => set.has(key(item)));
}

async function handleNetIntercept(msg, sender) {
  let tabId;
  try {
    tabId = resolveTabId(msg, sender);
  } catch (e) {
    return { ok: false, error: e.message };
  }
  const action = String(msg.action || 'status').toLowerCase();
  const session = getDebugSession(tabId);

  if (action === 'status') {
    const state = session.intercept;
    const log = interceptLogOf(state);
    // patterns 每项形如 { urlPattern, requestStage, source, match }，
    // source 直接告诉用户这条 pattern 是精确推导、glob 推导，还是被迫退化成 `*`。
    const fallbackMatches = state ? fallbackMatchesOf(state.rules) : [];
    return {
      ok: true,
      action: 'status',
      tabId,
      enabled: !!(state && state.fetchEnabled),
      rules: state ? state.rules.length : 0,
      patterns: state ? state.patterns : [],
      // 退化清单非空 = 过滤范围是全量（只有这些规则需要全量，其余仍在精确范围内）。
      fallbackMatches,
      debugPass: state ? !!state.debugPass : INTERCEPT_LOG_PASS_DEFAULT,
      stats: state ? Object.assign({}, state.stats) : emptyInterceptStats(),
      log: log.slice(-INTERCEPT_LOG_STATUS_LIMIT),
      logCount: log.length
    };
  }

  if (action === 'clearlog') {
    if (session.intercept) session.intercept.log = [];
    return { ok: true, action: 'clearlog', tabId, logCount: 0 };
  }

  if (action === 'stop') {
    const state = session.intercept;
    const stats = state ? Object.assign({}, state.stats) : emptyInterceptStats();
    // stop 只停「拦截」，不销毁数据：rules/stats/log 仍然保留，
    // 这样 `net stop` 之后 `net log` / `status` 还能读到刚才的现场（排障用）。
    // 这与 markInterceptDetached 的注释是同一意图——detach 后要能读 net log。
    const logCount = state ? interceptLogOf(state).length : 0;
    if (session.attached) {
      try {
        await chrome.debugger.sendCommand({ tabId }, 'Fetch.disable', {});
      } catch (e) {
        // disable 失败意味着可能还有已暂停但无人认领的请求；至少把原因记下来。
        console.warn('[TMWD-FETCH] stop: Fetch.disable 失败:', e && e.message);
      }
    }
    if (state) {
      state.fetchEnabled = false;
      // patterns 已随 Fetch.disable 失效，继续报告会让人误以为「这些还在拦」。
      state.patterns = [];
    }
    // 不再算「用途」→ 交给延迟 detach；同时清掉持久化记录，避免 SW 重启后又把拦截装回去。
    await persistDebugInterceptState();
    await detachDebugIfIdle(session);
    return {
      ok: true,
      action: 'stop',
      tabId,
      stats,
      logCount,
      rules: state ? state.rules.length : 0,
      retained: !!state
    };
  }

  if (action !== 'start' && action !== 'rules') {
    return { ok: false, error: 'Unknown netIntercept action: ' + action };
  }

  try {
    const rules = normalizeInterceptRules(msg.rules);
    await ensureDebugAttached(session);
    const state = interceptStateOf(session);
    // start = 新会话（统计与日志清零）；rules = 只换规则，统计/日志继续累计。
    if (action === 'start') {
      state.stats = emptyInterceptStats();
      state.log = [];
    }
    // 排查用开关：打开后放行（pass）也会进日志，默认关闭以保住环形缓冲。
    if (msg.debugPass !== undefined) state.debugPass = !!msg.debugPass;
    state.rules = rules;
    const patterns = await enableFetchInterception(session, rules);
    return {
      ok: true,
      action,
      tabId,
      rules: rules.length,
      patterns,
      // 退化到 `*` 的 match（正则等）：这些规则会让过滤范围变成全量，但命中判定仍然精确。
      fallbackMatches: fallbackMatchesOf(rules),
      debugPass: !!state.debugPass,
      fetchEnabled: !!state.fetchEnabled
    };
  } catch (e) {
    const message = (e && e.message) || String(e);
    // Fetch.enable 失败时 enableFetchInterception 已把状态清空；返回体必须与 status 一致，
    // 不能再带 rules:N 造成「规则还在」的假象。
    if (message.startsWith('fetch-enable-failed:')) {
      return {
        ok: false,
        action,
        tabId,
        enabled: false,
        rules: 0,
        patterns: [],
        fetchEnabled: false,
        error: message
      };
    }
    // attach 失败等要带原始 message，调用方才能知道是「别的扩展占着 debugger」还是真出错。
    return { ok: false, error: message };
  }
}

function isFetchResponseStage(params) {
  return params.responseStatusCode !== undefined || params.responseErrorReason !== undefined;
}

async function handleFetchPausedRequest(session, state, params, guard) {
  const requestId = params.requestId;
  const request = params.request || {};
  const url = String(request.url || '');
  const method = String(request.method || 'GET').toUpperCase();
  const resourceType = params.type || params.resourceType;
  const rules = state.rules.filter(interceptRuleHasRequestEffect);
  const rule = matchInterceptRule(rules, url, method);

  if (!rule) {
    await settleFetch(session, guard, 'Fetch.continueRequest', { requestId }, 'continued');
    recordInterceptLog(state, { requestId, url, method, resourceType, stage: 'request', action: 'pass' });
    return;
  }
  bumpInterceptStat(session, 'matched');

  if (rule.abort === true) {
    await settleFetch(session, guard, 'Fetch.failRequest', { requestId, errorReason: 'Failed' }, 'aborted');
    recordInterceptLog(state, { requestId, url, method, resourceType, stage: 'request', action: 'abort', match: rule.match });
    return;
  }

  if (isInterceptMockRule(rule)) {
    let body;
    if (rule.base64Body !== undefined) body = String(rule.base64Body);
    else body = textToBase64(interceptBodyText(rule.body));
    const explicit = headersToEntries(rule.headers);
    const hasType = explicit.some(entry => entry.name.toLowerCase() === 'content-type');
    const responseHeaders = hasType
      ? explicit
      : [{ name: 'content-type', value: 'application/json' }].concat(explicit);
    const responseCode = Number(rule.status) || 200;
    await settleFetch(session, guard, 'Fetch.fulfillRequest', {
      requestId,
      responseCode,
      responseHeaders,
      body
    }, 'mocked');
    recordInterceptLog(state, {
      requestId, url, method, resourceType, stage: 'request', action: 'mock', status: responseCode, match: rule.match,
      preview: rule.base64Body !== undefined ? '(base64 body)' : interceptBodyText(rule.body)
    });
    return;
  }

  if (rule.requestHeaders !== undefined || rule.requestBody !== undefined) {
    const payload = { requestId };
    if (rule.requestHeaders !== undefined) {
      payload.headers = mergeHeaderEntries(request.headers, rule.requestHeaders);
    }
    // GET/HEAD 不能带 body，与 page_net 的处理保持一致。
    if (rule.requestBody !== undefined && method !== 'GET' && method !== 'HEAD') {
      payload.postData = textToBase64(interceptBodyText(rule.requestBody));
    }
    await settleFetch(session, guard, 'Fetch.continueRequest', payload, 'continued');
    recordInterceptLog(state, { requestId, url, method, resourceType, stage: 'request', action: 'continue', match: rule.match });
    return;
  }

  await settleFetch(session, guard, 'Fetch.continueRequest', { requestId }, 'continued');
  recordInterceptLog(state, { requestId, url, method, resourceType, stage: 'request', action: 'pass', match: rule.match });
}

async function handleFetchPausedResponse(session, state, params, guard) {
  const requestId = params.requestId;
  const request = params.request || {};
  const url = String(request.url || '');
  const method = String(request.method || 'GET').toUpperCase();
  const resourceType = params.type || params.resourceType;
  const rule = matchInterceptRule(
    state.rules.filter(item => item && item.response && typeof item.response === 'object'),
    url,
    method
  );

  if (!rule) {
    await continueFetchResponse(session, requestId, guard);
    recordInterceptLog(state, {
      requestId, url, method, resourceType, stage: 'response', action: 'pass', status: params.responseStatusCode
    });
    return;
  }
  bumpInterceptStat(session, 'matched');

  const spec = rule.response;
  const responseCode = Number(spec.status) || Number(params.responseStatusCode) || 200;
  const mergedHeaders = mergeHeaderEntries(params.responseHeaders, rule.headers, spec.headers);
  const bodyReplaced = spec.base64Body !== undefined || spec.body !== undefined;
  recordInterceptLog(state, {
    requestId, url, method, resourceType, stage: 'response', action: 'rewrite', status: responseCode, match: rule.match,
    preview: bodyReplaced
      ? (spec.base64Body !== undefined ? '(base64 body)' : interceptBodyText(spec.body))
      : undefined
  });

  // 只改状态码 / 响应头：用 continueResponse 原地放行，不碰 body。
  // 走 getResponseBody + fulfillRequest 会把压缩响应重新编码一次，
  // content-encoding 与真实字节容易对不上（blob / 二进制响应尤其明显）。
  if (!bodyReplaced) {
    const sent = await settleFetch(session, guard, 'Fetch.continueResponse', {
      requestId,
      responseCode,
      responseHeaders: mergedHeaders
    }, 'rewritten');
    if (sent.ok) return;
    if (!isFetchMethodMissingError(sent.error)) {
      // 意外错误：continueFetchResponse 会再放行一次（失败也会退到 continueRequest）。
      await continueFetchResponse(session, requestId, guard);
      return;
    }
    // 老版本 Chrome 没有 continueResponse：退回「取体 + fulfill」，
    // 此时保持原始头不动（体没换，长度与编码仍然有效）。
    let passthrough;
    try {
      passthrough = await chrome.debugger.sendCommand({ tabId: session.tabId }, 'Fetch.getResponseBody', { requestId });
    } catch (e) {
      bumpInterceptStat(session, 'failed');
      await continueFetchResponse(session, requestId, guard);
      return;
    }
    await settleFetch(session, guard, 'Fetch.fulfillRequest', {
      requestId,
      responseCode,
      responseHeaders: mergedHeaders,
      body: passthrough && typeof passthrough.body === 'string' ? passthrough.body : ''
    }, 'rewritten');
    return;
  }

  let original;
  try {
    original = await chrome.debugger.sendCommand({ tabId: session.tabId }, 'Fetch.getResponseBody', { requestId });
  } catch (e) {
    // 拿不到原始体（请求已结束/被取消）就放行，别把请求挂死。
    bumpInterceptStat(session, 'failed');
    await continueFetchResponse(session, requestId, guard);
    return;
  }

  let body;
  if (spec.base64Body !== undefined) {
    body = String(spec.base64Body);
  } else if (spec.body !== undefined) {
    body = textToBase64(interceptBodyText(spec.body));
  } else {
    body = original && typeof original.body === 'string' ? original.body : '';
  }

  // 体换过之后 length/encoding 必然对不上，继续保留会让页面按旧长度截断。
  const responseHeaders = dropHeaderEntries(mergedHeaders, ['content-length', 'content-encoding']);

  await settleFetch(session, guard, 'Fetch.fulfillRequest', {
    requestId,
    responseCode,
    responseHeaders,
    body
  }, 'rewritten');
}

/// 所有 Fetch.requestPaused 的唯一入口。
/// 被暂停的请求必须被 continue/fail/fulfill 显式处置，否则会永久挂起，
/// 所以这里用 guard + finally 兜底：任何分支（包括 throw 出去）漏了放行，都会被补一次。
async function handleFetchPaused(session, params) {
  if (!params || !params.requestId) return;
  const requestId = params.requestId;
  const state = session.intercept;
  const guard = createFetchGuard();
  try {
    // 拦截已关闭（stop/detach 之后事件还在路上）时必须放行，否则请求会一直挂住。
    if (!state || !state.fetchEnabled) {
      // 成功即标记 settled，否则 finally 会再放行一次（重复 continue 会报 Invalid InterceptionId）。
      if (await releasePausedRequest(session, requestId, params)) guard.settled = true;
      return;
    }
    if (isFetchResponseStage(params)) await handleFetchPausedResponse(session, state, params, guard);
    else await handleFetchPausedRequest(session, state, params, guard);
  } finally {
    if (!guard.settled) await releasePausedRequest(session, requestId, params);
  }
}

function summarizeRequest(item) {
  return {
    requestId: item.requestId,
    url: item.url,
    method: item.method,
    status: item.status,
    mimeType: item.mimeType,
    resourceType: item.resourceType,
    completed: !!item.completed,
    failed: !!item.failed,
    errorText: item.errorText,
    timestamp: item.timestamp
  };
}

function rememberRequest(session, requestId) {
  if (!session.requests.has(requestId)) session.requestOrder.push(requestId);
  while (session.requestOrder.length > 1000) {
    const old = session.requestOrder.shift();
    session.requests.delete(old);
  }
  let item = session.requests.get(requestId);
  if (!item) {
    item = { requestId };
    session.requests.set(requestId, item);
  }
  return item;
}

function pushLog(session, item) {
  session.logs.push(item);
  if (session.logs.length > 1000) session.logs.splice(0, session.logs.length - 1000);
}

function remoteObjectText(arg) {
  if (!arg) return '';
  if ('value' in arg) {
    if (typeof arg.value === 'string') return truncateText(arg.value, 2000);
    try { return truncateText(JSON.stringify(arg.value), 2000); } catch (_) { return String(arg.value); }
  }
  return truncateText(arg.description || arg.type || '', 2000);
}

function truncateText(text, max) {
  const value = String(text || '');
  return value.length > max ? value.slice(0, max) : value;
}

/// SW 冷启动时 debugSessions 必然是空的，但浏览器侧的 Fetch 拦截仍然生效：
/// 此时送达的 Fetch.requestPaused 已经是一个被暂停的请求，**必须**放行。
/// 这里没有 session 对象可用，直接按阶段选 CDP 方法；失败只记日志，绝不再抛
/// （onDebuggerEvent 是同步监听器，抛出去只会变成未捕获异常，请求照样挂着）。
async function releaseOrphanFetchRequest(tabId, params) {
  const requestId = params && params.requestId;
  if (!requestId) return;
  const target = { tabId };
  const send = async (method, payload) => {
    try {
      await chrome.debugger.sendCommand(target, method, payload);
      return true;
    } catch (e) {
      console.warn('[TMWD-FETCH] 无会话放行失败:', method, requestId, e && e.message);
      return false;
    }
  };
  // 响应阶段的事件必须用 continueResponse；老 Chrome 没有该方法时退回 continueRequest。
  if (isFetchResponseStage(params)) {
    if (await send('Fetch.continueResponse', { requestId })) return;
  }
  await send('Fetch.continueRequest', { requestId });
}

function onDebuggerEvent(source, method, params) {
  const tabId = source.tabId;
  if (!tabId) return;
  const session = debugSessions.get(tabId);
  if (!session) {
    // SW 冷启动（或状态已随 tabs.onRemoved 清掉）时没有内存会话，但 CDP 请求仍可能
    // 处于暂停状态。其它事件可以安全忽略，唯独 Fetch.requestPaused 必须放行，
    // 否则页面会永久转圈且没有任何报错。
    if (method === 'Fetch.requestPaused') {
      releaseOrphanFetchRequest(tabId, params).catch((e) => {
        console.warn('[TMWD-FETCH] 无会话放行异常:', e && e.message);
      });
    }
    return;
  }
  if (method === 'Fetch.requestPaused') {
    if (!session.intercept) {
      // stop/detach 之后事件可能还在路上：必须放行，否则这个请求会一直挂住。
      // 阶段判断交给 releasePausedRequest（Response 阶段只能 continueResponse）。
      releasePausedRequest(session, params.requestId, params)
        .then((ok) => {
          if (!ok) console.warn('[TMWD-FETCH] 迟到 requestPaused 放行失败:', params.requestId);
        })
        .catch((e) => {
          console.warn('[TMWD-FETCH] 迟到 requestPaused 放行异常:', e && e.message);
        });
      return;
    }
    // onDebuggerEvent 是同步监听器，异步处理要自己兜住 rejection。
    handleFetchPaused(session, params).catch((e) => {
      console.warn('[TMWD-FETCH] requestPaused failed:', e && e.message);
      // 兜底：处理过程中抛出去时再放行一次，宁可重复也不要把请求挂死。
      releasePausedRequest(session, params.requestId, params);
    });
    return;
  }
  if (method === 'Network.requestWillBeSent') {
    const item = rememberRequest(session, params.requestId);
    item.url = params.request?.url || item.url;
    item.method = params.request?.method || item.method;
    item.resourceType = params.type || item.resourceType;
    item.timestamp = params.wallTime || params.timestamp || item.timestamp;
    item.requestHeaders = params.request?.headers;
  } else if (method === 'Network.responseReceived') {
    const item = rememberRequest(session, params.requestId);
    item.status = params.response?.status;
    item.statusText = params.response?.statusText;
    item.mimeType = params.response?.mimeType;
    item.responseHeaders = params.response?.headers;
    item.url = params.response?.url || item.url;
    item.resourceType = params.type || item.resourceType;
  } else if (method === 'Network.loadingFinished') {
    const item = rememberRequest(session, params.requestId);
    item.completed = true;
    item.encodedDataLength = params.encodedDataLength;
  } else if (method === 'Network.loadingFailed') {
    const item = rememberRequest(session, params.requestId);
    item.completed = true;
    item.failed = true;
    item.errorText = params.errorText;
  } else if (method === 'Runtime.consoleAPICalled') {
    pushLog(session, {
      level: params.type || 'log',
      text: (params.args || []).map(remoteObjectText).join(' '),
      timestamp: params.timestamp || Date.now(),
      url: params.stackTrace?.callFrames?.[0]?.url || '',
      line: params.stackTrace?.callFrames?.[0]?.lineNumber,
      column: params.stackTrace?.callFrames?.[0]?.columnNumber
    });
  } else if (method === 'Runtime.exceptionThrown') {
    pushLog(session, {
      level: 'error',
      text: params.exceptionDetails?.text || params.exceptionDetails?.exception?.description || 'exception thrown',
      timestamp: params.timestamp || Date.now(),
      url: params.exceptionDetails?.url || '',
      line: params.exceptionDetails?.lineNumber,
      column: params.exceptionDetails?.columnNumber
    });
  } else if (method === 'Log.entryAdded') {
    const e = params.entry || {};
    pushLog(session, {
      level: e.level || 'log',
      text: e.text || '',
      timestamp: e.timestamp || Date.now(),
      url: e.url || '',
      line: e.lineNumber,
      column: undefined
    });
  }
}

function onDebuggerDetach(source) {
  if (!source.tabId) return;
  chrome.alarms.clear(debugDetachAlarmName(source.tabId));
  const session = debugSessions.get(source.tabId);
  if (session) {
    session.attached = false;
    session.network = false;
    session.console = false;
    // 与 detachDebugSession 同一口径：保留 intercept 的规则与日志，只标记 Fetch 已关闭。
    // （我们自己调 detach 也会触发这个事件，所以两处必须一致，否则日志照样丢。）
    markInterceptDetached(session);
    session.detachReason = null;
    // detach 后 Fetch 不再生效，持久化记录也必须跟着失效。
    persistDebugInterceptState().catch(() => null);
  }
}

// 顶层注册必须全部走可选链：MV3 里 Service Worker 是在「脚本求值阶段」挂掉的，
// 任何一句 chrome.xxx.yyy.addListener 抛 TypeError 都会让整个扩展静默失效
// （popup 只显示「未连接」，没有任何线索）。
chrome.debugger?.onEvent?.addListener?.(onDebuggerEvent);
chrome.debugger?.onDetach?.addListener?.(onDebuggerDetach);



async function handleCloseTab(msg, sender) {
  try {
    const tabId = Number(msg.tabId || sender.tab?.id);
    if (!Number.isInteger(tabId) || tabId <= 0) throw new Error('tabId is required');
    const session = debugSessions.get(tabId);
    if (session?.attached) await detachDebugSession(session, 'close-tab');
    debugSessions.delete(tabId);
    await chrome.tabs.remove(tabId);
    return { ok: true, data: { status: 'success', tabId } };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function handleDebugClearAll() {
  const tabIds = [...debugSessions.keys()];
  await detachAllDebugSessions('debugClearAll');
  return { ok: true, status: 'cleared', tabs: tabIds.length };
}

async function handleOpenTab(msg) {
  try {
    const url = normalizeOpenUrl(msg.url);
    const active = msg.active !== false;
    if (msg.window === true) {
      // window 模式默认不聚焦，避免新窗口打断用户当前工作区。
      const win = await chrome.windows.create({ url, focused: msg.allowFocus === true });
      const tab = Array.isArray(win.tabs) && win.tabs.length ? win.tabs[0] : null;
      if (tab?.id && msg.keepAlive !== false) await keepTabAlive(tab.id);
      const group = tab?.id ? await groupTabIfRequested(tab.id, msg.groupTitle) : { ok: false, skipped: true, reason: 'window created without tab info' };
      return { ok: true, data: { id: tab?.id, url: tab?.url || url, title: tab?.title || '', active: !!tab?.active, windowId: win.id, window: true, group } };
    }
    const tab = await chrome.tabs.create({ url, active });
    // 受控标签页禁止被 Chrome「内存节省」自动丢弃，否则后台长任务会中途失去上下文。
    if (tab.id && msg.keepAlive !== false) await keepTabAlive(tab.id);
    const group = await groupTabIfRequested(tab.id, msg.groupTitle);
    // 默认创建/激活标签页但不聚焦浏览器窗口，避免打断当前工作区。
    if (active && msg.allowFocus === true && tab.windowId) await chrome.windows.update(tab.windowId, { focused: true });
    return { ok: true, data: { id: tab.id, url: tab.url || url, title: tab.title || '', active: tab.active, windowId: tab.windowId, window: false, group } };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function keepTabAlive(tabId) {
  try {
    await chrome.tabs.update(tabId, { autoDiscardable: false });
  } catch (e) {
    console.log('[TMWD] keepAlive failed:', tabId, e.message);
  }
}

async function groupTabIfRequested(tabId, title) {
  const cleanTitle = String(title || '').trim();
  if (!cleanTitle) return null;
  if (!chrome.tabs?.group || !chrome.tabGroups?.update) {
    return { ok: false, skipped: true, reason: 'tabGroups API unavailable' };
  }
  try {
    const existing = await findTabGroupByTitle(cleanTitle);
    const groupId = await chrome.tabs.group(existing ? { tabIds: tabId, groupId: existing.id } : { tabIds: tabId });
    await chrome.tabGroups.update(groupId, { title: cleanTitle });
    return { ok: true, id: groupId, title: cleanTitle };
  } catch (e) {
    // 分组只是整理标签，不影响打开页面的主流程。
    return { ok: false, skipped: true, reason: e.message };
  }
}

async function findTabGroupByTitle(title) {
  if (!chrome.tabGroups?.query) return null;
  const groups = await chrome.tabGroups.query({});
  return groups.find(group => group.title === title) || null;
}

function normalizeOpenUrl(url) {
  const raw = String(url || '').trim();
  if (!raw) throw new Error('url is required');
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(raw)) return raw;
  return 'https://' + raw;
}

async function handleBatch(msg, sender) {
  const R = [];
  const touchedSessions = new Set();
  const resolve$N = (params) => JSON.parse(JSON.stringify(params || {}).replace(/"\$(\d+)\.([^"]+)"/g,
    (_, i, path) => { let v = R[+i]; for (const k of path.split('.')) v = v[k]; return JSON.stringify(v); }));
  if (!Array.isArray(msg.commands)) {
    return { ok: false, error: 'commands must be an array', results: [], count: 0, failed: 0 };
  }
  const commands = msg.commands;
  try {
    for (let index = 0; index < commands.length; index += 1) {
      const c = commands[index] || {};
      if (c.tabId === undefined && msg.tabId !== undefined) c.tabId = msg.tabId;
      // 每条命令都带 `index`，并且**单条失败只记在这一条上**：
      // 以前 cdp 抛错会直接跳出整个循环，前面的结果一起丢掉，调用方看不出是哪一步坏了。
      try {
        if (c.cmd === 'cookies') {
          R.push(Object.assign({ index }, await handleCookies(c, sender)));
        } else if (c.cmd === 'tabs') {
          const tabs = await queryScriptableTabs();
          R.push({ index, ok: true, data: tabs.map(t => ({ id: t.id, url: t.url, title: t.title, active: t.active, windowId: t.windowId })) });
        } else if (c.cmd === 'cdp') {
          const tabId = c.tabId || msg.tabId || sender.tab?.id;
          if (!tabId) throw new Error('no tabId');
          if (c.method === 'Page.bringToFront' && c.allowFocus !== true && msg.allowFocus !== true) {
            R.push({ index, ok: true, data: { skipped: true, reason: 'Page.bringToFront requires allowFocus=true' } });
            continue;
          }
          const session = getDebugSession(tabId);
          await ensureDebugAttached(session);
          touchedSessions.add(session);
          const out = await chrome.debugger.sendCommand({ tabId }, c.method, resolve$N(c.params));
          // `data` 是统一后的规范位置；同时把 CDP 结果的顶层字段并进条目，
          // 兼容 daemon 里既有的 `results[N].result.value` / `$N.result.objectId` 读法
          //（daemon 侧不在本次改动范围，不能只留 data 把那些读法打断）。
          R.push(Object.assign({}, out, { index, ok: true, data: out }));
        } else {
          R.push({ index, ok: false, error: 'unknown cmd: ' + c.cmd });
        }
      } catch (e) {
        R.push({ index, ok: false, error: (e && e.message) || String(e) });
      }
    }
    for (const session of touchedSessions) scheduleDebugDetach(session, 'batch-idle');
    return { ok: true, results: R, count: R.length, failed: R.filter((item) => !item.ok).length };
  } catch (e) {
    for (const session of touchedSessions) scheduleDebugDetach(session, 'batch-error');
    return { ok: false, error: e.message, results: R, count: R.length, failed: R.filter((item) => !item.ok).length };
  }
}

async function handleCDP(msg, sender) {
  const tabId = msg.tabId || sender.tab?.id;
  if (!tabId) return { ok: false, error: 'no tabId' };
  if (msg.method === 'Page.bringToFront' && msg.allowFocus !== true) {
    return { ok: true, data: { skipped: true, reason: 'Page.bringToFront requires allowFocus=true' } };
  }
  const session = getDebugSession(tabId);
  try {
    await ensureDebugAttached(session);
    const result = await chrome.debugger.sendCommand({ tabId }, msg.method, msg.params || {});
    scheduleDebugDetach(session, 'cdp-idle');
    return { ok: true, data: result };
  } catch (e) {
    scheduleDebugDetach(session, 'cdp-error');
    return { ok: false, error: e.message };
  }
}
// Filter out chrome:// and other internal tabs that can't be scripted.
const isScriptable = (url, fileSchemeAccess = false) => !!url && (/^https?:/i.test(url) || (fileSchemeAccess && /^file:/i.test(url)));

/// chrome.extension 在少数 Chromium 分支/精简构建里可能缺失，缺了就当没有文件访问权。
async function isAllowedFileSchemeAccess() {
  try {
    return !!(await chrome.extension?.isAllowedFileSchemeAccess?.());
  } catch (_) {
    return false;
  }
}

async function queryScriptableTabs() {
  const fileSchemeAccess = await isAllowedFileSchemeAccess();
  return (await chrome.tabs.query({})).filter(tab => isScriptable(tab.url, fileSchemeAccess));
}

/// 上报给 daemon 的 tab 列表统一走这里。
/// streamlit 内部页不是用户可操作的浏览目标：ext_ready 的初始列表与 tabs_update
/// 必须用同一套过滤，否则 daemon 会先看到它、随后在全量更新里「消失」（等同于被关闭）。
function filterReportableTabs(tabs) {
  return (tabs || []).filter(t => !/streamlit/i.test(String((t && t.title) || '')));
}

async function injectContentScriptsIntoExistingTabs() {
  // manifest 已对新建页面注入 content_scripts；这里只需给「安装扩展前就打开着的标签页」补一次。
  // 之前每次 Service Worker 唤醒都全量重注入，多标签用户会有可感知的卡顿。
  try {
    const { injectedVersion } = await chrome.storage.local.get({ injectedVersion: null });
    if (injectedVersion === extensionVersion) return;
  } catch (_) {}
  const tabs = await queryScriptableTabs();
  for (const tab of tabs) {
    try {
      await chrome.scripting.executeScript({
        target: { tabId: tab.id, allFrames: true },
        files: ['config.js', 'content.js']
      });
    } catch (e) {
      console.log('[TMWD-WS] Inject content script failed:', tab.id, e.message);
    }
  }
  try {
    await chrome.storage.local.set({ injectedVersion: extensionVersion });
  } catch (_) {}
}

function buildDialogSuppressionScript(enabled) {
  if (!enabled) {
    return `(() => {
      const state = window.__TMWD_DIALOG_SUPPRESSION__;
      if (!state) return;
      state.count = Math.max(0, Number(state.count || 1) - 1);
      if (state.count > 0) return;
      try {
        window.alert = state.alert;
        window.confirm = state.confirm;
        window.prompt = state.prompt;
      } finally {
        delete window.__TMWD_DIALOG_SUPPRESSION__;
      }
    })()`;
  }
  return `(() => {
    const existing = window.__TMWD_DIALOG_SUPPRESSION__;
    if (existing) {
      existing.count = Number(existing.count || 1) + 1;
      return;
    }
    const state = {
      count: 1,
      alert: window.alert,
      confirm: window.confirm,
      prompt: window.prompt
    };
    window.__TMWD_DIALOG_SUPPRESSION__ = state;
    const toast = (type, msg) => {
      try { console.log('[TMWD] ' + type + ' suppressed during CLI command:', msg); } catch (_) {}
      try {
        const d = document.createElement('div');
        d.textContent = '[' + type + '] ' + msg;
        Object.assign(d.style, {
          position:'fixed', top:'12px', right:'12px', zIndex:'2147483647',
          background:'#222', color:'#fff', padding:'10px 18px', borderRadius:'8px',
          fontSize:'14px', maxWidth:'420px', wordBreak:'break-all',
          boxShadow:'0 4px 16px rgba(0,0,0,.3)', opacity:'1',
          transition:'opacity .5s', pointerEvents:'none'
        });
        (document.body || document.documentElement).appendChild(d);
        setTimeout(() => { d.style.opacity = '0'; }, 3000);
        setTimeout(() => { d.remove(); }, 3600);
      } catch (_) {}
    };
    window.alert = function(msg) { toast('alert', msg); };
    window.confirm = function(msg) { toast('confirm', msg); return true; };
    window.prompt = function(msg, def) { toast('prompt', msg); return def || null; };
  })()`;
}

async function setDialogSuppressionByScripting(tabId, enabled) {
  try {
    await chrome.scripting.executeScript({
      target: { tabId },
      world: 'MAIN',
      func: async (script) => await eval(script),
      args: [buildDialogSuppressionScript(enabled)]
    });
    return true;
  } catch (e) {
    console.log('[TMWD-WS] dialog suppression scripting failed:', e.message);
    return false;
  }
}

async function setDialogSuppressionByCdp(tabId, enabled) {
  try {
    await chrome.debugger.sendCommand({ tabId }, 'Runtime.evaluate', {
      expression: buildDialogSuppressionScript(enabled),
      awaitPromise: true,
      returnByValue: true
    });
    return true;
  } catch (e) {
    console.log('[TMWD-WS] dialog suppression CDP failed:', e.message);
    return false;
  }
}

// --- Shared page/CDP script builder core ---
function buildExecScript(code, errorHandler) {
  return `(async () => {
    // 序列化失败必须能传到外层（见下面的 return），不能伪装成正常的字符串结果。
    // 用 Symbol 当哨兵，避免与用户真实返回的 {message:...} 对象混淆。
    const __abcSerializeError = Symbol('abcSerializeError');
    function smartProcessResult(result) {
      if (result === null || result === undefined || typeof result !== 'object') return result;
      try { if (result.window === result && result.document) return '[Window: ' + (result.location?.href || 'about:blank') + ']'; } catch(_){}
      if (typeof jQuery !== 'undefined' && result instanceof jQuery) {
        const elements = []; for (let i = 0; i < result.length; i++) { if (result[i] && result[i].nodeType === 1) elements.push(result[i].outerHTML); } return elements;
      }
      if (result instanceof NodeList || result instanceof HTMLCollection) {
        const elements = []; for (let i = 0; i < result.length; i++) { if (result[i] && result[i].nodeType === 1) elements.push(result[i].outerHTML); } return elements;
      }
      if (result.nodeType === 1) return result.outerHTML;
      if (!Array.isArray(result) && typeof result === 'object' && 'length' in result && typeof result.length === 'number') {
        const firstElement = result[0];
        if (firstElement && firstElement.nodeType === 1) {
          const elements = []; const length = Math.min(result.length, 100);
          for (let i = 0; i < length; i++) { const elem = result[i]; if (elem && elem.nodeType === 1) elements.push(elem.outerHTML); } return elements;
        }
      }
      try { return JSON.parse(JSON.stringify(result, function(key, value) { if (typeof value === 'object' && value !== null) { if (value.nodeType === 1) return value.outerHTML; if (value === window || value === document) return '[Object]'; try { if (value.window === value && value.document) return '[Window]'; } catch(_){} } return value; })); } catch (e) { return { [__abcSerializeError]: true, message: String((e && e.message) || e) }; }
    }
    try {
      const jsCode = ${JSON.stringify(code)}.trim();
      const AsyncFunction = Object.getPrototypeOf(async function(){}).constructor;
      function _air(c) { const ls = c.split(/\\r?\\n/); let i = ls.length - 1; while (i >= 0 && !ls[i].trim()) i--; if (i < 0) return c; const t = ls[i].trim(); if (/^(return |return;|return$|let |const |var |if |if\\(|for |for\\(|while |while\\(|switch|try |throw |class |function |async |import |export |\\/\\/|})/.test(t)) return c; ls[i] = ls[i].match(/^(\\s*)/)[1] + 'return ' + t; return ls.join('\\n'); }
      // 先按原样求值：daemon 会把用户脚本包成自执行的 async 函数表达式，
      // 这种写法只有 eval 能拿到 IIFE 的返回值；一旦按「函数体」编译，
      // 末尾没有 return，整个 exec 就会永远得到 null。
      // 只有当 eval 因语句级 return / 顶层 await 报 SyntaxError 时，才退回函数体执行。
      let r;
      try {
        r = eval(jsCode);
        if (r instanceof Promise) r = await r;
      } catch (e) {
        if (e instanceof SyntaxError && (/return/i.test(e.message) || /await/i.test(e.message))) {
          // 有语句级 return 时按原样当函数体执行；否则说明是「表达式 + 顶层 await」，
          // 需要补一个 return，不然 await 的结果会被丢掉（返回 undefined）。
          const hasStatementReturn = /^\s*return\b/m.test(jsCode);
          try {
            r = await (new AsyncFunction(hasStatementReturn ? jsCode : _air(jsCode)))();
          } catch (e2) {
            r = await (new AsyncFunction(_air(jsCode)))();
          }
        } else {
          throw e;
        }
      }
      const processed = smartProcessResult(r);
      // 序列化失败必须报错：以前返回字符串 '[无法序列化: ...]' + 外层 ok:true，
      // daemon 会把它当成「成功的返回值」，把错误文案写进结果里。
      if (processed && typeof processed === 'object' && processed[__abcSerializeError]) {
        return { ok: false, error: '结果无法序列化: ' + processed.message };
      }
      return { ok: true, data: processed };
    } catch (e) {
      ${errorHandler}
    }
  })()`;
}

function buildPageScript(code) {
  return buildExecScript(code, `
      const errMsg = e.message || String(e);
      return { ok: false, error: { name: e.name || 'Error', message: errMsg, stack: e.stack || '' },
        csp: errMsg.includes('Refused to evaluate') || errMsg.includes('unsafe-eval') || errMsg.includes('Content Security Policy') };
  `);
}

function buildCdpScript(code) {
  return buildExecScript(code, `
      return { ok: false, error: { name: e.name || 'Error', message: e.message || String(e), stack: e.stack || '' } };
  `);
}

// --- WebSocket Client for TMWebDriver ---
let ws = null;

// 每次 Service Worker 冷启动 +1，随 ext_ready 上报（只加字段，不改已有字段）。
// daemon 据此识别「扩展重启过」并重新下发 netIntercept / networkStart / consoleStart。
// 注意：manifest.json 里 minimum_chrome_version = 120 的依据见文件顶部注释块。
const RESTART_SEQ_KEY = 'extRestartSeq';
let restartSeq = 0;
let restartSeqPromise = null;

function ensureRestartSeq() {
  if (!restartSeqPromise) {
    restartSeqPromise = (async () => {
      try {
        const data = await chrome.storage.local.get({ [RESTART_SEQ_KEY]: 0 });
        const next = (Number(data[RESTART_SEQ_KEY]) || 0) + 1;
        await chrome.storage.local.set({ [RESTART_SEQ_KEY]: next });
        restartSeq = next;
      } catch (e) {
        // 读失败就上报 0：daemon 仍能靠「序号变小」判定这是一次新启动。
        console.warn('[TMWD-WS] restartSeq 自增失败:', e && e.message);
        restartSeq = 0;
      }
      return restartSeq;
    })();
  }
  return restartSeqPromise;
}

/**
 * 安全发送：只向指定 socket 写，socket 已关闭时静默失败。
 *
 * 之前所有发送都读写全局 `ws`：daemon 空闲退出 / 重连时 `onclose` 会把它置 null，
 * 正在执行的命令在 await 之后 `ws.send(...)` 就会抛
 * "Cannot read properties of null (reading 'send')"，命令也永远收不到回复。
 */
function socketSend(socket, payload) {
  if (!socket || socket.readyState !== WebSocket.OPEN) return false;
  try {
    socket.send(typeof payload === 'string' ? payload : JSON.stringify(payload));
    return true;
  } catch (e) {
    console.log('[TMWD-WS] send failed:', e.message);
    return false;
  }
}

function sendWs(payload) {
  return socketSend(ws, payload);
}

// --- 自更新检测 ---
// 扩展是装载目录里的文件副本：磁盘改了 Chrome 仍执行旧 Service Worker，
// 表现就是「代码明明修好了，控制台报错照旧」。这里让扩展自己盯住 background.js
// 的内容哈希，发现变化就在空闲时自我重载，不用每次手动点「重新加载」。
const SELF_CHECK_ALARM = 'tmwd-self-check';
const SELF_BUILD_KEY = 'appliedBuildHash';
let commandInFlight = 0;

async function computeOwnBuildHash() {
  const url = `${chrome.runtime.getURL('background.js')}?ts=${Date.now()}`;
  const res = await fetch(url, { cache: 'no-store' });
  if (!res.ok) return null;
  const text = await res.text();
  return { hash: await sha256Hex(text), length: text.length };
}

async function sha256Hex(text) {
  const buf = new TextEncoder().encode(text);
  const digest = await crypto.subtle.digest('SHA-256', buf);
  return Array.from(new Uint8Array(digest))
    .map(byte => byte.toString(16).padStart(2, '0'))
    .join('');
}

async function handleSelfCheck() {
  const state = await chrome.storage.local.get({ [SELF_BUILD_KEY]: null, selfCheckAt: 0 });
  const alarm = await chrome.alarms.get(SELF_CHECK_ALARM).catch(() => null);
  const out = {
    storedHash: state[SELF_BUILD_KEY],
    lastCheckAt: state.selfCheckAt || 0,
    alarmScheduled: !!alarm,
    alarmPeriodMinutes: alarm?.periodInMinutes ?? null,
    commandInFlight,
    lastCommandAt,
    manifestVersion: chrome.runtime.getManifest().version,
  };
  try {
    const fetched = await computeOwnBuildHash();
    out.fetchedHash = fetched?.hash ?? null;
    out.fetchedLength = fetched?.length ?? null;
    out.changed = !!state[SELF_BUILD_KEY] && state[SELF_BUILD_KEY] !== out.fetchedHash;
  } catch (e) {
    out.fetchError = e.message;
  }
  return { ok: true, data: out };
}

async function checkSelfUpdate({ force = false } = {}) {
  // 命令执行中重载会掐断正在跑的操作，只在真正空闲时做。
  if (commandInFlight > 0 || Date.now() - lastCommandAt < 5000) return;
  const now = Date.now();
  const state = await chrome.storage.local.get({ [SELF_BUILD_KEY]: null, selfCheckAt: 0 });
  if (!force && now - (state.selfCheckAt || 0) < 55000) return;
  await chrome.storage.local.set({ selfCheckAt: now });
  let hash = null;
  try {
    const fetched = await computeOwnBuildHash();
    hash = fetched?.hash ?? null;
  } catch (e) {
    return;
  }
  if (!hash) return;
  if (!state[SELF_BUILD_KEY]) {
    // 手动重载后的第一轮只记录基线，不触发重载，避免来回抖动。
    await chrome.storage.local.set({ [SELF_BUILD_KEY]: hash });
    return;
  }
  if (state[SELF_BUILD_KEY] === hash) return;
  console.log('[TMWD] background.js 已更新，自动重载扩展');
  await chrome.storage.local.set({ [SELF_BUILD_KEY]: hash });
  chrome.runtime.reload();
}


function normalizePort(port) {
  const value = Number(port);
  if (!Number.isInteger(value) || value < 1 || value > 65535) {
    throw new Error('端口必须是 1-65535');
  }
  if (value === CLI_API_PORT) {
    throw new Error('18767 是 agent-browser-cli API 端口，请换一个插件端口');
  }
  return value;
}

async function loadWsPort() {
  const data = await chrome.storage.local.get({ wsPort: DEFAULT_WS_PORT });
  wsPort = normalizePort(data.wsPort);
  return wsPort;
}

async function saveWsPort(port) {
  wsPort = normalizePort(port);
  await chrome.storage.local.set({ wsPort });
}

async function loadWsToken() {
  const data = await chrome.storage.local.get({ wsToken: '' });
  // daemon 侧 `token_value()` 会 trim，这里保持同一口径：
  // 否则「文件末尾多一个换行」这种最常见的手抄错误会变成 401，且完全看不出原因。
  wsToken = String(data.wsToken || '').trim();
  return wsToken;
}

function getWsUrl() {
  return `ws://127.0.0.1:${wsPort}`;
}

/// 真正用于握手的 URL。
/// **只有配置了 token 才追加查询串**；没有 token 时返回值与 getWsUrl() 完全一致，
/// 这样日志、status、既有测试、popup 展示都不受影响（默认行为零变化）。
function getWsConnectUrl() {
  return wsToken ? `${getWsUrl()}/?token=${encodeURIComponent(wsToken)}` : getWsUrl();
}

/// 脱敏：token 绝不能进 console / status / popup —— 日志经常被贴进 issue。
function maskWsUrl(url) {
  return String(url || '').replace(/([?&]token=)[^&]*/gi, '$1***');
}

const WS_LAST_ERROR_KEY = 'lastConnectError';

/// 记录「最后一次连接失败」，供 popup 显示。
/// 同一 (kind, message, hadToken) 连续出现时只更新计数与时间，不重复落盘
/// —— daemon 没起来时 probe 是 ~1s 一次，否则会把 storage 写爆。
async function recordWsConnectError(kind, message, extra) {
  const entry = Object.assign({
    at: Date.now(),
    kind,
    message: String(message || ''),
    hadToken: !!wsToken,
    url: maskWsUrl(getWsConnectUrl())
  }, extra || {});
  const fingerprint = `${entry.kind}|${entry.message}|${entry.hadToken}`;
  const repeated = lastConnectError && lastConnectError.fingerprint === fingerprint;
  entry.fingerprint = fingerprint;
  entry.repeats = repeated ? (Number(lastConnectError.repeats) || 0) + 1 : 0;
  lastConnectError = entry;
  wsConnectErrorHydrated = true;
  if (repeated) return;
  try {
    await chrome.storage.local.set({ [WS_LAST_ERROR_KEY]: entry });
  } catch (_) {}
}

/// 连上之后清掉失败记录，避免 popup 里挂着一条已经过期的错误。
async function clearWsConnectError() {
  // 内存为空但还没从 storage 恢复过 → storage 里可能有旧记录，照样要清。
  const maybeStored = lastConnectError || !wsConnectErrorHydrated;
  lastConnectError = null;
  wsConnectErrorHydrated = true;
  if (!maybeStored) return;
  try {
    await chrome.storage.local.set({ [WS_LAST_ERROR_KEY]: null });
  } catch (_) {}
}

/// SW 重启后内存里的 lastConnectError 是空的，从 storage 恢复一次（只读一次）。
async function hydrateWsConnectError() {
  if (wsConnectErrorHydrated) return lastConnectError;
  wsConnectErrorHydrated = true;
  try {
    const data = await chrome.storage.local.get({ [WS_LAST_ERROR_KEY]: null });
    if (!lastConnectError && data[WS_LAST_ERROR_KEY]) lastConnectError = data[WS_LAST_ERROR_KEY];
  } catch (_) {}
  return lastConnectError;
}

/// 对外（status / popup）只暴露可读字段，内部用来去重的 fingerprint 不外泄。
function publicWsConnectError(entry) {
  if (!entry) return null;
  const out = Object.assign({}, entry);
  delete out.fingerprint;
  return out;
}

/// 把 WebSocket close 事件翻译成一句人话，重点区分「token 不匹配」这种静默失败。
function describeWsClose(event) {
  const code = event && event.code;
  const reason = String((event && event.reason) || '').trim();
  if (reason) return reason;
  if (code === 1006) {
    return wsToken
      ? '握手被拒绝（HTTP 401/403 的典型表现）：token 可能与 ws-trust.json 不一致，或该扩展 ID 不在 extension_ids 白名单；也可能是 daemon 未启动'
      : '握手被拒绝或连接被异常关闭（daemon 未启动 / 扩展 ID 未被信任）；若 daemon 已启用 token，请在扩展里填写 token';
  }
  if (code === 1000) return '连接被正常关闭（daemon 退出或重启）';
  return code ? `连接关闭（code ${code}）` : '连接关闭';
}

/// 上一次真正开始尝试建连的时刻（connectWS 里写）。probe 用它避免重复排期。
let lastConnectAttemptAt = 0;
/// 单次握手最长等多久。超过就当这条 socket 已经废了（见 keepalive 的三态处理）。
const WS_CONNECT_TIMEOUT_MS = 15000;
/// alarms 的硬下限：Chrome 120+ 会把 delayInMinutes/periodInMinutes 抬到 ≥30s，
/// 传 0.017（~1s）也只会变成 30s。所以 probe 的真实间隔是 30~60s，不是注释里曾宣称的 1s。
const ALARM_MIN_MINUTES = 0.5;

function scheduleProbe() {
  // chrome.alarms 能跨 SW 回收存活，这是 probe 循环唯一的载体。
  // 真实间隔由 Chrome 的 30s 最小粒度决定（实测 30~60s），不是下面这个数字：
  //   0.25min(15s) / 0.5min(30s) 都会被抬到 ≥30s，注释里不再宣称「1s 一次」。
  // 保护：距上次建连尝试不到 25s 时说明刚试过（keepalive / onStartup / popup 触发），
  // 这一轮不再请求更短的延迟，避免「刚建连又排一个新的 probe」在同一时刻各建一条
  // 连接（daemon 侧双连接）。同名 alarm 的 create 是覆盖语义，不会叠出两个 probe。
  const recentlyAttempted = !!lastConnectAttemptAt && Date.now() - lastConnectAttemptAt < 25000;
  chrome.alarms.create('tmwd-ws-probe', {
    delayInMinutes: recentlyAttempted ? ALARM_MIN_MINUTES : ALARM_MIN_MINUTES / 2
  });
}

function scheduleKeepalive() {
  // 保活 + 断线检测。0.5min 是 alarms 的下限，已经无法更短：
  // 想要真正「<30s 一次」的保活只能靠 offscreen document 或 runtime.connect 端口
  //（本批不改架构，见报告里的建议）。
  chrome.alarms.create('tmwd-ws-keepalive', { delayInMinutes: ALARM_MIN_MINUTES });
}

/// 主动放弃一条 socket：必须先摘掉回调再 close，否则被丢弃的 socket 之后仍可能
/// 触发 onopen/onmessage，闭包继续用 `ws` 之外的引用发消息 —— 那就是「实际连着
/// 却发不出响应」的来源。
function discardSocket(socket, reason) {
  if (!socket) return;
  try {
    socket.onopen = null;
    socket.onmessage = null;
    socket.onclose = null;
    socket.onerror = null;
  } catch (_) { /* 已关闭的 socket 赋值可能抛，忽略 */ }
  try { socket.close(); } catch (_) { /* 同上 */ }
  console.log('[TMWD-WS] 丢弃 socket:', reason);
}

async function isServerAlive() {
  try {
    const ctrl = new AbortController();
    setTimeout(() => ctrl.abort(), 2000);
    await fetch(`http://127.0.0.1:${wsPort}`, { signal: ctrl.signal });
    return true; // Got HTTP response → port is listening
  } catch (e) {
    return false; // Network error (connection refused) or timeout → server not alive
  }
}

chrome.alarms?.onAlarm?.addListener?.(async (alarm) => {
  if (alarm.name === 'tmwd-self-reload') {
    chrome.runtime.reload();
    return;
  }
  if (alarm.name === SELF_CHECK_ALARM) {
    checkSelfUpdate();
    return;
  }
  if (alarm.name.startsWith(DEBUG_DETACH_ALARM_PREFIX)) {
    const tabId = Number(alarm.name.slice(DEBUG_DETACH_ALARM_PREFIX.length));
    const session = debugSessions.get(tabId);
    // 延迟期间可能又被 network/console/Fetch 重新占用，这里必须再判一次用途集合。
    if (session?.attached && !debugSessionInUse(session)) {
      await detachDebugSession(session, session.detachReason || 'delayed-idle');
    }
    return;
  }
  if (alarm.name === 'tmwd-ws-keepalive') {
    // 三态处理，不能再用「!== OPEN 就算断」：
    //   OPEN(1)       → 真连着：ping + 继续保活；
    //   CONNECTING(0) → 握手还没回来。既不是「断了」也不是「可用」：以前这里直接
    //                   ws=null + detachAllDebugSessions，把这条 socket 变成孤儿
    //                   —— 它的 onopen/onmessage 闭包仍在跑，daemon 也能收到 ext_ready，
    //                   但全局 ws 已经没了 → 「实际连着却发不出任何响应」，下一次 probe
    //                   还会再建一条并行连接。所以这里只观察、不清空、不 detach；
    //                   握手超时（WS_CONNECT_TIMEOUT_MS）才当失败处理。
    //   CLOSING/CLOSED/null → 真的没了：清空并重连。
    const socket = ws;
    const readyState = socket ? socket.readyState : WebSocket.CLOSED;
    if (readyState === WebSocket.OPEN) {
      sendWs('{"type":"ping"}');
      scheduleKeepalive();
      return;
    }
    if (readyState === WebSocket.CONNECTING) {
      const waited = Date.now() - (lastConnectAttemptAt || 0);
      if (waited < WS_CONNECT_TIMEOUT_MS) {
        // 只延长一次等待：保活周期本身就是 30s，下一拍再看即可。
        console.log('[TMWD-WS] keepalive: 仍在握手，继续等待', waited + 'ms');
        scheduleKeepalive();
        return;
      }
      console.warn('[TMWD-WS] keepalive: 握手超时，丢弃这条 socket', waited + 'ms');
    }
    // 到这里可能是：CLOSING/CLOSED、null，或握手超时的 CONNECTING。
    discardSocket(socket, readyState === WebSocket.CONNECTING ? 'connect-timeout' : 'keepalive-lost');
    ws = null;
    detachAllDebugSessions('keepalive-lost').finally(() => scheduleProbe());
  }
  if (alarm.name === 'tmwd-ws-probe') {
    if (ws && ws.readyState <= 1) return; // Already connected/connecting
    if (await isServerAlive()) {
      console.log('[TMWD-WS] Server detected, connecting...');
      connectWS();
    } else {
      scheduleProbe(); // Server not up, keep probing
    }
  }
});

/**
 * 注入返回 null 时，区分「页面根本不许注入」和「能注入但 eval 被 CSP 禁掉」。
 * 这两种情况的修复方式完全不同，笼统报一句 CSP 只会让人瞎猜。
 */
async function describeInjectFailure(tabId) {
  try {
    const probe = await chrome.scripting.executeScript({
      target: { tabId },
      world: 'MAIN',
      func: () => 1
    });
    if (probe?.[0]?.result === 1) {
      return '页面 CSP 禁止 eval（unsafe-eval 未放行），exec 无法在 MAIN world 执行；需要 CDP';
    }
    return 'executeScript 能调用但页面脚本没有返回结果（页面可能冻结或导航中）';
  } catch (e) {
    return 'executeScript 被拒绝: ' + e.message;
  }
}

async function handleWsExec(data, socket = ws) {
  const tabId = data.tabId;
  console.log('[TMWD-WS] Exec request', data.id, 'on tab', tabId);
  if (!socketSend(socket, { type: 'ack', id: data.id })) {
    console.log('[TMWD-WS] socket closed before ack, drop command', data.id);
    return;
  }
  if (!tabId) {
    socketSend(socket, { type: 'error', id: data.id, error: 'No tabId provided' });
    return;
  }
  const codeText = String(data.code || '');
  // 只在脚本可能触发原生弹窗/新标签页时才付额外开销：
  // 之前每条 exec 都固定做 2 次弹窗抑制注入 + 200ms 新标签宽限，隐藏标签页里尤其浪费。
  // 指定了 frame 时禁止 CDP 回退：CDP 那条路径永远跑在主框架，
  // 会把「frame 不存在」的失败悄悄变成「在主框架执行成功」。
  const hasFrameTarget = !!(data.frameId !== undefined || data.frame_id !== undefined || data.allFrames === true || data.all_frames === true);
  const needsDialogGuard = /\b(alert|confirm|prompt)\s*\(/.test(codeText);
  const mayOpenTab = /window\.open\s*\(|target\s*=\s*["']?_blank|\.click\s*\(/.test(codeText);
  // Use onCreated listener to reliably capture new tabs (avoids race condition with query-diff)
  const newTabIds = new Set();
  const onCreated = (tab) => { newTabIds.add(tab.id); };
  chrome.tabs?.onCreated?.addListener?.(onCreated);
  if (needsDialogGuard) await setDialogSuppressionByScripting(tabId, true);
  try {
    let res;
    let injectError = null;
    try {
      const result = await chrome.scripting.executeScript({
        target: buildScriptTarget(tabId, data),
        world: data.world === 'ISOLATED' ? 'ISOLATED' : 'MAIN',
        func: async (s) => await eval(s),
        args: [buildPageScript(codeText)]
      });
      res = result[0]?.result;
      if (res === null || res === undefined) {
        console.log('[TMWD-WS] executeScript returned null/undefined, treating as CSP issue');
        injectError = await describeInjectFailure(tabId);
        res = { ok: false, error: { name: 'Error', message: injectError, stack: '' }, csp: true };
      }
    } catch (e) {
      console.log('[TMWD-WS] scripting.executeScript failed:', e.message);
      injectError = e.message || String(e);
      res = { ok: false, error: { name: e.name || 'Error', message: injectError, stack: e.stack || '' }, csp: true };
    }
    // CDP fallback for CSP-restricted pages
    if (res && !res.ok && res.csp && !hasFrameTarget) {
      console.log('[TMWD-WS] CDP fallback for tab', tabId);
      const wrappedCode = buildCdpScript(codeText);
      try {
        const session = getDebugSession(tabId);
        await ensureDebugAttached(session);
        if (needsDialogGuard) await setDialogSuppressionByCdp(tabId, true);
        const cdpRes = await chrome.debugger.sendCommand({ tabId }, 'Runtime.evaluate', {
          expression: wrappedCode, awaitPromise: true, returnByValue: true
        });
        if (needsDialogGuard) await setDialogSuppressionByCdp(tabId, false);
        scheduleDebugDetach(session, 'ws-exec-idle');
        if (cdpRes.exceptionDetails) {
          const desc = cdpRes.exceptionDetails.exception?.description || 'CDP Error';
          res = { ok: false, error: { name: 'Error', message: desc, stack: desc } };
        } else {
          res = cdpRes.result.value;
        }
      } catch (cdpErr) {
        if (needsDialogGuard) { try { await setDialogSuppressionByCdp(tabId, false); } catch (_) {} }
        scheduleDebugDetach(getDebugSession(tabId), 'ws-exec-error');
        // 把首次注入的真实错误一起带出来：只报"CDP fallback failed"会让人误以为是 CDP 的问题，
        // 实际根因往往在 executeScript 那一步。
        const parts = ['CDP fallback failed: ' + cdpErr.message];
        if (injectError) parts.push('inject failed: ' + injectError);
        res = { ok: false, error: { name: 'Error', message: parts.join(' | '), stack: '' } };
      }
    }
    // 只有脚本里出现"可能新建标签页"的动作时才等待，避免每条命令白付 200ms。
    if (newTabIds.size === 0 && mayOpenTab) await new Promise(r => setTimeout(r, 120));
    chrome.tabs?.onCreated?.removeListener?.(onCreated);
    // Get full info for captured new tabs
    const newTabs = [];
    for (const id of newTabIds) {
      try { const t = await chrome.tabs.get(id); newTabs.push({id: t.id, url: t.url, title: t.title}); } catch (_) {}
    }
    if (res?.ok) {
      socketSend(socket, { type: 'result', id: data.id, result: res.data ?? null, newTabs });
    } else {
      console.log(res);
      socketSend(socket, { type: 'error', id: data.id, error: res?.error || 'Unknown error', newTabs });
    }
  } catch (e) {
    socketSend(socket, { type: 'error', id: data.id, error: { name: e.name || 'Error', message: e.message || String(e), stack: e.stack || '' } });
  } finally {
    if (needsDialogGuard) await setDialogSuppressionByScripting(tabId, false);
    chrome.tabs?.onCreated?.removeListener?.(onCreated);
  }
}

async function connectWS() {
  // loadWsPort 内部 normalizePort 会 throw：storage 里的 wsPort 一旦被写坏（比如手工
  // 改成 "abc"），旧代码会让 connectWS 直接 reject，而所有调用点只 catch 记日志 →
  // 没有任何路径重新 scheduleProbe → 永久断连且不自愈。这里必须自己兜住。
  try {
    await loadWsPort();
  } catch (e) {
    console.warn('[TMWD-WS] wsPort 配置无效，回退默认端口', DEFAULT_WS_PORT, ':', e && e.message);
    wsPort = DEFAULT_WS_PORT;
    try {
      await recordWsConnectError('bad-port', `wsPort 配置无效，已回退 ${DEFAULT_WS_PORT}: ${(e && e.message) || e}`);
    } catch (_) {}
    // 顺手把脏值修正掉，避免每次启动都在降级路径上打转。
    try { await chrome.storage.local.set({ wsPort: DEFAULT_WS_PORT }); } catch (_) {}
    // 兜底：即使后续任何一步再失败，probe 循环也不会断。
    scheduleProbe();
  }
  try {
    await loadWsToken();
  } catch (e) {
    // token 读失败按「未配置」处理：连接本身仍可继续（兼容模式）。
    console.warn('[TMWD-WS] wsToken 读取失败，按未配置处理:', e && e.message);
    wsToken = '';
  }
  if (ws && ws.readyState <= 1) return; // CONNECTING or OPEN
  // 记录「真正开始尝试建连」的时刻：scheduleProbe 用它避免刚试过又立刻排一轮，
  // keepalive 用它判断一次握手是否已经等太久（的 CONNECTING 分支）。
  lastConnectAttemptAt = Date.now();
  if (!(await isServerAlive())) {
    console.warn('[TMWD-WS] Server not ready, retrying later:', maskWsUrl(getWsConnectUrl()));
    ws = null;
    await recordWsConnectError('server-down', 'daemon 未在该端口监听（或探测超时）');
    scheduleProbe();
    return;
  }
  const wsUrl = getWsConnectUrl();
  console.log('[TMWD-WS] Connecting to', maskWsUrl(wsUrl));
  let socket;
  try {
    socket = new WebSocket(wsUrl);
  } catch (e) {
    console.error('[TMWD-WS] Constructor error:', e);
    ws = null;
    await recordWsConnectError('constructor', (e && e.message) || String(e));
    scheduleProbe();
    return;
  }
  ws = socket;
  socket.onopen = async () => {
    console.log('[TMWD-WS] Connected!');
    // daemon 在 accept 之后就在等 ext_ready：这里任何一步抛错都不能让它永不发出，
    // 否则 daemon 侧所有命令超时、扩展侧 popup 还显示「已连接」，故障极难定位。
    const warnings = [];
    let tabs = [];
    let fileSchemeAccess = false;
    try {
      try { await clearWsConnectError(); } catch (e) { warnings.push('clearConnectError: ' + ((e && e.message) || e)); }
      try { scheduleKeepalive(); } catch (e) { warnings.push('scheduleKeepalive: ' + ((e && e.message) || e)); }
      try { await ensureClientIdentity(); } catch (e) { warnings.push('clientIdentity: ' + ((e && e.message) || e)); }
      try { tabs = await queryScriptableTabs(); } catch (e) { warnings.push('queryTabs: ' + ((e && e.message) || e)); }
      try { fileSchemeAccess = await isAllowedFileSchemeAccess(); } catch (e) { warnings.push('fileSchemeAccess: ' + ((e && e.message) || e)); }
      try { await ensureRestartSeq(); } catch (e) { warnings.push('restartSeq: ' + ((e && e.message) || e)); }
      tabs = filterReportableTabs(tabs);
    } finally {
      // finally 里无论如何都发一次 ext_ready；拿不到的信息用默认值/空数组。
      socketSend(socket, withClientIdentity({
        type: 'ext_ready',
        extension_version: extensionVersion,
        file_scheme_access: fileSchemeAccess,
        tabs: tabs.map(t => ({ id: t.id, url: t.url, title: t.title })),
        restartSeq: restartSeq || 0,
        warnings
      }));
      console.log('[TMWD-WS] Sent ext_ready with', tabs.length, 'tabs',
        warnings.length ? ('warnings: ' + warnings.join(' | ')) : '');
    }
  };
  socket.onmessage = async (event) => {
    let data;
    try {
      data = JSON.parse(event.data);
    } catch (e) {
      console.error('[TMWD-WS] message parse error', e);
      return;
    }
    if (!data || !data.id) return;
    if (!data.code) {
      // 有 id 但没有 code：旧代码直接 return → daemon 空等到超时，扩展侧毫无痕迹。
      // 必须回一条显式失败。用协议里既有的 {type:'error', id, error} 形状：
      // daemon 的 WsIncoming::Result 要求 result 字段必填，只带 ok:false 的 "result"
      // 会被 serde 归到 Other，等于还是没回复。
      socketSend(socket, { type: 'error', id: data.id, error: 'code 缺失或无效' });
      return;
    }
    commandInFlight += 1;
    try {
      lastCommandAt = Date.now();
      let code = data.code;
      // If code is a JSON string representing an object, parse it
      if (typeof code === 'string') {
        try { const p = JSON.parse(code); if (p && typeof p === 'object') code = p; } catch (_) {}
      }
      if (typeof code === 'object' && code !== null && code.cmd) {
        // Custom protocol message → route to handleExtMessage
        if (code.tabId === undefined && data.tabId !== undefined) code.tabId = data.tabId;
        const res = await handleExtMessage(code, {});
        socketSend(socket, { type: res.ok ? 'result' : 'error', id: data.id, result: res.data ?? res.results ?? res, error: res.error });
      } else if (typeof code === 'string') {
        // Plain JS code（把当前 socket 传进去，避免 await 之后读到已被置空的全局 ws）
        await handleWsExec(data, socket);
      } else if (typeof code === 'object' && code !== null) {
        // Object without cmd → legacy extension message
        const msg = code.tabId === undefined && data.tabId !== undefined ? { ...code, tabId: data.tabId } : code;
        const res = await handleExtMessage(msg, {});
        socketSend(socket, { type: res.ok ? 'result' : 'error', id: data.id, result: res.data ?? res.results ?? res, error: res.error });
      }
    } catch (e) {
      console.error('[TMWD-WS] message handler error', e);
      socketSend(socket, { type: 'error', id: data.id, error: { name: e.name || 'Error', message: e.message || String(e), stack: e.stack || '' } });
    } finally {
      commandInFlight = Math.max(0, commandInFlight - 1);
    }
  };
  socket.onclose = async (event) => {
    console.log('[TMWD-WS] Disconnected');
    // 只在"当前连接"仍是这个 socket 时清空全局；否则会把新连接误置空。
    if (ws === socket) ws = null;
    // 1006 基本上是握手阶段就被拒（token 不对 / 扩展未信任 / daemon 重启中），
    // 这条信息是用户唯一的线索，务必记下来（URL 已脱敏）。
    await recordWsConnectError('closed', describeWsClose(event), {
      code: event && event.code !== undefined ? event.code : null,
      wasClean: !!(event && event.wasClean)
    });
    // daemon crash/kill 时 server.rs 无法发送 debugClearAll，扩展必须自行释放 CDP attach。
    await detachAllDebugSessions('ws-close');
    scheduleProbe();
  };
  socket.onerror = (e) => {
    console.warn('[TMWD-WS] Connection warning:', e);
    // onclose will fire after this, which triggers reconnect
  };
}

function reconnectWS() {
  if (ws) {
    try { ws.onclose = null; ws.close(); } catch (_) {}
  }
  ws = null;
  chrome.alarms.clear('tmwd-ws-probe');
  chrome.alarms.clear('tmwd-ws-keepalive');
  connectWS().catch((e) => console.warn('[TMWD-WS] reconnect failed:', e && e.message));
}

// token 变更后立即重建连接（走 storage.onChanged，popup/控制台/未来 CLI 都走同一条路）。
// 只有值真的变了才重连，避免「重复写同一个 token」把 probe 循环打崩。
chrome.storage?.onChanged?.addListener?.((changes, area) => {
  if (area !== 'local' || !changes || !changes.wsToken) return;
  const next = String(changes.wsToken.newValue || '').trim();
  if (next === wsToken) return;
  wsToken = next;
  console.log('[TMWD-WS] wsToken 已' + (wsToken ? '设置' : '清空') + '，重建连接');
  reconnectWS();
});

// Initial connect + wake-up hooks
// 这些都是「启动顺手做的事」，失败不能把 SW 整个带下去：统一吞掉 rejection 只记日志。
const swallow = (label) => (e) => console.warn(`[TMWD] ${label} failed:`, e && e.message);
// SW 每次求值都 +1：随 ext_ready 上报，daemon 靠它识别扩展重启。
ensureRestartSeq().catch(swallow('restartSeq'));
// 冷启动恢复：debugSessions 是内存态，但浏览器的 Fetch 拦截仍然生效。
// 必须在 connectWS 之外独立跑，且失败不影响后续启动流程。
restoreDebugSessions().catch(swallow('restoreDebugSessions'));
connectWS().catch(swallow('connectWS'));
syncPersistBoot().catch(swallow('syncPersistBoot'));
injectContentScriptsIntoExistingTabs().catch(swallow('injectContentScripts'));
// 每分钟比对一次 background.js 的内容哈希：磁盘有改动就在空闲时自我重载。
chrome.alarms?.create?.(SELF_CHECK_ALARM, { periodInMinutes: 1 });
checkSelfUpdate({ force: true }).catch(swallow('checkSelfUpdate'));
chrome.runtime?.onStartup?.addListener?.(() => {
  connectWS().catch(swallow('connectWS'));
  injectContentScriptsIntoExistingTabs().catch(swallow('injectContentScripts'));
});
chrome.runtime?.onInstalled?.addListener?.(() => {
  connectWS().catch(swallow('connectWS'));
  injectContentScriptsIntoExistingTabs().catch(swallow('injectContentScripts'));
});

// Sync tab list on changes
async function sendTabsUpdate() {
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  const socket = ws;
  // 与 ext_ready 用同一个过滤函数：两处口径不一致会让 daemon 看到 tab「先出现后消失」。
  const tabs = filterReportableTabs(await queryScriptableTabs());
  await ensureClientIdentity();
  socketSend(socket, withClientIdentity({
    type: 'tabs_update',
    extension_version: extensionVersion,
    file_scheme_access: await isAllowedFileSchemeAccess(),
    tabs: tabs.map(t => ({ id: t.id, url: t.url, title: t.title }))
  }));
}
// Sync tab list on changes。
// 这里**不做 setTimeout 去抖**：定时器只在内存里，SW 一被回收这次更新就
// 永久丢失，daemon 侧的 tab 列表会一直停在旧状态（表现为「刚打开的标签页看不到」）。
// tabs 事件本身不密集（一次导航最多 onCreated/onUpdated/onRemoved 几条），用
// 「同时只跑一个、期间有新事件就在结束后补一次」替代，既不刷屏也不会丢。
let tabsUpdateRunning = false;
let tabsUpdatePending = false;
function scheduleTabsUpdate() {
  if (tabsUpdateRunning) {
    tabsUpdatePending = true;
    return;
  }
  tabsUpdateRunning = true;
  sendTabsUpdate()
    .catch(swallow('sendTabsUpdate'))
    .finally(() => {
      tabsUpdateRunning = false;
      if (tabsUpdatePending) {
        tabsUpdatePending = false;
        scheduleTabsUpdate();
      }
    });
}
// 持久注入：每次主文档提交后重放（MV3 不能注册内联代码，只能这样"补钩子"）。
// webNavigation 缺失时整段跳过：没有它只是「补钩子晚一点」，不该让 SW 求值阶段崩掉。
if (chrome.webNavigation?.onCommitted?.addListener) {
  chrome.webNavigation.onCommitted.addListener(async (details) => {
    if (details.frameId !== 0) return;
    // boot 脚本已经覆盖 document_start 时不再重复执行，避免 hook 被套两层。
    if (persistBootReady) return;
    // persistBootReady 只是内存标志，SW 冷启动窗口内它是 false，而 boot 脚本可能
    // 已经在跑 → 这里再重放一次就会执行两遍。用「本次导航」的一次性去重挡住。
    if (!(await claimPersistInjection(details.tabId, details.frameId, details.url))) return;
    const store = await chrome.storage.local.get({ persistInjections: {} });
    const map = store.persistInjections || {};
    for (const [name, entry] of Object.entries(map)) {
      if (!matchesPersistPattern(entry.matches, details.url)) continue;
      try {
        const outcome = await runUserCodeInTab(details.tabId, entry.code, entry.world || 'MAIN', { tabId: details.tabId });
        if (!outcome.ok) console.log('[TMWD] persist reinject failed:', name, outcome.error);
      } catch (e) {
        console.log('[TMWD] persist reinject failed:', name, e.message);
      }
    }
  });
} else {
  console.warn('[TMWD] webNavigation.onCommitted 不可用：持久注入只能靠 document_start 的 persist_boot.js（manifest 需要 "webNavigation" 权限）');
}

chrome.tabs?.onUpdated?.addListener?.((_, changeInfo) => {
  if (changeInfo.status === 'complete') scheduleTabsUpdate();
});
chrome.tabs?.onRemoved?.addListener?.((tabId) => {
  // 标签关闭时 Chrome 会自动 detach（onDetach 会到），这里只需要把会话状态从表里摘掉：
  // 否则 debugSessions 会为每个关闭过的标签留一份 requests/logs，越积越大。
  chrome.alarms?.clear?.(debugDetachAlarmName(tabId));
  debugSessions.delete(tabId);
  scheduleTabsUpdate();
});
chrome.tabs?.onCreated?.addListener?.(() => scheduleTabsUpdate());

// 启动自检：可选 API 缺失时明确报出来，避免「popup 只显示未连接、毫无线索」。
warnMissingApis();

function warnMissingApis() {
  const missing = [
    ['debugger', chrome.debugger?.attach],
    ['webNavigation', chrome.webNavigation?.onCommitted],
    ['scripting', chrome.scripting?.executeScript],
    ['downloads', chrome.downloads?.download],
    ['tabs', chrome.tabs?.query],
    ['alarms', chrome.alarms?.create],
    ['management', chrome.management?.getAll],
    ['contentSettings', chrome.contentSettings],
    ['tabGroups', chrome.tabGroups?.update]
  ].filter(([, present]) => !present).map(([name]) => name);
  if (missing.length) {
    console.warn('[TMWD] 以下 chrome API 不可用，相关命令会降级或失败：' + missing.join(', '));
  }
  return missing;
}
