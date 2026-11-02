// 单往返页面动作：定位 → 滚动 → 校验 → 执行 → 返回命中信息。
// 与 CDP 路径相比少了 2~3 次往返，且在没有 debugger 权限时仍可用。
function __abcVisible(el) {
  if (!el || el.nodeType !== 1) return false;
  const rect = el.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) return false;
  const style = getComputedStyle(el);
  return style.visibility !== 'hidden' && style.display !== 'none' && Number(style.opacity) !== 0;
}

function __abcResolve(target, index) {
  if (!target) return null;
  if (typeof target === 'object' && target.selector) target = target.selector;
  if (typeof target === 'object' && target.text) target = 'text=' + target.text;
  const raw = String(target);
  let nodes = [];
  if (raw.startsWith('text=')) {
    const needle = raw.slice(5).trim().toLowerCase();
    const all = document.querySelectorAll('body *');
    for (const el of all) {
      if (el.children.length > 3) continue;
      const t = (el.innerText || el.textContent || '').trim().toLowerCase();
      if (t && t.includes(needle)) nodes.push(el);
      if (nodes.length > 200) break;
    }
  } else if (raw.startsWith('xpath=')) {
    const snapshot = document.evaluate(raw.slice(6), document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
    for (let i = 0; i < snapshot.snapshotLength; i += 1) nodes.push(snapshot.snapshotItem(i));
  } else {
    nodes = Array.from(document.querySelectorAll(raw));
  }
  // 默认取第一个可见节点；显式 index 时按原始顺序取。
  if (Number.isInteger(index)) return nodes[index] || null;
  return nodes.find(__abcVisible) || nodes[0] || null;
}

function __abcDispatch(el, type, init) {
  const opts = Object.assign({ bubbles: true, cancelable: true, composed: true }, init || {});
  let event;
  if (type.startsWith('pointer') && typeof PointerEvent === 'function') {
    event = new PointerEvent(type, opts);
  } else if (type.startsWith('mouse') && typeof MouseEvent === 'function') {
    event = new MouseEvent(type, opts);
  } else {
    event = new Event(type, opts);
  }
  return el.dispatchEvent(event);
}

function __abcSetNativeValue(el, value) {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype
    : (el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype);
  const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set;
  if (setter) setter.call(el, value);
  else el.value = value;
}

function __abcAct(op, target, params) {
  params = params || {};
  const index = params.index;
  const el = __abcResolve(target, index);

  if (op === 'scroll' && !el) {
    const dy = Number(params.dy || 0);
    const dx = Number(params.dx || 0);
    const to = params.to;
    if (to === 'top') window.scrollTo({ top: 0, behavior: 'instant' });
    // document_start 阶段 / XML 文档里 document.body 可能还不存在，退回 documentElement。
    else if (to === 'bottom') {
      const root = document.body || document.documentElement;
      window.scrollTo({ top: root ? root.scrollHeight : 0, behavior: 'instant' });
    }
    else window.scrollBy({ top: dy, left: dx, behavior: 'instant' });
    return { ok: true, op, target: null, scrollY: Math.round(window.scrollY), scrollX: Math.round(window.scrollX) };
  }

  if (!el) return { ok: false, op, error: 'target not found: ' + String(target) };

  el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
  const rect = el.getBoundingClientRect();
  const cx = Math.round(rect.x + rect.width / 2);
  const cy = Math.round(rect.y + rect.height / 2);
  const hitEl = document.elementFromPoint(cx, cy);
  const hit = {
    tag: hitEl ? hitEl.tagName.toLowerCase() : null,
    matches: !!hitEl && (hitEl === el || el.contains(hitEl) || hitEl.contains(el)),
    covered_by: hitEl && hitEl !== el && !el.contains(hitEl) ? (hitEl.tagName.toLowerCase() + (hitEl.id ? '#' + hitEl.id : '')) : null
  };

  const rectInfo = { x: Math.round(rect.x), y: Math.round(rect.y), w: Math.round(rect.width), h: Math.round(rect.height) };
  const base = { op, tag: el.tagName.toLowerCase(), text: (el.innerText || '').trim().slice(0, 120), rect: rectInfo, hit };
  const pointerInit = { clientX: cx, clientY: cy, pointerType: 'mouse', button: 0, buttons: 0 };

  try {
    if (op === 'hover') {
      __abcDispatch(el, 'pointerover', pointerInit);
      __abcDispatch(el, 'pointerenter', pointerInit);
      __abcDispatch(el, 'mouseover', pointerInit);
      __abcDispatch(el, 'mouseenter', pointerInit);
      __abcDispatch(el, 'pointermove', pointerInit);
      __abcDispatch(el, 'mousemove', pointerInit);
      return Object.assign(base, { ok: true });
    }

    if (op === 'click') {
      __abcDispatch(el, 'pointerover', pointerInit);
      __abcDispatch(el, 'mouseover', pointerInit);
      __abcDispatch(el, 'pointerdown', Object.assign({}, pointerInit, { buttons: 1 }));
      __abcDispatch(el, 'mousedown', Object.assign({}, pointerInit, { buttons: 1 }));
      if (typeof el.focus === 'function') { try { el.focus({ preventScroll: true }); } catch (_) {} }
      __abcDispatch(el, 'pointerup', Object.assign({}, pointerInit, { buttons: 0 }));
      __abcDispatch(el, 'mouseup', Object.assign({}, pointerInit, { buttons: 0 }));
      // 只让浏览器原生派发 click（el.click()）。这里再手动 dispatch 一次 click
      // 会让页面收到两次点击，提交类按钮会被重复触发。
      if (typeof el.click === 'function') {
        el.click();
      } else {
        __abcDispatch(el, 'click', Object.assign({}, pointerInit, { buttons: 0, detail: 1 }));
      }
      return Object.assign(base, { ok: true });
    }

    if (op === 'focus') {
      if (typeof el.focus === 'function') el.focus({ preventScroll: true });
      return Object.assign(base, { ok: true, active: document.activeElement === el });
    }

    if (op === 'select') {
      if (el.tagName.toLowerCase() !== 'select') return Object.assign(base, { ok: false, error: 'not a <select>' });
      const want = params.value != null ? String(params.value) : (params.label != null ? String(params.label) : '');
      let chosen = -1;
      for (let i = 0; i < el.options.length; i += 1) {
        const opt = el.options[i];
        if (String(opt.value) === want || (opt.text || '').trim() === want) { chosen = i; break; }
      }
      if (chosen < 0) {
        return Object.assign(base, {
          ok: false,
          error: 'option not found: ' + want,
          options: Array.from(el.options).slice(0, 40).map(o => ({ value: o.value, text: (o.text || '').trim() }))
        });
      }
      el.selectedIndex = chosen;
      __abcDispatch(el, 'input', {});
      __abcDispatch(el, 'change', {});
      return Object.assign(base, { ok: true, value: el.value });
    }

    if (op === 'check' || op === 'uncheck') {
      const want = op === 'check';
      el.checked = want;
      __abcDispatch(el, 'input', {});
      __abcDispatch(el, 'change', {});
      return Object.assign(base, { ok: true, checked: el.checked });
    }

    if (op === 'setValue') {
      const value = params.value == null ? '' : String(params.value);
      if (params.clear !== false) __abcSetNativeValue(el, '');
      __abcSetNativeValue(el, params.append ? (el.value + value) : value);
      __abcDispatch(el, 'input', {});
      __abcDispatch(el, 'change', {});
      return Object.assign(base, { ok: true, value: el.value });
    }

    if (op === 'scroll') {
      el.scrollIntoView({ block: params.block || 'center', inline: 'nearest', behavior: 'instant' });
      const after = el.getBoundingClientRect();
      return Object.assign(base, { ok: true, rect: { x: Math.round(after.x), y: Math.round(after.y), w: Math.round(after.width), h: Math.round(after.height) } });
    }
  } catch (e) {
    return Object.assign(base, { ok: false, error: e && e.message ? e.message : String(e) });
  }

  return Object.assign(base, { ok: false, error: 'unsupported op: ' + op });
}
