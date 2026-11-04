;(function(){ // 只跳过 Streamlit 的内部端点页（/_stcore/*）。按标题匹配会误伤「标题里带 streamlit」
// 的普通网页（教程、博客），那些页面照样需要角标与状态。
if (/\/_stcore\//i.test(location.href)) return;
// 重注入时先把上一版留下的角标 + 配置监听撤掉，避免同一页面叠出两套 storage 回调。
if (window.__agentBrowserCliCleanup) window.__agentBrowserCliCleanup();
// 旧版本在这里装过一个追 TID 元素的 MutationObserver（每个 DOM 变更都跑），
// 现在这套通道已经废弃，这里保留一次清理调用，把旧注入留下的 observer 停掉。
if (window.__agentBrowserCliObserverCleanup) window.__agentBrowserCliObserverCleanup();
document.querySelectorAll('#atb-ind,#atb-style').forEach(e => e.remove());

/**
 * Render a right-side floating badge that reflects the real bridge connection status.
 *
 * 默认不渲染。历史版本无条件往每个页面插一个固定 id 的角标，
 * 站点只要 querySelector 那个 id 就能判定访客是自动化工具，
 * 这和「别被站点检测到自动化」直接冲突。现在分两层收紧：
 *   1) 只有用户在 popup 里显式勾选 showBadge 才挂载，关闭时连 style/定时器都不创建；
 *   2) 内部结构全部关在 closed shadow root 里，页面外面只剩一个中性 id 的空壳宿主。
 */
(function(){
  if(window.self!==window.top)return;

  // 宿主 id 用中性名字：旧 id 本身就是现成的自动化指纹，不再复用。
  const HOST_ID='atb-status-host';
  // 角标状态刷新策略（E1-15：注释按实现写，不再宣称「纯事件驱动」）。
  //
  // 早期版本每个 top frame 每 5s 无条件发一条 runtime 消息，等于把 MV3 的 Service Worker
  // 钉在常驻状态（30s 无活动才允许回收），多标签用户既费电也让 SW 生命周期失去意义。
  // 现在的行为：
  //   1) 页面隐藏时完全不轮询（visibilitychange 直接把定时器停掉）；
  //   2) 「页面可见 / 窗口获得焦点 / bfcache 恢复」这几个事件上各探一次；
  //   3) 角标**正在显示**期间保留一个 30s 的低频轮询，角标一隐藏立刻停掉；
  //   4) 点关闭按钮后立刻停轮询，不等下一个 tick。
  //
  // 3) 为什么不能省：连接状态只有 background 侧知道，而 chrome.runtime.sendMessage 是
  // 「请求-响应」通道；目前唯一的反向通道是 popup 广播的 badgeConfig（只带配置、不带状态）。
  // 所以「ws 断开 / SW 被回收后重新唤起的角标」只能由页面自己按低频节奏问一次。
  // 30s 是这个取舍的上限：既能在 SW 被回收或 daemon 断开后几十秒内把角标切掉，
  // 又不会像 5s 常驻那样把 SW 一直钉住。
  const BADGE_POLL_MS=30000;

  // 当前挂载的角标实例（销毁函数）；null 表示页面上没有任何角标 DOM，也没有任何定时器。
  let inst=null;

  /**
   * 读 showBadge。缺省值必须当 false：开关不存在时宁可不显示，也不给站点留指纹。
   * 读失败（非扩展环境 / storage 不可用）同样按关闭处理。
   */
  async function readShowBadge(){
    try{
      const got=await globalThis.chrome?.storage?.local?.get({showBadge:false});
      return got?.showBadge===true;
    }catch(_){
      return false;
    }
  }

  function destroyBadge(){
    if(!inst)return;
    const teardown=inst;
    inst=null;
    try{ teardown(); }catch(_){}
  }

  function createBadge(){
    if(inst)return;
    // 宿主只是个空壳（自身不带任何视觉/布局痕迹），样式和结构全在 shadow root 里：
    // 页面 CSS 进不来，页面 JS 也 querySelector 不到内部节点。
    const host=document.createElement('div');
    host.id=HOST_ID;
    const root=host.attachShadow({mode:'closed'});
    const style=document.createElement('style');
    style.textContent=':host{all:initial}#atb-badge{position:fixed;top:40%;right:0;display:flex;align-items:center;gap:10px;width:218px;height:36px;box-sizing:border-box;padding:0 8px 0 9px;color:white;border-radius:18px 0 0 18px;font-size:11px;font-weight:bold;line-height:36px;z-index:99999;cursor:grab;box-shadow:0 4px 12px rgba(0,0,0,0.22);background:#4CAF50;opacity:.72;overflow:visible;white-space:nowrap;user-select:none;transform:translateX(190px);transition:transform .18s ease,opacity .18s ease;}#atb-badge::after{content:"";position:absolute;inset:0;border-radius:18px 0 0 18px;background:#4CAF50;z-index:-1;}#atb-dot{flex:0 0 auto;width:10px;height:10px;border-radius:999px;background:white;}#atb-label{flex:0 0 auto;max-width:150px;overflow:hidden;}#atb-badge[data-expanded="1"]{opacity:.92;transform:translateX(0);}#atb-badge[data-dragging="1"]{cursor:grabbing;transition:none;}#atb-close{position:relative;flex:0 0 auto;width:20px;height:20px;border-radius:999px;line-height:18px;text-align:center;font-size:15px;font-weight:bold;background:#1f7a33;color:white;opacity:0;pointer-events:none;cursor:pointer;}#atb-badge[data-expanded="1"] #atb-close{opacity:1;pointer-events:auto;}#atb-close:hover{background:#145523;}#atb-close::after{content:"本次隐藏";position:absolute;right:0;bottom:28px;padding:4px 7px;border-radius:4px;background:#1f2937;color:white;font-size:12px;font-weight:500;line-height:16px;opacity:0;pointer-events:none;transform:translateY(4px);transition:opacity .12s ease,transform .12s ease;}#atb-close:hover::after{opacity:1;transform:translateY(0);}';
    root.appendChild(style);
    const d=document.createElement('div');
    d.id='atb-badge';
    const dot=document.createElement('span');
    dot.id='atb-dot';
    const label=document.createElement('span');
    label.id='atb-label';
    label.textContent='agent_browser_cli: 已连接';
    const close=document.createElement('span');
    close.id='atb-close';
    close.textContent='×';
    d.appendChild(dot);
    d.appendChild(label);
    d.appendChild(close);
    root.appendChild(d);
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
        const resp = await globalThis.chrome?.runtime?.sendMessage?.({ cmd: 'status' });
        const connected = !!resp?.ok && !!resp?.data?.wsConnected;
        setBadgeState(connected, resp?.data?.wsUrl || '', resp?.data?.lastCommandAt);
      } catch (e) {
        setBadgeState(false, (e && e.message) || '', 0);
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
    const onResize = () => {
      lastTop = clampTop(lastTop);
      d.style.top = lastTop + 'px';
    };
    window.addEventListener('resize', onResize);
    (document.body||document.documentElement).appendChild(host);
    setBadgeState(false, '');
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

    inst = function teardownBadge() {
      if (collapseTimer) { clearTimeout(collapseTimer); collapseTimer = null; }
      stopPolling();
      document.removeEventListener('visibilitychange', onVisibilityChange);
      window.removeEventListener('focus', probeAndSync);
      window.removeEventListener('pageshow', probeAndSync);
      window.removeEventListener('resize', onResize);
      // 连宿主一起摘掉：关掉开关后页面上不该再剩下任何角标节点。
      host.remove();
    };
  }

  /**
   * storage 是主通道：popup 写 showBadge 后，已打开的标签页即时跟着开/关，不必等刷新。
   */
  const onStorageChanged = (changes, area) => {
    if (!changes || !('showBadge' in changes)) return;
    if (area && area !== 'local') return;
    if (changes.showBadge && changes.showBadge.newValue === true) createBadge();
    else destroyBadge();
  };

  /**
   * popup 额外广播的 badgeConfig：兜住「本页 content script 还没跑过 storage.get」的极短窗口。
   * 这里不信任消息里的布尔值，直接用 storage 的最新值对齐，避免两条通道打架。
   */
  const onRuntimeMessage = (msg) => {
    if (!msg || msg.cmd !== 'badgeConfig') return;
    readShowBadge().then((show) => {
      if (show) createBadge();
      else destroyBadge();
    });
  };

  try { globalThis.chrome?.storage?.onChanged?.addListener(onStorageChanged); } catch (_) {}
  try { globalThis.chrome?.runtime?.onMessage?.addListener(onRuntimeMessage); } catch (_) {}

  window.__agentBrowserCliCleanup = () => {
    destroyBadge();
    try { globalThis.chrome?.storage?.onChanged?.removeListener(onStorageChanged); } catch (_) {}
    try { globalThis.chrome?.runtime?.onMessage?.removeListener(onRuntimeMessage); } catch (_) {}
  };

  // 默认关闭：读到的不是明确的 true 就什么都不创建（无 DOM、无 style、无定时器）。
  readShowBadge().then((show) => { if (show) createBadge(); });
})();
})();
