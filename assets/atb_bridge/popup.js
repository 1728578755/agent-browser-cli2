// popup 的 UI 逻辑。
//
// 安全前提：
//   * 打开 popup **不再自动拉取并明文渲染 cookie**；只有点「刷新」才请求，
//     且默认只显示 name + [H]/[S]/[P] 标记，值一律 •••；
//   * 「显示值」需要二次点击确认；popup 关闭 / 页面隐藏时立即恢复掩码；
//   * 「复制 cookies」写入系统剪贴板后按 TTL（默认 30s，范围 30..300）自动清除，
//     清除前尽量比对剪贴板内容是否仍是刚写入的文本（无读取权限时退化为盲清，
//     并在 UI 上如实说明）。
//
// 可访问性：状态区 role=status + aria-live=polite，错误区 role=alert；
// 底层 JS 异常先经 friendlyError() 映射成人能读懂的提示，不直接回显裸 message。

const MSG_TTL_OK_MS = 4000;
const COOKIE_TTL_MIN_SEC = 30;
const COOKIE_TTL_MAX_SEC = 300;
const COOKIE_TTL_DEFAULT_SEC = 30;

let lastCookieData = [];
let lastCookieText = '点击「刷新」获取 cookies（默认只显示名称与标记）...';
let cookieRevealed = false;
let revealArmedAt = 0;
let clipboardTimer = null;
let lastCopiedText = '';
let clipboardTtlSec = COOKIE_TTL_DEFAULT_SEC;

const msgTimers = new WeakMap();

/**
 * 把底层异常映射成用户能行动的中文提示。
 * raw message 里带 "Could not establish connection" 这类字眼时，
 * 直接展示等于让用户面对一条 chrome 内部错误，完全无法自查。
 */
function friendlyError(error) {
  const raw = String((error && error.message) || error || '');
  if (!raw) return '未知错误';
  if (/Could not establish connection|Receiving end does not exist/i.test(raw)) {
    return '扩展后台正在重启，请稍后重试，或点「重新加载扩展」';
  }
  if (/message port closed|The message port closed/i.test(raw)) {
    return '扩展后台没有响应，请稍后重试，或点「重新加载扩展」';
  }
  if (/Extension context invalidated|context invalidated/i.test(raw)) {
    return '扩展已更新，请关闭并重新打开这个 popup';
  }
  if (/Cannot access contents of|chrome:\/\/|chrome-extension:\/\//i.test(raw)) {
    return '当前标签页不允许访问（chrome:// 或扩展页），请先切到普通网页';
  }
  if (/No tab with id|Invalid tab ID/i.test(raw)) {
    return '当前标签页已关闭，请重新打开一个网页';
  }
  if (/Clipboard|clipboard/i.test(raw)) {
    return '剪贴板操作被浏览器拒绝（需要焦点与用户手势），请重试';
  }
  if (/^unknown$/i.test(raw.trim())) return '扩展后台返回未知错误';
  return raw;
}

/** 统一的提示写入：成功 4s 后自动清空，错误保留（用户需要读完）。 */
function setMsg(el, text, kind, ttlMs) {
  if (!el) return;
  const previous = msgTimers.get(el);
  if (previous) { clearTimeout(previous); msgTimers.delete(el); }
  el.textContent = text || '';
  el.className = kind === 'error' ? 'error' : 'status';
  // 错误用 alert（屏幕阅读器立即播报），其余用 status（polite）。
  el.setAttribute('role', kind === 'error' ? 'alert' : 'status');
  el.setAttribute('aria-live', kind === 'error' ? 'assertive' : 'polite');
  if (text && kind !== 'error') {
    const ttl = ttlMs > 0 ? ttlMs : MSG_TTL_OK_MS;
    msgTimers.set(el, setTimeout(() => { el.textContent = ''; }, ttl));
  }
}

function clearMsg(el) {
  if (!el) return;
  const previous = msgTimers.get(el);
  if (previous) { clearTimeout(previous); msgTimers.delete(el); }
  el.textContent = '';
}

document.addEventListener('DOMContentLoaded', () => {
  const refreshBtn = document.getElementById('refresh');
  if (refreshBtn) refreshBtn.addEventListener('click', fetchCookies);
  document.getElementById('savePort').addEventListener('click', savePort);
  document.getElementById('saveProfileLabel').addEventListener('click', saveProfileLabel);
  document.getElementById('clearProfileLabel').addEventListener('click', clearProfileLabel);
  // 输入即清旧提示：否则上一次「保存失败」会一直挂在那儿误导用户。
  const portInput = document.getElementById('port');
  if (portInput) portInput.addEventListener('input', () => clearMsg(document.getElementById('portMsg')));
  const labelInput = document.getElementById('profileLabel');
  if (labelInput) labelInput.addEventListener('input', () => clearMsg(document.getElementById('profileMsg')));
  initWidthMode();
  initReloadButton();
  initWsTokenRow();
  initBadgeToggle();
  initCookieTools();
  initClipboardLifecycle();
  refreshBridgeStatus();
  // 注意：这里**不**调用 fetchCookies —— 打开 popup 不产生任何 cookie 读取/渲染。
});

/** 宽度档位：存 storage.local.popupWidthMode，缺省 standard。 */
async function initWidthMode() {
  const select = document.getElementById('widthMode');
  if (!select) return;
  const apply = (mode) => {
    const value = mode === 'compact' || mode === 'wide' ? mode : 'standard';
    document.body.dataset.width = value;
    select.value = value;
    return value;
  };
  let current = 'standard';
  try {
    const got = await chrome.storage.local.get({ popupWidthMode: 'standard' });
    current = apply(got.popupWidthMode);
  } catch (_) {
    current = apply('standard');
  }
  select.addEventListener('change', async () => {
    const mode = apply(select.value);
    if (mode === current) return;
    current = mode;
    try {
      await chrome.storage.local.set({ popupWidthMode: mode });
    } catch (_) {}
  });
}

/**
 * WS token 输入行（popup.html 里预留了 #wsTokenRow 容器）。
 * 保存后交给 background 落 storage 并由 storage.onChanged 重连。
 */
function initWsTokenRow() {
  const host = document.getElementById('wsTokenRow');
  if (!host || document.getElementById('wsToken')) return;
  const row = document.createElement('div');
  row.className = 'row';
  const label = document.createElement('label');
  label.textContent = 'Token ';
  const input = document.createElement('input');
  input.id = 'wsToken';
  input.type = 'password';
  input.autocomplete = 'off';
  input.placeholder = '未设置';
  // 360px 宽度下把「保存并重连 / 清空」作为一组保持在同一行：
  // 旧实现两个按钮直接挂在 .row 上，窄窗口会把「清空」单独挤到下一行，
  // 视觉上像是另一个操作的入口（视觉检查）。
  input.style.width = '120px';
  label.appendChild(input);
  const save = document.createElement('button');
  save.id = 'saveWsToken';
  save.textContent = '保存并重连';
  const clear = document.createElement('button');
  clear.id = 'clearWsToken';
  clear.textContent = '清空';
  const actions = document.createElement('span');
  actions.className = 'btn-group';
  actions.appendChild(save);
  actions.appendChild(clear);
  row.appendChild(label);
  row.appendChild(actions);
  const msg = document.createElement('div');
  msg.id = 'wsTokenMsg';
  msg.className = 'status';
  msg.setAttribute('role', 'status');
  msg.setAttribute('aria-live', 'polite');
  host.appendChild(row);
  host.appendChild(msg);
  input.addEventListener('input', () => clearMsg(msg));

  const apply = async (token) => {
    save.disabled = true;
    clear.disabled = true;
    try {
      const resp = await chrome.runtime.sendMessage({ cmd: 'setWsToken', token });
      if (!resp || !resp.ok) throw new Error((resp && resp.error) || 'unknown');
      // 不在 popup 里回显 token：保存后立刻清空输入框。
      input.value = '';
      await refreshBridgeStatus();
      setMsg(msg, resp.data && resp.data.wsTokenSet
        ? '已保存 token，正在重连'
        : '已清空 token，正在重连', 'status');
    } catch (e) {
      setMsg(msg, '保存失败: ' + friendlyError(e), 'error');
    } finally {
      save.disabled = false;
      clear.disabled = false;
    }
  };
  save.addEventListener('click', () => apply(input.value.trim()));
  clear.addEventListener('click', () => apply(''));
}

/**
 * 「在页面上显示状态角标」开关（storage.local.showBadge，缺省 false）。
 * 默认关：角标是页面里可见的 DOM，站点 querySelector 一下就能判定访客是自动化工具。
 */
async function initBadgeToggle() {
  const box = document.getElementById('showBadge');
  const msg = document.getElementById('showBadgeMsg');
  if (!box) return;
  try {
    const got = await chrome.storage.local.get({ showBadge: false });
    box.checked = got.showBadge === true;
  } catch (e) {
    setMsg(msg, '读取角标开关失败: ' + friendlyError(e), 'error');
  }
  box.addEventListener('change', async () => {
    const showBadge = box.checked;
    try {
      await chrome.storage.local.set({ showBadge });
    } catch (e) {
      setMsg(msg, '保存角标开关失败: ' + friendlyError(e), 'error');
      return;
    }
    setMsg(msg, showBadge ? '角标已开启（已打开的标签页即时生效）' : '角标已关闭', 'status');
    // 拿不到活动标签页（chrome://、扩展页、无活动窗口）时直接忽略：
    // storage.onChanged 已经覆盖了绝大多数页面。
    try {
      const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
      if (tab && tab.id != null) await chrome.tabs.sendMessage(tab.id, { cmd: 'badgeConfig' });
    } catch (_) {}
  });
}

// ---------------------------------------------------------------------------
// Cookie 区
// ---------------------------------------------------------------------------

function initCookieTools() {
  const copy = document.getElementById('copyCookies');
  if (copy) copy.addEventListener('click', copyCookies);
  const toggle = document.getElementById('toggleCookies');
  if (toggle) toggle.addEventListener('click', toggleCookieValues);
  const select = document.getElementById('clipboardTtl');
  if (select) {
    chrome.storage.local.get({ clipboardTtlSec: COOKIE_TTL_DEFAULT_SEC }).then((got) => {
      clipboardTtlSec = clampTtl(got.clipboardTtlSec);
      select.value = String(clipboardTtlSec);
    }).catch(() => { select.value = String(clipboardTtlSec); });
    select.addEventListener('change', async () => {
      clipboardTtlSec = clampTtl(Number(select.value));
      select.value = String(clipboardTtlSec);
      try { await chrome.storage.local.set({ clipboardTtlSec }); } catch (_) {}
      // 已经复制过：按新的 TTL 重新计时，别让旧的长 TTL 继续生效。
      if (lastCopiedText) armClipboardClear();
    });
  }
}

function clampTtl(value) {
  const raw = Math.floor(Number(value));
  if (!Number.isFinite(raw) || raw <= 0) return COOKIE_TTL_DEFAULT_SEC;
  return Math.min(COOKIE_TTL_MAX_SEC, Math.max(COOKIE_TTL_MIN_SEC, raw));
}

/** 「显示值」二次确认：第一次点击只是解锁 5 秒，第二次才真的展开。 */
function toggleCookieValues() {
  const btn = document.getElementById('toggleCookies');
  const msg = document.getElementById('cookieMsg');
  if (!lastCookieData.length) {
    setMsg(msg, '先点「刷新」获取 cookies', 'status');
    return;
  }
  if (cookieRevealed) {
    cookieRevealed = false;
    revealArmedAt = 0;
    if (btn) btn.textContent = '显示值';
    renderCookies();
    setMsg(msg, '已恢复掩码显示', 'status');
    return;
  }
  const now = Date.now();
  if (!revealArmedAt || now - revealArmedAt > 5000) {
    revealArmedAt = now;
    if (btn) btn.textContent = '再次点击确认显示';
    setMsg(msg, '明文会把 cookie 值画到屏幕上（含 httpOnly/secure），5 秒内再点一次确认', 'status', 5000);
    return;
  }
  cookieRevealed = true;
  revealArmedAt = 0;
  if (btn) btn.textContent = '隐藏值';
  renderCookies();
  setMsg(msg, '已显示明文；关闭 popup 会自动恢复掩码', 'status');
}

/** 默认只渲染 name + 标记 + 条数，值用 •••。 */
function renderCookies() {
  const out = document.getElementById('out');
  if (!out) return;
  if (!lastCookieData.length) {
    out.textContent = lastCookieText;
    return;
  }
  const lines = lastCookieData.map((c) => {
    const flags = (c.httpOnly ? ' [H]' : '') + (c.secure ? ' [S]' : '') + (c.partitionKey ? ' [P]' : '');
    const value = cookieRevealed ? String(c.value == null ? '' : c.value) : '•••';
    // 默认只藏**值**：name 与 [H]/[S]/[P] 标记是判断「这条 cookie 该不该复制」的依据。
    return c.name + '=' + value + flags;
  });
  const header = cookieRevealed
    ? `共 ${lastCookieData.length} 条（明文显示中，关闭 popup 恢复掩码）`
    : `共 ${lastCookieData.length} 条（值已隐藏；点「显示值」查看）`;
  out.textContent = header + '\n' + lines.join('\n');
}

/** 关闭 popup / 页面隐藏时恢复掩码（不重新请求，避免多余 IO）。 */
function hideCookieValues() {
  if (!cookieRevealed) return;
  cookieRevealed = false;
  revealArmedAt = 0;
  const btn = document.getElementById('toggleCookies');
  if (btn) btn.textContent = '显示值';
  renderCookies();
}

async function copyCookies() {
  const out = document.getElementById('out');
  const include = document.getElementById('includeHttpOnly');
  const msg = document.getElementById('cookieMsg');
  const includeSensitive = !!(include && include.checked);
  if (!lastCookieData.length) {
    setMsg(msg, '没有可复制的 cookies，先点「刷新」', 'status');
    return;
  }
  // 默认只复制普通 cookie；httpOnly/secure 必须显式勾选（二者都视作敏感条目）。
  const picked = lastCookieData.filter((c) => includeSensitive || !(c.httpOnly || c.secure));
  const skipped = lastCookieData.length - picked.length;
  if (!picked.length) {
    setMsg(msg, `未复制：${skipped} 条全是 httpOnly/secure，请勾选「包含 httpOnly/secure」`, 'status');
    return;
  }
  try {
    lastCopiedText = picked.map((c) => `${c.name}=${c.value}`).join('; ');
    await navigator.clipboard.writeText(lastCopiedText);
    const note = `已复制 ${picked.length} 条${includeSensitive ? '（含 httpOnly/secure）' : ''}`
      + (skipped ? `，跳过 ${skipped} 条 httpOnly/secure` : '')
      + `，${clipboardTtlSec} 秒后自动清除`;
    setMsg(msg, note, 'status', clipboardTtlSec * 1000);
    armClipboardClear();
    if (out) renderCookies();
  } catch (e) {
    setMsg(msg, '复制失败: ' + friendlyError(e), 'error');
  }
}

/** 复制后启动定时清除。重复复制时重置计时。 */
function armClipboardClear() {
  if (clipboardTimer) { clearTimeout(clipboardTimer); clipboardTimer = null; }
  if (!lastCopiedText) return;
  const ttlMs = clampTtl(clipboardTtlSec) * 1000;
  clipboardTimer = setTimeout(() => {
    clipboardTimer = null;
    clearClipboard('timeout');
  }, ttlMs);
}

/**
 * 清除剪贴板里的 cookie 副本。
 *
 * 先读一次做比对：内容已经变了（用户又复制了别的东西）就不再覆盖，避免把用户的
 * 新剪贴板内容清掉。没有 clipboardRead 权限时 readText 会抛错 —— 这种情况下
 * 只能盲清（安全优先），并在 UI 上说明「无法校验」。
 */
async function clearClipboard(reason) {
  const msg = document.getElementById('cookieMsg');
  if (!lastCopiedText) return;
  let verified = false;
  let same = true;
  try {
    if (navigator.clipboard && navigator.clipboard.readText) {
      const current = await navigator.clipboard.readText();
      verified = true;
      same = current === lastCopiedText;
    }
  } catch (_) {
    verified = false;
  }
  if (verified && !same) {
    lastCopiedText = '';
    setMsg(msg, '剪贴板内容已被你替换，未清空（原 cookie 副本已被覆盖）', 'status');
    return;
  }
  try {
    await navigator.clipboard.writeText('');
    lastCopiedText = '';
    setMsg(msg, verified
      ? '已自动清除剪贴板里的 cookies'
      : '已自动清除剪贴板（无法校验内容：缺少剪贴板读取权限）', 'status');
  } catch (e) {
    if (reason === 'timeout') {
      setMsg(msg, '自动清除失败，请手动清空剪贴板: ' + friendlyError(e), 'error');
    }
  }
}

/** pagehide / beforeunload / 隐藏时立刻清一次，别等 TTL。 */
function initClipboardLifecycle() {
  const flush = () => {
    if (lastCopiedText) {
      // unload 阶段异步 Promise 可能来不及完成，仍先发一次（能成功就成功）。
      try { navigator.clipboard.writeText(''); } catch (_) {}
      lastCopiedText = '';
    }
    if (clipboardTimer) { clearTimeout(clipboardTimer); clipboardTimer = null; }
  };
  window.addEventListener('pagehide', () => { flush(); hideCookieValues(); });
  window.addEventListener('beforeunload', flush);
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) hideCookieValues();
  });
}

// ---------------------------------------------------------------------------
// Bridge / Profile 状态
// ---------------------------------------------------------------------------

function initReloadButton() {
  const info = document.getElementById('buildInfo');
  const button = document.getElementById('reloadExt');
  try {
    if (info) info.textContent = `运行版本 ${chrome.runtime.getManifest().version}`;
  } catch (_) {}
  if (!button) return;
  button.addEventListener('click', () => {
    button.disabled = true;
    button.textContent = '正在重载…';
    chrome.runtime.reload();
  });
}

/**
 * 读 background 状态并渲染。
 * 返回 data（失败返回 null）—— savePort / saveWsToken 需要拿到刷新后的真实连接态，
 * 否则「已保存端口，正在重连」会和「状态: 未连接」并排出现。
 */
async function refreshBridgeStatus() {
  const status = document.getElementById('bridgeStatus');
  const portInput = document.getElementById('port');
  try {
    const resp = await chrome.runtime.sendMessage({ cmd: 'status' });
    if (!resp || !resp.ok) throw new Error((resp && resp.error) || 'unknown');
    const data = resp.data || {};
    if (portInput && data.wsPort) portInput.value = data.wsPort;
    // token 只显示「有没有配」，绝不回显原文。
    status.textContent = `状态: ${data.wsConnected ? '已连接' : '未连接'} ${data.wsUrl || ''}`
      + (data.wsTokenSet ? ' / token 已设置' : '');
    status.className = 'status';
    renderAuthWarning(data);
    const fileAccessStatus = document.getElementById('fileAccessStatus');
    if (fileAccessStatus) {
      fileAccessStatus.textContent = `文件网址访问: ${data.fileSchemeAccess ? '已启用' : '未启用'}`;
      fileAccessStatus.className = data.fileSchemeAccess ? 'status' : 'error';
    }
    renderProfileStatus(data);
    renderConnectError(data);
    return data;
  } catch (e) {
    const friendly = friendlyError(e);
    status.textContent = '状态读取失败: ' + friendly;
    status.className = 'error';
    const fileAccessStatus = document.getElementById('fileAccessStatus');
    if (fileAccessStatus) {
      fileAccessStatus.textContent = '文件网址权限读取失败: ' + friendly;
      fileAccessStatus.className = 'error';
    }
    const profileStatus = document.getElementById('profileStatus');
    if (profileStatus) {
      profileStatus.textContent = 'Profile 读取失败: ' + friendly;
      profileStatus.className = 'error';
    }
    renderAuthWarning(null);
    renderConnectError(null);
    return null;
  }
}

/**
 * 认证告警。
 * 旧文案只覆盖「daemon 空闲退出后端口被抢注」，但真正的风险面更大：
 * 兼容档（未配置 wsToken）下 daemon 运行期本就接受任何本机进程的连接，
 * 对方可以直接发命令接管本浏览器，根本不需要等端口空出来。
 */
function renderAuthWarning(data) {
  const box = document.getElementById('authWarning');
  if (!box) return;
  if (!data || data.wsTokenSet) {
    box.hidden = true;
    box.textContent = '';
    return;
  }
  box.hidden = false;
  box.textContent = '当前为兼容档：任何本机进程都可能连入 daemon 并接管本浏览器（不限于空闲窗口）；'
    + '建议执行 atb trust-extension --token 切到 token 档';
}

function renderConnectError(data) {
  const box = document.getElementById('wsConnectError');
  if (!box) return;
  const err = data && !data.wsConnected ? data.lastConnectError : null;
  if (!err) {
    box.hidden = true;
    box.textContent = '';
    return;
  }
  box.hidden = false;
  const when = err.at ? new Date(err.at).toLocaleTimeString() : '';
  const tokenNote = err.hadToken ? '已带 token' : '未带 token';
  const times = err.repeats ? `，重复 ${err.repeats} 次` : '';
  box.textContent = `连接失败(${when}${when ? '，' : ''}${tokenNote}${times})：${err.message || '未知原因'}`;
}

function renderProfileStatus(data) {
  const profileStatus = document.getElementById('profileStatus');
  const profileDetail = document.getElementById('profileDetail');
  const profileLabelInput = document.getElementById('profileLabel');
  const label = data.profileLabel || '';
  if (profileLabelInput) profileLabelInput.value = label;
  if (profileStatus) {
    profileStatus.textContent = `Profile: ${label || '(未设置)'}`;
    profileStatus.className = 'status';
  }
  // 具体 id 放进折叠区：它是本机环境标识，不需要默认摊在屏幕上。
  if (profileDetail) {
    profileDetail.textContent = `id: ${data.profileId || '-'} / browser: ${data.browserId || '-'}`;
  }
}

async function saveProfileLabel() {
  const input = document.getElementById('profileLabel');
  await setProfileLabel(input ? input.value : '');
}

async function clearProfileLabel() {
  const input = document.getElementById('profileLabel');
  if (input) input.value = '';
  await setProfileLabel(null);
}

async function setProfileLabel(label) {
  const msg = document.getElementById('profileMsg');
  try {
    const resp = await chrome.runtime.sendMessage({ cmd: 'setProfileLabel', label });
    if (!resp || !resp.ok) throw new Error((resp && resp.error) || 'unknown');
    await refreshBridgeStatus();
    setMsg(msg, `Profile Label ${(resp.data && resp.data.profileLabel) || '已清空'}`, 'status');
  } catch (e) {
    setMsg(msg, '保存失败: ' + friendlyError(e), 'error');
  }
}

async function savePort() {
  const portInput = document.getElementById('port');
  const msg = document.getElementById('portMsg');
  try {
    const port = Number(portInput ? portInput.value : NaN);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      throw new Error('端口必须是 1-65535');
    }
    if (port === 18767) {
      throw new Error('18767 是 atb API 端口，请换一个插件端口');
    }
    const resp = await chrome.runtime.sendMessage({ cmd: 'setPort', port });
    if (!resp || !resp.ok) throw new Error((resp && resp.error) || 'unknown');
    // 先刷新状态再写文案：旧实现先报「正在使用新端口重连」，随后 refreshBridgeStatus
    // 把状态改成「未连接」，两条信息互相矛盾。
    const data = await refreshBridgeStatus();
    const connected = !!(data && data.wsConnected);
    setMsg(msg, `已保存端口 ${port}，${connected ? '已连接' : '正在重连（当前未连接）'}`, 'status');
  } catch (e) {
    setMsg(msg, '保存失败: ' + friendlyError(e), 'error');
  }
}

async function fetchCookies() {
  const out = document.getElementById('out');
  const msg = document.getElementById('cookieMsg');
  try {
    const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (!tab || !tab.url) {
      lastCookieData = [];
      lastCookieText = '没有活动标签页';
      renderCookies();
      return;
    }
    const resp = await chrome.runtime.sendMessage({ cmd: 'cookies', url: tab.url });
    if (!resp || !resp.ok) {
      lastCookieData = [];
      const friendly = friendlyError(new Error((resp && resp.error) || 'unknown'));
      lastCookieText = '读取失败: ' + friendly;
      renderCookies();
      setMsg(msg, '读取 cookies 失败: ' + friendly, 'error');
      return;
    }
    if (!resp.data || !resp.data.length) {
      lastCookieData = [];
      lastCookieText = '(该站点没有 cookies)';
      renderCookies();
      return;
    }
    lastCookieData = resp.data;
    cookieRevealed = false;
    revealArmedAt = 0;
    const btn = document.getElementById('toggleCookies');
    if (btn) btn.textContent = '显示值';
    renderCookies();
    setMsg(msg, `已读取 ${resp.data.length} 条（值默认隐藏）`, 'status');
  } catch (e) {
    lastCookieData = [];
    lastCookieText = '读取失败: ' + friendlyError(e);
    renderCookies();
    setMsg(msg, '读取 cookies 失败: ' + friendlyError(e), 'error');
  }
}
