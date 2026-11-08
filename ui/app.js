// Orbit – shell logic
// The shell draws the UI. Real web pages are native browser webviews that
// Rust places on top of the #viewport and #yt-viewport placeholders.
// All user-facing text goes through t() (see i18n.js / locales/en.json).

const { invoke, listen, appWindow } = window.OrbitBridge;

const SEARCH_URL = 'https://www.google.com/search?q=';
const QUICK_LINKS = [
  { name: 'YouTube', url: 'https://www.youtube.com', color: '#ff3b4e', letter: '▶' },
  { name: 'GitHub', url: 'https://github.com', color: '#30363d', letter: 'G' },
  { name: 'Wikipedia', url: 'https://www.wikipedia.org', color: '#5a5a66', letter: 'W' },
  { name: 'Reddit', url: 'https://www.reddit.com', color: '#ff4500', letter: 'R' },
  { name: 'Twitch', url: 'https://www.twitch.tv', color: '#9146ff', letter: 'T' },
  { name: 'Gmail', url: 'https://mail.google.com', color: '#ea4335', letter: 'M' },
];

const $ = (s) => document.querySelector(s);
const $$ = (s) => document.querySelectorAll(s);

// ------------------------------------------------------------ Persistence
function load(key, fallback) {
  try {
    const v = localStorage.getItem('orbit.' + key);
    return v === null ? fallback : JSON.parse(v);
  } catch { return fallback; }
}
function save(key, value) {
  try { localStorage.setItem('orbit.' + key, JSON.stringify(value)); } catch { /* ignore */ }
}

// ------------------------------------------------------------ State
// Tab: { id, url, title, loading, live }  live = webview exists (otherwise the tab is "sleeping")
const state = {
  tabs: [],
  activeId: null,
  nextId: 1,
  view: 'browser',
  ytPopped: false,
};

const activeTab = () => state.tabs.find((t) => t.id === state.activeId) || null;
const findTab = (id) => state.tabs.find((t) => t.id === id) || null;

function persistTabs() {
  save('session', {
    tabs: state.tabs.map((t) => ({ url: t.url, title: t.title })),
    active: state.tabs.findIndex((t) => t.id === state.activeId),
  });
}

function call(cmd, args) {
  return invoke(cmd, args).catch((e) => {
    console.error(cmd, e);
    setStatus(String(e));
  });
}

let statusTimer;
function setStatus(text) {
  const el = $('#status-text');
  el.textContent = text;
  clearTimeout(statusTimer);
  statusTimer = setTimeout(() => (el.textContent = ''), 4000);
}

// ------------------------------------------------------------ URL input
function toUrl(input) {
  const s = input.trim();
  if (!s) return null;
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(s) || /^(about|data|file):/i.test(s)) return s;
  if (/^localhost(:\d+)?(\/|$)/i.test(s)) return 'http://' + s;
  if (!/\s/.test(s) && /^[^\s/]+\.[a-z]{2,}(:\d+)?(\/.*)?$/i.test(s)) return 'https://' + s;
  return SEARCH_URL + encodeURIComponent(s);
}

function hostOf(url) {
  try { return new URL(url).hostname.replace(/^www\./, ''); } catch { return ''; }
}

// ------------------------------------------------------------ Tabs
function newTab(url = null, activate = true) {
  const tab = { id: state.nextId++, url, title: url ? hostOf(url) : t('tabs.newTitle'), loading: false, live: false };
  const cur = state.tabs.findIndex((t) => t.id === state.activeId);
  state.tabs.splice(cur >= 0 ? cur + 1 : state.tabs.length, 0, tab);
  if (activate) activateTab(tab.id);
  else render();
  persistTabs();
  if (activate && !url) focusStart();
  return tab;
}

async function activateTab(id) {
  const tab = findTab(id);
  if (!tab) return;
  state.activeId = id;
  if (state.view !== 'browser') setView('browser');
  render();
  if (tab.url && !tab.live) {
    tab.live = true;
    tab.loading = true;
    render();
    await call('tab_create', { id, url: tab.url, activate: true });
  } else {
    await call('tab_activate', { id: tab.live ? id : null });
  }
  persistTabs();
}

async function closeTab(id) {
  const idx = state.tabs.findIndex((t) => t.id === id);
  if (idx < 0) return;
  const [tab] = state.tabs.splice(idx, 1);
  if (tab.live) await call('tab_close', { id });
  if (state.tabs.length === 0) {
    state.activeId = null;
    newTab();
    return;
  }
  if (state.activeId === id) {
    const next = state.tabs[Math.min(idx, state.tabs.length - 1)];
    await activateTab(next.id);
  } else {
    render();
  }
  persistTabs();
}

async function navigate(input) {
  const url = toUrl(input);
  if (!url) return;
  let tab = activeTab();
  if (!tab) tab = newTab(null, true);
  tab.url = url;
  tab.title = hostOf(url) || url;
  tab.loading = true;
  if (tab.live) {
    await call('tab_navigate', { id: tab.id, url });
  } else {
    tab.live = true;
    await call('tab_create', { id: tab.id, url, activate: true });
  }
  render();
  persistTabs();
  $('#address').blur();
}

function cycleTab(dir) {
  if (state.tabs.length < 2) return;
  const i = state.tabs.findIndex((t) => t.id === state.activeId);
  const next = state.tabs[(i + dir + state.tabs.length) % state.tabs.length];
  activateTab(next.id);
}

function tabAction(action) {
  const tab = activeTab();
  if (tab && tab.live) call('tab_action', { id: tab.id, action });
}

// ------------------------------------------------------------ Views & layout
function setView(view) {
  state.view = view;
  $$('.rail-btn[data-view]').forEach((b) => b.classList.toggle('active', b.dataset.view === view));
  $$('.view').forEach((v) => (v.hidden = v.id !== 'view-' + view));
  const titles = {
    youtube: t('view.youtube'), explorer: t('view.files'), music: t('view.music'),
    extensions: t('view.extensions'), settings: t('view.settings'),
  };
  $('#tabstrip').hidden = view !== 'browser';
  $('#view-title').hidden = view === 'browser';
  $('#view-title').textContent = titles[view] || '';
  if (view === 'extensions') refreshExtensions();
  syncLayout();
}

function setPopped(popped) {
  state.ytPopped = popped;
  $('#yt-popped').hidden = !popped;
  $('#yt-popout').hidden = popped;
  syncLayout();
}

function rectOf(el) {
  const r = el.getBoundingClientRect();
  return { x: r.left, y: r.top, w: r.width, h: r.height };
}

let layoutQueued = false;
function syncLayout() {
  if (layoutQueued) return;
  layoutQueued = true;
  requestAnimationFrame(() => {
    layoutQueued = false;
    const tab = activeTab();
    call('set_layout', {
      content: rectOf($('#viewport')),
      yt: state.view === 'youtube' && !state.ytPopped ? rectOf($('#yt-viewport')) : null,
      browserVisible: state.view === 'browser' && !!tab && tab.live,
    });
  });
}

// ------------------------------------------------------------ Rendering
function faviconEl(tab) {
  const wrap = document.createElement('span');
  wrap.className = 'fav';
  if (tab.loading) {
    wrap.innerHTML = '<span class="spinner"></span>';
  } else if (tab.url) {
    try {
      const img = new Image();
      img.src = new URL(tab.url).origin + '/favicon.ico';
      img.onerror = () => (wrap.innerHTML = '<svg><use href="#i-globe"/></svg>');
      wrap.appendChild(img);
    } catch {
      wrap.innerHTML = '<svg><use href="#i-globe"/></svg>';
    }
  } else {
    wrap.innerHTML = '<span class="logo" style="width:12px;height:12px;box-shadow:none"></span>';
  }
  return wrap;
}

function renderTabs() {
  const box = $('#tabs');
  // reuse existing tab elements (no flicker animation on every update)
  const existing = new Map([...box.children].map((el) => [Number(el.dataset.id), el]));
  const frag = document.createDocumentFragment();
  for (const tab of state.tabs) {
    let el = existing.get(tab.id);
    if (!el) {
      el = document.createElement('div');
      el.className = 'tab';
      el.dataset.id = tab.id;
      el.innerHTML = `<span class="fav"></span><span class="t-title"></span><button class="t-close" title="${t('tabs.close')}"><svg><use href="#i-x"/></svg></button>`;
      el.addEventListener('mousedown', (e) => {
        if (e.button === 1) { e.preventDefault(); closeTab(tab.id); }
      });
      el.addEventListener('click', (e) => {
        if (e.target.closest('.t-close')) closeTab(tab.id);
        else activateTab(tab.id);
      });
    }
    const sig = `${tab.loading}|${tab.url}`;
    if (el.dataset.sig !== sig) {
      el.dataset.sig = sig;
      el.querySelector('.fav').replaceWith(faviconEl(tab));
    }
    el.querySelector('.t-title').textContent = tab.title || t('tabs.newTitle');
    el.title = tab.title + (tab.url ? '\n' + tab.url : '');
    el.classList.toggle('active', tab.id === state.activeId);
    el.classList.toggle('sleeping', !!tab.url && !tab.live);
    frag.appendChild(el);
  }
  box.replaceChildren(frag);
}

function render() {
  renderTabs();
  const tab = activeTab();
  const addr = $('#address');
  if (document.activeElement !== addr) addr.value = tab?.url || '';
  $('#start').hidden = !!(tab && tab.url);
  $('#nav-back').disabled = !tab?.live;
  $('#nav-fwd').disabled = !tab?.live;
  $('#nav-reload').disabled = !tab?.live;

  const p = $('#progress');
  if (tab?.loading) { p.className = 'loading'; }
  else if (p.classList.contains('loading')) { p.className = 'done'; }

  document.title = tab?.title ? `${tab.title} – Orbit` : 'Orbit';
  syncLayout();
}

function focusStart() {
  requestAnimationFrame(() => $('#start-input').focus());
}

function renderStart() {
  const h = new Date().getHours();
  const part = h < 5 ? 'night' : h < 11 ? 'morning' : h < 17 ? 'day' : h < 22 ? 'evening' : 'night';
  const greeting = $('#greeting');
  const accent = document.createElement('span');
  accent.textContent = t('start.welcome');
  greeting.replaceChildren(t('start.greeting.' + part) + ', ', accent);
  const tick = () => {
    $('#clock').textContent = new Date().toLocaleString(I18N.lang, {
      weekday: 'long', day: 'numeric', month: 'long', hour: '2-digit', minute: '2-digit',
    });
  };
  tick();
  setInterval(tick, 15000);

  $('#quick').replaceChildren(...QUICK_LINKS.map((q) => {
    const b = document.createElement('button');
    b.className = 'quick';
    b.innerHTML = `<span class="q-icon" style="background:${q.color}">${q.letter}</span>${q.name}`;
    b.addEventListener('click', () => navigate(q.url));
    return b;
  }));
}

// ------------------------------------------------------------ Extensions
function extMsg(text, kind = '') {
  const el = $('#ext-msg');
  el.textContent = text;
  el.className = 'muted small ' + kind;
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

async function refreshExtensions() {
  const list = (await call('ext_list')) || [];
  const box = $('#ext-list');
  if (!list.length) {
    const empty = document.createElement('div');
    empty.className = 'ext-empty';
    empty.textContent = t('ext.empty');
    box.replaceChildren(empty);
    return;
  }
  box.replaceChildren(...list.map((ext) => {
    const el = document.createElement('div');
    el.className = 'ext-item';
    const icon = ext.icon
      ? `<img src="${ext.icon}" alt="">`
      : '<span class="ext-fallback"><svg><use href="#i-puzzle"/></svg></span>';
    el.innerHTML = `${icon}
      <div class="ext-info">
        <div class="ext-name">${escapeHtml(ext.name)} <span class="muted small">${escapeHtml(ext.version)}</span></div>
        <div class="ext-desc">${escapeHtml(ext.description)}</div>
      </div>`;
    const base = ext.id ? `chrome-extension://${ext.id}/` : null;
    if (base && ext.popup) el.appendChild(extButton(t('ext.open'), '#i-open', () => newTab(base + ext.popup)));
    if (base && ext.options) el.appendChild(extButton(t('ext.options'), '#i-reload', () => newTab(base + ext.options)));
    const del = extButton('', '#i-trash', async () => {
      del.disabled = true;
      try {
        await invoke('ext_remove', { folder: ext.folder });
        extMsg(t('ext.removed', { name: ext.name }), 'ok');
      } catch (e) {
        extMsg(String(e), 'err');
      }
      refreshExtensions();
    });
    del.title = t('ext.remove');
    el.appendChild(del);
    return el;
  }));
}

function extButton(label, icon, onClick) {
  const b = document.createElement('button');
  b.className = label ? 'pill-btn' : 'icon-btn';
  b.innerHTML = `<svg><use href="${icon}"/></svg>${label ? `<span>${label}</span>` : ''}`;
  b.onclick = onClick;
  return b;
}

async function installExtension() {
  const input = $('#ext-input').value.trim();
  if (!input) return;
  const btn = $('#ext-install');
  btn.disabled = true;
  extMsg(t('ext.installing'));
  try {
    const ext = await invoke('ext_install', { input });
    $('#ext-input').value = '';
    extMsg(t('ext.installed', { name: ext.name }), 'ok');
    refreshExtensions();
  } catch (e) {
    extMsg(String(e), 'err');
  } finally {
    btn.disabled = false;
  }
}

// ------------------------------------------------------------ Media bar
// The core reports the "primary" media (most recently started) of all tabs and
// YouTube: { page, playing, title, artist, artwork, position, duration, ... }
const media = { info: null, at: 0 };

function fmtTime(sec) {
  if (!isFinite(sec) || sec < 0) sec = 0;
  const s = Math.floor(sec % 60), m = Math.floor(sec / 60) % 60, h = Math.floor(sec / 3600);
  const mm = h ? String(m).padStart(2, '0') : m;
  return (h ? h + ':' : '') + mm + ':' + String(s).padStart(2, '0');
}

function mediaPosition() {
  const i = media.info;
  if (!i) return 0;
  const p = i.position + (i.playing ? (performance.now() - media.at) / 1000 : 0);
  return i.duration ? Math.min(p, i.duration) : p;
}

function tickMedia() {
  const i = media.info;
  $('#player-pos').textContent = i ? fmtTime(mediaPosition()) : '';
  $('#player-fill').style.width = i && i.duration ? (mediaPosition() / i.duration) * 100 + '%' : '0%';
}

function renderMedia() {
  const i = media.info;
  const bar = $('#player');
  bar.classList.toggle('idle', !i);
  bar.classList.toggle('playing', !!i?.playing);
  $('#player-title').textContent = i ? i.title : t('player.idleTitle');
  $('#player-artist').textContent = i ? i.artist : t('player.idleSubtitle');
  $('#player-cover').style.backgroundImage = i?.artwork ? `url("${String(i.artwork).replace(/"/g, '%22')}")` : '';
  const toggle = $('#player-toggle');
  toggle.querySelector('use').setAttribute('href', i?.playing ? '#i-pause' : '#i-pp');
  toggle.title = t(i?.playing ? 'player.pause' : 'player.play');
  for (const id of ['#player-toggle', '#player-prev', '#player-next']) $(id).disabled = !i;
  $('#player-dur').textContent = i?.duration ? fmtTime(i.duration) : '';
  tickMedia();
}

function mediaControl(action, value = 0) {
  if (media.info) call('media_control', { action, value });
}

function goToMediaSource() {
  const i = media.info;
  if (!i) return;
  if (i.page === 0) {
    if (state.ytPopped) call('yt_popout'); // focuses the pop-out window
    else setView('youtube');
  } else {
    activateTab(i.page);
  }
}

// ------------------------------------------------------------ Shortcuts
function runShortcut(cmd) {
  switch (cmd) {
    case 'new-tab': newTab(); break;
    case 'close-tab': if (state.activeId != null) closeTab(state.activeId); break;
    case 'focus-address': setView('browser'); $('#address').focus(); $('#address').select(); break;
    case 'next-tab': cycleTab(1); break;
    case 'prev-tab': cycleTab(-1); break;
    case 'toggle-yt': setView(state.view === 'youtube' ? 'browser' : 'youtube'); break;
    // sent by the core when the UI itself has focus
    case 'reload': tabAction('reload'); break;
    case 'back': tabAction('back'); break;
    case 'forward': tabAction('forward'); break;
  }
}

document.addEventListener('keydown', (e) => {
  const k = e.key.toLowerCase();
  let cmd = null;
  if (e.ctrlKey && e.shiftKey && k === 'tab') cmd = 'prev-tab';
  else if (e.ctrlKey && e.shiftKey && k === 'y') cmd = 'toggle-yt';
  else if (e.ctrlKey && !e.shiftKey && !e.altKey) {
    if (k === 't') cmd = 'new-tab';
    else if (k === 'w') cmd = 'close-tab';
    else if (k === 'l') cmd = 'focus-address';
    else if (k === 'tab') cmd = 'next-tab';
    else if (k === 'r') { e.preventDefault(); tabAction('reload'); return; }
  } else if (e.altKey && k === 'arrowleft') { e.preventDefault(); tabAction('back'); return; }
  else if (e.altKey && k === 'arrowright') { e.preventDefault(); tabAction('forward'); return; }
  else if (k === 'f5') { e.preventDefault(); tabAction('reload'); return; }
  if (cmd) { e.preventDefault(); runShortcut(cmd); }
});

// ------------------------------------------------------------ Wiring
function wire() {
  $('#win-min').onclick = () => appWindow.minimize();
  $('#win-max').onclick = () => appWindow.toggleMaximize();
  $('#win-close').onclick = () => appWindow.close();

  $('#new-tab').onclick = () => newTab();
  $('#nav-back').onclick = () => tabAction('back');
  $('#nav-fwd').onclick = () => tabAction('forward');
  $('#nav-reload').onclick = () => tabAction(activeTab()?.loading ? 'stop' : 'reload');

  const addr = $('#address');
  addr.addEventListener('focus', () => requestAnimationFrame(() => addr.select()));
  addr.addEventListener('blur', () => { addr.value = activeTab()?.url || ''; });
  addr.addEventListener('keydown', (e) => { if (e.key === 'Escape') addr.blur(); });
  $('#address-form').addEventListener('submit', (e) => { e.preventDefault(); navigate(addr.value); });
  $('#start-form').addEventListener('submit', (e) => {
    e.preventDefault();
    const v = $('#start-input').value;
    $('#start-input').value = '';
    navigate(v);
  });

  $$('.rail-btn[data-view]').forEach((b) => (b.onclick = () => setView(b.dataset.view)));
  $$('[data-yt]').forEach((b) => (b.onclick = () => call('yt_action', { action: b.dataset.yt })));
  $('#yt-popout').onclick = () => call('yt_popout');
  $('#yt-dock').onclick = () => call('yt_dock');
  listen('yt-popout', ({ payload }) => setPopped(payload));

  $('#ext-form').addEventListener('submit', (e) => { e.preventDefault(); installExtension(); });
  $('#ext-store').onclick = () => navigate('https://chromewebstore.google.com/');

  const langSelect = $('#lang-select');
  const options = [['system', t('settings.languageSystem')], ...LANGUAGES];
  langSelect.replaceChildren(...options.map(([code, name]) => new Option(name, code)));
  langSelect.value = savedLanguage();
  langSelect.onchange = () => setLanguage(langSelect.value);

  const ro = new ResizeObserver(syncLayout);
  ro.observe($('#viewport'));
  ro.observe($('#yt-viewport'));
  document.querySelectorAll('.view').forEach((v) => ro.observe(v));
  window.addEventListener('resize', syncLayout);

  listen('tab-updated', ({ payload }) => {
    const tab = findTab(payload.id);
    if (!tab) return;
    if (payload.url && !payload.url.startsWith('https://orbit-cmd.invalid')) tab.url = payload.url;
    if (payload.title) tab.title = payload.title;
    if (payload.loading !== undefined) tab.loading = payload.loading;
    render();
    persistTabs();
  });
  listen('open-tab', ({ payload }) => newTab(payload, true));
  listen('shortcut', ({ payload }) => runShortcut(payload));

  listen('media', ({ payload }) => {
    media.info = payload;
    media.at = performance.now();
    renderMedia();
  });
  $('#player-toggle').onclick = () => mediaControl('toggle');
  $('#player-prev').onclick = () => mediaControl('prev');
  $('#player-next').onclick = () => mediaControl('next');
  $('#player-source').onclick = goToMediaSource;
  $('#player-seek').onclick = (e) => {
    const i = media.info;
    if (!i || !i.duration) return;
    const r = e.currentTarget.getBoundingClientRect();
    mediaControl('seek', ((e.clientX - r.left) / r.width) * i.duration);
  };
  setInterval(tickMedia, 500);
  renderMedia();
}

// ------------------------------------------------------------ Start
function restoreSession() {
  const s = load('session', null);
  if (s && Array.isArray(s.tabs) && s.tabs.length) {
    // Tabs are only created, not loaded – they load when clicked (saves RAM)
    for (const t of s.tabs) {
      state.tabs.push({ id: state.nextId++, url: t.url, title: t.title || hostOf(t.url || '') || t('tabs.newTitle'), loading: false, live: false });
    }
    const active = state.tabs[Math.max(0, Math.min(s.active, state.tabs.length - 1))];
    activateTab(active.id);
  } else {
    newTab();
  }
}

(async () => {
  await initI18n();
  wire();
  renderStart();
  setView('browser');
  restoreSession();
})();
