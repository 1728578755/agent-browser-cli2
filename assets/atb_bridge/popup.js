document.addEventListener('DOMContentLoaded', () => {
  const btn = document.getElementById('refresh');
  const savePortBtn = document.getElementById('savePort');
  const saveProfileLabelBtn = document.getElementById('saveProfileLabel');
  const clearProfileLabelBtn = document.getElementById('clearProfileLabel');
  btn.addEventListener('click', fetchCookies);
  savePortBtn.addEventListener('click', savePort);
  saveProfileLabelBtn.addEventListener('click', saveProfileLabel);
  clearProfileLabelBtn.addEventListener('click', clearProfileLabel);
  initReloadButton();
  initWsTokenRow();
  initBadgeToggle();
  initCookieTools();
  refreshBridgeStatus();
  fetchCookies();
});

/**
 * WS token 输入行。
 * popup.html 不在本次可改范围，所以这一行在 JS 里动态创建（样式复用 .row / input），
 * 与端口行保持同样的交互；保存后交给 background 落 storage 并由 storage.onChanged 重连。
 */
function initWsTokenRow() {
  const portMsg = document.getElementById('portMsg');
  if (!portMsg || document.getElementById('wsToken')) return;
  const row = document.createElement('div');
  row.className = 'row';
  const label = document.createElement('label');
  label.textContent = 'Token ';
  const input = document.createElement('input');
  input.id = 'wsToken';
  input.type = 'password';
  input.autocomplete = 'off';
  input.placeholder = '未设置';
  input.style.width = '150px';
  label.appendChild(input);
  const save = document.createElement('button');
  save.id = 'saveWsToken';
  save.textContent = '保存并重连';
  const clear = document.createElement('button');
  clear.id = 'clearWsToken';
  clear.textContent = '清空';
  row.appendChild(label);
  row.appendChild(save);
  row.appendChild(clear);
  const msg = document.createElement('div');
  msg.id = 'wsTokenMsg';
  portMsg.insertAdjacentElement('afterend', row);
  row.insertAdjacentElement('afterend', msg);

  const apply = async (token) => {
    save.disabled = true;
    clear.disabled = true;
    try {
      const resp = await chrome.runtime.sendMessage({ cmd: 'setWsToken', token });
      if (!resp?.ok) throw new Error(resp?.error || 'unknown');
      msg.textContent = resp.data?.wsTokenSet ? 'Success: 已保存 token，正在重连' : 'Success: 已清空 token，正在重连';
      msg.className = 'status';
      // 不在 popup 里回显 token：保存后立刻清空输入框。
      input.value = '';
      await refreshBridgeStatus();
    } catch (e) {
      msg.textContent = '保存失败: ' + e.message;
      msg.className = 'error';
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
 * 为什么默认关：角标是页面里可见的 DOM，站点 querySelector 一下就能判定访客是自动化工具，
 * 与「不被站点检测到自动化」冲突。只有需要肉眼确认连接状态的人才打开。
 * 写入后先靠 content.js 的 storage.onChanged 生效，再给当前活动标签页广播一次兜底。
 */
async function initBadgeToggle() {
  const box = document.getElementById('showBadge');
  const msg = document.getElementById('showBadgeMsg');
  if (!box) return;
  try {
    const got = await chrome.storage.local.get({ showBadge: false });
    box.checked = got.showBadge === true;
  } catch (e) {
    msg.textContent = '读取角标开关失败: ' + e.message;
    msg.className = 'error';
  }
  box.addEventListener('change', async () => {
    const showBadge = box.checked;
    try {
      await chrome.storage.local.set({ showBadge });
    } catch (e) {
      msg.textContent = '保存角标开关失败: ' + e.message;
      msg.className = 'error';
      return;
    }
    msg.textContent = showBadge ? '角标已开启（已打开的标签页即时生效）' : '角标已关闭';
    msg.className = 'status';
    // 拿不到活动标签页（chrome://、扩展页、无活动窗口）时直接忽略：
    // storage.onChanged 已经覆盖了绝大多数页面。
    try {
      const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
      if (tab?.id != null) await chrome.tabs.sendMessage(tab.id, { cmd: 'badgeConfig' });
    } catch (_) {}
  });
}

// popup.html 不在本次可改范围：Cookie 区的「复制」按钮与「包含 httpOnly」开关在 JS 里
// 动态创建（复用 .row / input / button 的既有样式，与端口行保持同样的交互）。
// 关键点：打开 popup **不再自动写剪贴板** —— 会话 cookie（含 httpOnly）进入系统剪贴板后
// 任何本地进程都能读到，必须由用户显式点击才复制。
let lastCookieData = [];
let lastCookieText = '';

function initCookieTools() {
  const out = document.getElementById('out');
  if (!out || document.getElementById('copyCookies')) return;
  const row = document.createElement('div');
  row.className = 'row';
  const copy = document.createElement('button');
  copy.id = 'copyCookies';
  copy.textContent = '复制 cookies';
  const label = document.createElement('label');
  const include = document.createElement('input');
  include.id = 'includeHttpOnly';
  include.type = 'checkbox';
  include.style.width = 'auto';
  label.appendChild(include);
  label.appendChild(document.createTextNode('包含 httpOnly/secure'));
  row.appendChild(copy);
  row.appendChild(label);
  out.insertAdjacentElement('beforebegin', row);
  copy.addEventListener('click', copyCookies);
}

async function copyCookies() {
  const out = document.getElementById('out');
  const include = document.getElementById('includeHttpOnly');
  const includeSensitive = !!(include && include.checked);
  // 默认只复制普通 cookie；httpOnly/secure 必须显式勾选（二者都视作敏感条目）。
  const picked = lastCookieData.filter(c => includeSensitive || !(c.httpOnly || c.secure));
  const skipped = lastCookieData.length - picked.length;
  if (!picked.length) {
    out.textContent = `已复制 0 条${skipped ? `（已跳过 ${skipped} 条 httpOnly/secure）` : ''}\n\n${lastCookieText}`;
    return;
  }
  try {
    await navigator.clipboard.writeText(picked.map(c => `${c.name}=${c.value}`).join('; '));
    const note = `已复制 ${picked.length} 条${includeSensitive ? '（含 httpOnly/secure）' : ''}`
      + (skipped ? `，跳过 ${skipped} 条 httpOnly/secure` : '');
    out.textContent = `${note}\n\n${lastCookieText}`;
  } catch (e) {
    out.textContent = `复制失败: ${e.message}\n\n${lastCookieText}`;
  }
}

// popup 的 HTML/JS 每次打开都从磁盘读取，所以「一键重载」在旧 Service Worker
// 还在跑的时候也能用：点一下即可让浏览器换成磁盘上的最新代码。
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

async function refreshBridgeStatus() {
  const status = document.getElementById('bridgeStatus');
  const portInput = document.getElementById('port');
  try {
    const resp = await chrome.runtime.sendMessage({ cmd: 'status' });
    if (!resp?.ok) throw new Error(resp?.error || 'unknown');
    const data = resp.data || {};
    portInput.value = data.wsPort || 18765;
    // token 只显示「有没有配」，绝不回显原文。
    status.textContent = `状态: ${data.wsConnected ? '已连接' : '未连接'} ${data.wsUrl || ''}`
      + (data.wsTokenSet ? ' / token 已设置' : '');
    renderAuthWarning(data);
    const fileAccessStatus = document.getElementById('fileAccessStatus');
    fileAccessStatus.textContent = `文件网址访问: ${data.fileSchemeAccess ? '已启用' : '未启用'}`;
    fileAccessStatus.className = data.fileSchemeAccess ? 'status' : 'error';
    renderProfileStatus(data);
    renderConnectError(data);
  } catch (e) {
    status.textContent = '状态读取失败: ' + e.message;
    status.className = 'error';
    const fileAccessStatus = document.getElementById('fileAccessStatus');
    fileAccessStatus.textContent = '文件网址权限读取失败: ' + e.message;
    fileAccessStatus.className = 'error';
    const profileStatus = document.getElementById('profileStatus');
    profileStatus.textContent = 'Profile 读取失败: ' + e.message;
    profileStatus.className = 'error';
    renderAuthWarning(null);
    renderConnectError(null);
  }
}

/**
 * 的「提示层」部分（不改分发行为，完整握手需要产品决策）。
 * wsToken 为空 = 扩展对 daemon 没有任何身份校验；而 daemon 默认空闲 300s 就退出，
 * 18765 端口大部分时间是空的 —— 本机任意进程都可能抢注端口并驱动本浏览器。
 */
function renderAuthWarning(data) {
  const existing = document.getElementById('authWarning');
  if (!data || data.wsTokenSet) {
    if (existing) existing.remove();
    return;
  }
  let box = existing;
  if (!box) {
    box = document.createElement('div');
    box.id = 'authWarning';
    box.className = 'error';
    const anchor = document.getElementById('bridgeStatus');
    anchor.insertAdjacentElement('afterend', box);
  }
  box.textContent = '注意：当前未配置 wsToken —— 任何本机进程都可能在 daemon 空闲退出后抢注端口并驱动本浏览器';
}

/**
 * 把「最后一次连接失败」显示出来（仅未连接时）。
 * 没有这条信息时，token/信任配置错误在 popup 里只表现为「未连接」，完全没法自查。
 */
function renderConnectError(data) {
  let box = document.getElementById('wsConnectError');
  const err = data && !data.wsConnected ? data.lastConnectError : null;
  if (!err) {
    if (box) box.remove();
    return;
  }
  if (!box) {
    box = document.createElement('div');
    box.id = 'wsConnectError';
    box.className = 'error';
    const anchor = document.getElementById('fileAccessStatus');
    anchor.insertAdjacentElement('afterend', box);
  }
  const when = err.at ? new Date(err.at).toLocaleTimeString() : '';
  const tokenNote = err.hadToken ? '已带 token' : '未带 token';
  const times = err.repeats ? `，重复 ${err.repeats} 次` : '';
  box.textContent = `连接失败(${when}${when ? '，' : ''}${tokenNote}${times})：${err.message || '未知原因'}`;
}

function renderProfileStatus(data) {
  const profileStatus = document.getElementById('profileStatus');
  const profileLabelInput = document.getElementById('profileLabel');
  const profileId = data.profileId || '-';
  const browserId = data.browserId || '-';
  const label = data.profileLabel || '';
  profileLabelInput.value = label;
  profileStatus.textContent = `Profile: ${label || '(未设置)'} / ${profileId} / ${browserId}`;
  profileStatus.className = 'status';
}

async function saveProfileLabel() {
  const input = document.getElementById('profileLabel');
  await setProfileLabel(input.value);
}

async function clearProfileLabel() {
  const input = document.getElementById('profileLabel');
  input.value = '';
  await setProfileLabel(null);
}

async function setProfileLabel(label) {
  const profileMsg = document.getElementById('profileMsg');
  try {
    const resp = await chrome.runtime.sendMessage({ cmd: 'setProfileLabel', label });
    if (!resp?.ok) throw new Error(resp?.error || 'unknown');
    profileMsg.textContent = `Success: Profile Label ${resp.data?.profileLabel || '已清空'}`;
    profileMsg.className = 'status';
    await refreshBridgeStatus();
  } catch (e) {
    profileMsg.textContent = '保存失败: ' + e.message;
    profileMsg.className = 'error';
  }
}

async function savePort() {
  const portInput = document.getElementById('port');
  const portMsg = document.getElementById('portMsg');
  try {
    const port = Number(portInput.value);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      throw new Error('端口必须是 1-65535');
    }
    if (port === 18767) {
      throw new Error('18767 是 atb API 端口，请换一个插件端口');
    }
    const resp = await chrome.runtime.sendMessage({ cmd: 'setPort', port });
    if (!resp?.ok) throw new Error(resp?.error || 'unknown');
    portMsg.textContent = `Success: 已保存端口 ${port}，正在使用新端口重连`;
    portMsg.className = 'status';
    await refreshBridgeStatus();
  } catch (e) {
    portMsg.textContent = '保存失败: ' + e.message;
    portMsg.className = 'error';
  }
}

async function fetchCookies() {
  const out = document.getElementById('out');
  try {
    const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (!tab?.url) { lastCookieData = []; lastCookieText = 'No active tab'; out.textContent = lastCookieText; return; }
    const resp = await chrome.runtime.sendMessage({ cmd: 'cookies', url: tab.url });
    if (!resp?.ok) { lastCookieData = []; lastCookieText = 'Error: ' + (resp?.error || 'unknown'); out.textContent = lastCookieText; return; }
    if (!resp.data.length) { lastCookieData = []; lastCookieText = '(no cookies)'; out.textContent = lastCookieText; return; }
    // 只展示带标记；复制必须由用户点「复制 cookies」显式触发（见 copyCookies）。
    lastCookieData = resp.data;
    lastCookieText = resp.data.map(c =>
      `${c.name}=${c.value}` + (c.httpOnly ? ' [H]' : '') + (c.secure ? ' [S]' : '') + (c.partitionKey ? ' [P]' : '')
    ).join('\n');
    out.textContent = lastCookieText;
  } catch (e) {
    lastCookieData = [];
    lastCookieText = 'Error: ' + e.message;
    out.textContent = lastCookieText;
  }
}
