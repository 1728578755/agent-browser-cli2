;(function(){ if (/streamlit/i.test(document.title)) return;
if (window.__agentBrowserCliCleanup) window.__agentBrowserCliCleanup();
// 旧版本在这里装过一个追 TID 元素的 MutationObserver（每个 DOM 变更都跑），
// 现在这套通道已经废弃，这里保留一次清理调用，把旧注入留下的 observer 停掉。
if (window.__agentBrowserCliObserverCleanup) window.__agentBrowserCliObserverCleanup();
document.querySelectorAll('#agent-browser-cli-ind,#agent-browser-cli-style').forEach(e => e.remove());

/**
 * Render a right-side floating badge that reflects the real bridge connection status.
 */
(function(){
  if(window.self!==window.top)return;
  const s=document.createElement('style');
  s.id='agent-browser-cli-style';
  s.textContent='#agent-browser-cli-ind{position:fixed;top:40%;right:0;display:flex;align-items:center;gap:10px;width:218px;height:36px;box-sizing:border-box;padding:0 8px 0 9px;color:white;border-radius:18px 0 0 18px;font-size:11px;font-weight:bold;line-height:36px;z-index:99999;cursor:grab;box-shadow:0 4px 12px rgba(0,0,0,0.22);background:#4CAF50;opacity:.72;overflow:visible;white-space:nowrap;user-select:none;transform:translateX(190px);transition:transform .18s ease,opacity .18s ease;}#agent-browser-cli-ind::after{content:"";position:absolute;inset:0;border-radius:18px 0 0 18px;background:#4CAF50;z-index:-1;}#agent-browser-cli-dot{flex:0 0 auto;width:10px;height:10px;border-radius:999px;background:white;}#agent-browser-cli-label{flex:0 0 auto;max-width:150px;overflow:hidden;}#agent-browser-cli-ind[data-expanded="1"]{opacity:.92;transform:translateX(0);}#agent-browser-cli-ind[data-dragging="1"]{cursor:grabbing;transition:none;}#agent-browser-cli-close{position:relative;flex:0 0 auto;width:20px;height:20px;border-radius:999px;line-height:18px;text-align:center;font-size:15px;font-weight:bold;background:#1f7a33;color:white;opacity:0;pointer-events:none;cursor:pointer;}#agent-browser-cli-ind[data-expanded="1"] #agent-browser-cli-close{opacity:1;pointer-events:auto;}#agent-browser-cli-close:hover{background:#145523;}#agent-browser-cli-close::after{content:"本次隐藏";position:absolute;right:0;bottom:28px;padding:4px 7px;border-radius:4px;background:#1f2937;color:white;font-size:12px;font-weight:500;line-height:16px;opacity:0;pointer-events:none;transform:translateY(4px);transition:opacity .12s ease,transform .12s ease;}#agent-browser-cli-close:hover::after{opacity:1;transform:translateY(0);}';
  (document.head||document.documentElement).appendChild(s);
  const d=document.createElement('div');
  d.id='agent-browser-cli-ind';
  const dot=document.createElement('span');
  dot.id='agent-browser-cli-dot';
  const label=document.createElement('span');
  label.id='agent-browser-cli-label';
  label.textContent='agent_browser_cli: 已连接';
  const close=document.createElement('span');
  close.id='agent-browser-cli-close';
  close.textContent='×';
  d.appendChild(dot);
  d.appendChild(label);
  d.appendChild(close);
  let collapseTimer = null;
  let dragging = false;
  let moved = false;
  let dragOffsetY = 0;
  let dismissed = false;
  let wasConnected = false;
  let lastSeenCommandAt = 0;
  let lastTop = Math.round(window.innerHeight * 0.4);
  d.style.top = lastTop + 'px';
  /**
   * Show the badge only when the bridge is really connected.
   */
  function setBadgeState(connected, detail, lastCommandAt) {
    // 手动关闭只在当前连接周期生效；断开后下次重新连接会再次显示。
    if (connected && !wasConnected) dismissed = false;
    wasConnected = connected;
    const commandAt = Number(lastCommandAt) || 0;
    if (commandAt > lastSeenCommandAt) lastSeenCommandAt = commandAt;
    const activeRecently = lastSeenCommandAt > 0 && Date.now() - lastSeenCommandAt <= 10000;
    d.style.display = connected && !dismissed && activeRecently ? 'flex' : 'none';
    label.textContent = 'agent_browser_cli: 已连接';
    d.dataset.connected = connected ? '1' : '0';
    d.dataset.detail = detail || '';
  }
  function setExpanded(expanded) {
    d.dataset.expanded = expanded ? '1' : '0';
  }
  function clampTop(top) {
    return Math.max(8, Math.min(window.innerHeight - d.offsetHeight - 8, top));
  }
  /**
   * Poll background status and update the badge visibility.
   */
  async function refreshBadgeState() {
    try {
      const resp = await chrome.runtime.sendMessage({ cmd: 'status' });
      const connected = !!resp?.ok && !!resp?.data?.wsConnected;
      setBadgeState(connected, resp?.data?.wsUrl || '', resp?.data?.lastCommandAt);
    } catch (e) {
      setBadgeState(false, e.message || '', 0);
    }
    // 状态更新后同步定时器：角标一隐藏就停掉，避免页面可见时空转轮询。
    syncPolling();
  }
  d.addEventListener('mouseenter', () => {
    if (collapseTimer) clearTimeout(collapseTimer);
    setExpanded(true);
  });
  d.addEventListener('mouseleave', () => {
    if (collapseTimer) clearTimeout(collapseTimer);
    setExpanded(false);
  });
  d.addEventListener('pointerdown', (e) => {
    dragging = true;
    moved = false;
    dragOffsetY = e.clientY - d.getBoundingClientRect().top;
    d.dataset.dragging = '1';
    d.setPointerCapture(e.pointerId);
  });
  d.addEventListener('pointermove', (e) => {
    if (!dragging) return;
    const nextTop = clampTop(e.clientY - dragOffsetY);
    if (Math.abs(nextTop - lastTop) > 2) moved = true;
    lastTop = nextTop;
    d.style.top = lastTop + 'px';
  });
  d.addEventListener('pointerup', (e) => {
    dragging = false;
    d.dataset.dragging = '0';
    try { d.releasePointerCapture(e.pointerId); } catch (_) {}
  });
  close.addEventListener('pointerdown', (e) => { e.stopPropagation(); });
  close.addEventListener('click', (e) => {
    e.stopPropagation();
    dismissed = true;
    setExpanded(false);
    d.style.display = 'none';
    // 手动隐藏后立刻停轮询，不必等到下一个 tick。
    stopPolling();
  });
  window.addEventListener('resize', () => {
    lastTop = clampTop(lastTop);
    d.style.top = lastTop + 'px';
  });
  (document.body||document.documentElement).appendChild(d);
  setBadgeState(false, '');
  // 角标状态刷新：从「每 5s 常驻轮询 + 只判断 document.hidden」改成「事件驱动 + 低频定时器」。
  // 原因：每个 top frame 每 5s 发一条 runtime 消息，等于把 MV3 的 Service Worker 钉在常驻状态
  // （SW 30s 无活动就会被回收），多标签用户既费电也让 SW 生命周期失去意义。
  // 现在的策略：
  //   1) 隐藏时完全不轮询；
  //   2) 仅在「页面可见 / 窗口获得焦点 / bfcache 恢复」这几个事件上探一次；
  //   3) 只有角标正在显示时才挂一个 30s 的低频定时器，角标一隐藏立刻停掉。
  const BADGE_POLL_MS = 30000;
  let refreshTimer = null;
  function badgeVisible() {
    return d.style.display !== 'none' && !document.hidden;
  }
  function stopPolling() {
    if (refreshTimer) { clearInterval(refreshTimer); refreshTimer = null; }
  }
  function syncPolling() {
    if (badgeVisible()) {
      if (!refreshTimer) refreshTimer = setInterval(refreshBadgeState, BADGE_POLL_MS);
    } else {
      stopPolling();
    }
  }
  async function probeAndSync() {
    if (document.hidden) { stopPolling(); return; }
    await refreshBadgeState();
  }
  function onVisibilityChange() {
    if (document.hidden) { stopPolling(); return; }
    probeAndSync();
  }
  document.addEventListener('visibilitychange', onVisibilityChange);
  window.addEventListener('focus', probeAndSync);
  window.addEventListener('pageshow', probeAndSync);
  probeAndSync();
  window.__agentBrowserCliCleanup = () => {
    stopPolling();
    document.removeEventListener('visibilitychange', onVisibilityChange);
    window.removeEventListener('focus', probeAndSync);
    window.removeEventListener('pageshow', probeAndSync);
    d.remove();
    s.remove();
  };
})();
})();
