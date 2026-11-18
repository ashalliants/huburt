(() => {
  'use strict';

  const tauri = window.__TAURI__;
  // The same page serves two web views: the sidebar ("ui") and the cover shown over the content area while a service's
  // page isn't ready ("cover"). In a plain browser (previews) it shows both side by side.
  const meta = (window.__TAURI_INTERNALS__ || {}).metadata || {};
  const label = (meta.currentWebview || {}).label || '';
  const mode = label === 'cover' ? 'cover' : label === 'ui' ? 'sidebar' : (new URLSearchParams(location.search).get('mode') || 'both');
  document.documentElement.dataset.mode = mode;
  const sidebar = document.getElementById('sidebar');
  const content = document.getElementById('content');
  const invoke = (cmd, args) => tauri ? tauri.core.invoke(cmd, args || {}) : Promise.resolve(mockInvoke(cmd, args));

  function mockInvoke(cmd, args) {
    if (cmd === 'select' && state) { state.selected = args.id; render(); }
    else if (cmd === 'reorder' && state) state.order = args.order;
    else console.log('invoke', cmd, args);
  }

  // ---- Icons (monochrome, currentColor) ----
  const svg = (inner, fill) =>
    `<svg viewBox="0 0 24 24" fill="${fill ? 'currentColor' : 'none'}" stroke="${fill ? 'none' : 'currentColor'}" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${inner}</svg>`;
  const ICONS = {
    slack: svg('<path d="M9.5 3.5 7.5 20.5M16.5 3.5l-2 17M4 8.5h16.5M3.5 15.5H20"/>'),
    whatsapp: svg('<path d="M12 3.5a8.5 8.5 0 0 0-7.3 12.8L3.5 20.5l4.3-1.1A8.5 8.5 0 1 0 12 3.5z"/><path d="M9 8.8c.2 3 2.700 5.500 6 6.200l1-1.600-2-1-.9.900c-.9-.4-1.700-1.200-2.100-2.100l.9-.9-1-2z"/>'),
    gmail: svg('<rect x="3.500" y="5.500" width="17" height="13" rx="2.500"/><path d="m4 7.500 8 6 8-6"/>'),
  };
  const iconFor = (name) => ICONS[name] || ICONS.slack;
  const POWER = svg('<path d="M12 3v8M6.800 6.800a7 7 0 1 0 10.400 0"/>').replace('stroke-width="1.8"', 'stroke-width="3"');
  const BELL_OFF = svg('<path d="M6 17h12l-1.500-2v-5a4.500 4.500 0 0 0-9 0v5zM10 20h4M3.500 3.500l17 17"/>').replace('stroke-width="1.8"', 'stroke-width="2.600"');
  const WIFI_OFF = svg('<path d="M3 9a14 14 0 0 1 4-2.300M21 9a14 14 0 0 0-9-3.500M6.500 12.500a9 9 0 0 1 3-1.700M17.500 12.500a9 9 0 0 0-3.800-2M9.500 16a4.500 4.500 0 0 1 5 0M12 19.500h.01M3 3l18 18"/>');
  const POWER_BIG = svg('<path d="M12 3v8M6.800 6.800a7 7 0 1 0 10.400 0"/>');
  const KEY = svg('<circle cx="8" cy="15" r="3.500"/><path d="m10.500 12.500 8-8M15 8l2.500 2.500"/>');

  // ---- State ----
  let state = null;
  let dragging = false;

  const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

  // ---- Sidebar ----
  function tileTitle(id, s) {
    const lines = [s.name];
    if (!s.enabled) return `${s.name} is turned off\nRight-click to turn it on`;
    if (s.unread > 0) lines.push(`${s.unread} unread`);
    if (s.quiet > 0) lines.push(`${s.quiet} unread channel${s.quiet === 1 ? '' : 's'} (no mentions)`);
    if (s.signedOut) lines.push('Signed out. Click to sign in again.');
    if (s.muted) lines.push('Notifications muted');
    lines.push('Drag to reorder, right-click for more');
    return lines.join('\n');
  }

  function renderTile(id, s, selected) {
    let btn = sidebar.querySelector(`.tile[data-id="${CSS.escape(id)}"]`);
    if (!btn) {
      btn = document.createElement('button');
      btn.className = 'tile';
      btn.type = 'button';
      btn.dataset.id = id;
      btn.innerHTML = '<span class="icon"></span><span class="label"></span>';
    }
    btn.classList.toggle('selected', selected);
    btn.classList.toggle('off', !s.enabled);
    btn.setAttribute('aria-pressed', selected ? 'true' : 'false');
    btn.title = tileTitle(id, s);
    btn.querySelector('.label').textContent = s.name;

    const icon = btn.querySelector('.icon');
    let h = iconFor(s.icon);
    if (!s.enabled) h += `<span class="power">${POWER}</span>`;
    if (s.muted && s.enabled) h += `<span class="muted-mark">${BELL_OFF}</span>`;
    if (s.enabled) {
      if (s.signedOut) h += '<span class="warn" aria-label="Signed out">!</span>';
      else if (s.unread > 0) h += `<span class="badge">${s.unread > 99 ? '99+' : s.unread}</span>`;
      else if (s.quiet > 0) h += '<span class="quiet-dot"></span>';
    }
    if (icon.dataset.h !== h) { icon.innerHTML = h; icon.dataset.h = h; }
    return btn;
  }

  function renderSidebar() {
    if (dragging) return;
    const frag = state.order.filter((id) => state.services[id]).map((id) =>
      renderTile(id, state.services[id], state.selected === id));
    // Re-append in order (keeps existing nodes, so focus and hover survive)
    frag.forEach((el, i) => { if (sidebar.children[i] !== el) sidebar.insertBefore(el, sidebar.children[i] || null); });
    while (sidebar.children.length > frag.length) sidebar.lastChild.remove();
  }

  // ---- Content cover ----
  function cover(glyph, title, detail, button, onClick) {
    const el = document.createElement('div');
    el.className = 'cover';
    el.innerHTML = `<div class="glyph">${glyph}</div><h1>${esc(title)}</h1>` +
      (detail ? `<p>${esc(detail)}</p>` : '') + (button ? `<button type="button">${esc(button)}</button>` : '');
    if (button) el.querySelector('button').addEventListener('click', onClick);
    return el;
  }

  let lastCoverKey = null;
  function renderContent() {
    const id = state.selected;
    const s = state.services[id];
    let key = 'none', el = null;
    if (s) {
      const n = s.name;
      if (!s.enabled) {
        key = `off:${id}`;
        el = cover(POWER_BIG, `${n} is turned off`, "Huburt isn't loading or checking it, so it uses no memory. Your login is kept.",
          `Turn On ${n}`, () => invoke('set_enabled', { id, enabled: true }));
      } else if (s.error) {
        key = `err:${id}:${s.error}`;
        el = cover(WIFI_OFF, s.error, 'Check your internet connection. Huburt will try again by itself as soon as you’re back online.',
          'Try Again', () => invoke('reload', { id }));
      } else if (s.waking) {
        key = `wake:${id}`;
        el = document.createElement('div');
        el.className = 'cover';
        el.innerHTML = `<div class="spinner" role="progressbar" aria-label="Loading"></div><p class="hint">Waking up ${esc(n)}…</p>`;
      } else if (s.signedOut) {
        key = `out:${id}`;
        el = cover(KEY, 'Signed out', `Open ${n} and sign in again to keep getting your messages.`);
      }
    }
    if (key === lastCoverKey) return;
    lastCoverKey = key;
    content.replaceChildren(...(el ? [el] : []));
  }

  function render() {
    if (!state) return;
    renderSidebar();
    renderContent();
  }

  function setState(next) {
    if (!next || !next.order) return;
    state = next;
    render();
  }

  // ---- Interactions: click, context menu, drag to reorder ----
  const tileOf = (e) => e.target.closest && e.target.closest('.tile');

  sidebar.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    const t = tileOf(e);
    if (t) invoke('show_menu', { id: t.dataset.id, x: e.clientX, y: e.clientY });
  });

  let press = null;
  sidebar.addEventListener('pointerdown', (e) => {
    const t = tileOf(e);
    if (!t || e.button !== 0) return;
    press = { t, id: t.dataset.id, startY: e.clientY, moved: false, pid: e.pointerId, offset: 0 };
    try { t.setPointerCapture(e.pointerId); } catch (_) {}
  });

  sidebar.addEventListener('pointermove', (e) => {
    if (!press || e.pointerId !== press.pid) return;
    const dy = e.clientY - press.startY;
    if (!press.moved) {
      if (Math.abs(dy) < 5) return;
      press.moved = true;
      dragging = true;
      press.t.classList.add('dragging');
    }
    const t = press.t;
    // Swap with whichever sibling the pointer is over, then keep the tile under the pointer
    const siblings = [...sidebar.children].filter((c) => c !== t);
    for (const s of siblings) {
      const r = s.getBoundingClientRect();
      if (e.clientY > r.top && e.clientY < r.bottom) {
        const before = t.compareDocumentPosition(s) & Node.DOCUMENT_POSITION_FOLLOWING;
        sidebar.insertBefore(t, before ? s.nextSibling : s);
        break;
      }
    }
    // position relative to natural slot
    t.style.transform = 'translateY(0)';
    const natural = t.getBoundingClientRect().top;
    const want = e.clientY - press.grab;
    t.style.transform = `translateY(${want - natural}px)`;
  });

  sidebar.addEventListener('pointerdown', (e) => {
    if (press) press.grab = e.clientY - press.t.getBoundingClientRect().top;
  });

  function endPress(e, cancelled) {
    if (!press || e.pointerId !== press.pid) return;
    const p = press; press = null;
    try { p.t.releasePointerCapture(p.pid); } catch (_) {}
    if (!p.moved) {
      if (!cancelled) { p.t.blur(); invoke('select', { id: p.id }); }
      return;
    }
    p.t.classList.remove('dragging');
    p.t.style.transform = '';
    dragging = false;
    const order = [...sidebar.children].map((c) => c.dataset.id);
    if (!cancelled && order.join() !== state.order.join()) {
      state.order = order;
      invoke('reorder', { order });
    }
    render();
  }
  sidebar.addEventListener('pointerup', (e) => endPress(e, false));
  sidebar.addEventListener('pointercancel', (e) => endPress(e, true));

  // Swallow the native/web context menu everywhere (the native one is built in Rust)
  document.addEventListener('contextmenu', (e) => e.preventDefault());

  // ---- Keyboard ----
  // macOS: the native app menu owns these shortcuts. Windows (and browser previews): Ctrl+1..9 switch service, Ctrl+R
  // reloads the selected one, Ctrl+W / Ctrl+H hide the window to the tray.
  const isMac = /Mac/i.test(navigator.platform);
  document.addEventListener('keydown', (e) => {
    if ((tauri && isMac) || !state || e.altKey || e.shiftKey) return;
    if (isMac ? (!e.metaKey || e.ctrlKey) : (!e.ctrlKey || e.metaKey)) return;
    const k = e.key.toLowerCase();
    if (/^[1-9]$/.test(k)) {
      const id = state.order[Number(k) - 1];
      if (id) { e.preventDefault(); invoke('select', { id }); }
    } else if (k === 'r') {
      e.preventDefault();
      if (state.selected) invoke('reload', { id: state.selected });
    } else if (k === 'w' || k === 'h') {
      e.preventDefault();
      invoke('shortcut', { kind: 'hide' });
    }
  });

  // ---- Boot ----
  function mockState(kind) {
    const mk = (name, icon, o) => Object.assign({ name, icon, enabled: true, unread: 0, quiet: 0, signedOut: false, muted: false, waking: false, error: null }, o || {});
    const s = {
      order: ['slack', 'whatsapp', 'gmail'], selected: 'slack', paused: false,
      services: { slack: mk('Slack', 'slack', { unread: 2 }), whatsapp: mk('WhatsApp', 'whatsapp', { quiet: 3 }), gmail: mk('Gmail', 'gmail') },
    };
    const sl = s.services.slack;
    switch (kind) {
      case 'waking': sl.waking = true; break;
      case 'error': sl.error = "Can't reach Slack"; break;
      case 'off': sl.enabled = false; s.services.gmail.enabled = false; break;
      case 'signedout': sl.signedOut = true; sl.unread = 0; s.services.whatsapp.signedOut = true; break;
      case 'badges': sl.unread = 142; s.services.whatsapp.unread = 7; s.services.gmail.quiet = 1; s.services.gmail.muted = true; break;
    }
    return s;
  }

  if (tauri) {
    tauri.event.listen('state', (e) => setState(e.payload));
    invoke('get_state').then(setState);
  } else {
    const q = location.search, hash = location.hash;
    if (/[?&]dark\b/.test(q)) document.documentElement.dataset.theme = 'dark';
    else if (/[?&]light\b/.test(q)) document.documentElement.dataset.theme = 'light';
    const m = /mock=(\w+)/.exec(hash);
    setState(mockState(m && m[1]));
    // let mock clicks do something visible in a plain browser
    window.addEventListener('hashchange', () => { const mm = /mock=(\w+)/.exec(location.hash); setState(mockState(mm && mm[1])); });
  }
})();
