// Orbit – file explorer (UI). The heavy lifting (listing, copying, searching,
// folder sizes) happens in the native core on worker threads (fs_* commands).

const fx = {
  ready: false,
  path: null,
  history: [],
  index: -1,
  entries: [],
  shown: [], // entries currently displayed (listing or search results)
  selected: new Set(),
  anchor: null, // last clicked path (for shift-select)
  sort: { key: 'name', dir: 1 },
  hidden: false,
  clipboard: null, // { paths, mode: 'copy' | 'move' }
  sizes: new Map(), // folder path -> bytes
  search: { id: 0, query: '', results: [], done: true },
  previewOpen: false,
  places: null,
};

const FILE_KINDS = {
  image: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'svg', 'ico', 'avif', 'heic', 'tif', 'tiff', 'psd'],
  video: ['mp4', 'mkv', 'webm', 'mov', 'avi', 'wmv', 'flv', 'm4v'],
  audio: ['mp3', 'wav', 'flac', 'ogg', 'm4a', 'aac', 'wma', 'opus'],
  archive: ['zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz', 'iso'],
  code: ['js', 'ts', 'jsx', 'tsx', 'rs', 'py', 'java', 'c', 'h', 'cpp', 'cs', 'go', 'php', 'rb', 'html', 'css', 'json', 'xml', 'yml', 'yaml', 'toml', 'sh', 'ps1', 'bat', 'sql', 'md'],
  doc: ['pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'odt', 'ods', 'txt', 'rtf', 'csv'],
  app: ['exe', 'msi', 'lnk', 'dll', 'appx'],
};
const KIND_COLORS = {
  folder: '#f5c451', image: '#f472b6', video: '#a78bfa', audio: '#22d3ee', archive: '#fb923c',
  code: '#4ade80', doc: '#60a5fa', app: '#94a3b8', file: '#8b8b9c',
};

function kindOf(e) {
  if (e.dir) return 'folder';
  for (const [kind, exts] of Object.entries(FILE_KINDS)) if (exts.includes(e.ext)) return kind;
  return 'file';
}

function fmtBytes(n) {
  if (n == null) return '';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
  return (i === 0 ? n : n.toFixed(n < 10 ? 1 : 0)) + ' ' + units[i];
}

function fmtDate(ms) {
  if (!ms) return '';
  return new Date(ms).toLocaleString(I18N.lang, { dateStyle: 'medium', timeStyle: 'short' });
}

function typeLabel(e) {
  if (e.dir) return t('files.typeFolder');
  return e.ext ? t('files.typeFile', { ext: e.ext.toUpperCase() }) : t('files.typeFileNoExt');
}

const sep = '\\';
function joinPath(dir, name) { return dir.endsWith(sep) ? dir + name : dir + sep + name; }
function parentOf(p) {
  const trimmed = p.replace(/\\+$/, '');
  const i = trimmed.lastIndexOf(sep);
  if (i < 0) return null;
  return i <= 2 ? trimmed.slice(0, 3).replace(/\\?$/, sep) : trimmed.slice(0, i);
}

function fxStatus(text, isError = false) {
  const el = $('#fx-status-right');
  el.textContent = text || '';
  el.classList.toggle('err', isError);
}

// ------------------------------------------------------------ navigation

async function fxGo(path, push = true) {
  if (!path) return;
  fxClearSearch(false);
  try {
    const res = await invoke('fs_list', { path, hidden: fx.hidden });
    fx.path = res.path;
    fx.entries = res.entries;
    fx.selected.clear();
    fx.anchor = null;
    if (push) {
      fx.history = fx.history.slice(0, fx.index + 1);
      fx.history.push(fx.path);
      fx.index = fx.history.length - 1;
    }
    save('fx.last', fx.path);
    fxStatus('');
    fxRender();
  } catch (e) {
    fxStatus(t('files.error') + ' – ' + e, true);
  }
}

function fxBack() { if (fx.index > 0) fxGo(fx.history[--fx.index], false); }
function fxForward() { if (fx.index < fx.history.length - 1) fxGo(fx.history[++fx.index], false); }
function fxUp() { const p = fx.path && parentOf(fx.path); if (p && p !== fx.path) fxGo(p); }
function fxRefresh() { if (fx.path) fxGo(fx.path, false); }

// ------------------------------------------------------------ rendering

function sortedEntries(list) {
  const { key, dir } = fx.sort;
  const val = (e) => {
    switch (key) {
      case 'modified': return e.modified || 0;
      case 'size': return e.dir ? fx.sizes.get(e.path) ?? -1 : e.size ?? 0;
      case 'type': return e.dir ? '' : e.ext;
      default: return e.name.toLowerCase();
    }
  };
  return [...list].sort((a, b) => {
    if (key !== 'size' && a.dir !== b.dir) return a.dir ? -1 : 1; // folders first
    const va = val(a), vb = val(b);
    const c = typeof va === 'number' ? va - vb : String(va).localeCompare(String(vb), undefined, { numeric: true });
    return c * dir;
  });
}

function fxRender() {
  const searching = !!fx.search.query;
  fx.shown = sortedEntries(searching ? fx.search.results : fx.entries);
  const maxDirSize = Math.max(1, ...fx.shown.filter((e) => e.dir).map((e) => fx.sizes.get(e.path) || 0));

  const list = $('#fx-list');
  const frag = document.createDocumentFragment();
  for (const e of fx.shown) {
    const row = document.createElement('div');
    row.className = 'fx-row' + (fx.selected.has(e.path) ? ' sel' : '') + (e.hidden ? ' hidden-file' : '');
    row.dataset.path = e.path;
    const kind = kindOf(e);
    const size = e.dir ? fx.sizes.get(e.path) : e.size;
    const bar = e.dir && fx.sizes.has(e.path)
      ? `<span class="fx-sizebar" style="width:${Math.max(2, (fx.sizes.get(e.path) / maxDirSize) * 100)}%"></span>` : '';
    row.innerHTML = `
      <span class="fx-name"><svg style="color:${KIND_COLORS[kind]}"><use href="#${e.dir ? 'i-folder' : 'i-file'}"/></svg><span class="fx-label"></span></span>
      <span class="fx-cell">${fmtDate(e.modified)}</span>
      <span class="fx-cell fx-type"></span>
      <span class="fx-cell fx-size">${bar}<span>${size != null ? fmtBytes(size) : ''}</span></span>`;
    row.querySelector('.fx-label').textContent = e.name;
    // in search results show where the file is
    row.querySelector('.fx-type').textContent = searching ? parentOf(e.path) || '' : typeLabel(e);
    frag.appendChild(row);
  }
  list.replaceChildren(frag);
  if (!fx.shown.length) {
    const empty = document.createElement('div');
    empty.className = 'fx-empty';
    empty.textContent = searching ? (fx.search.done ? t('files.noResults') : t('files.searching')) : t('files.empty');
    list.appendChild(empty);
  }

  // header sort indicators
  $$('#fx-head [data-sort]').forEach((b) => {
    b.classList.toggle('active', b.dataset.sort === fx.sort.key);
    b.dataset.dir = fx.sort.dir > 0 ? '↑' : '↓';
  });
  $('#fx-head [data-sort="type"]').textContent = t(searching ? 'files.col.location' : 'files.col.type');

  fxRenderCrumbs();
  fxRenderStatus();
  $('#fx-back').disabled = fx.index <= 0;
  $('#fx-fwd').disabled = fx.index >= fx.history.length - 1;
  $('#fx-up').disabled = !fx.path || parentOf(fx.path) === null;
  $('#fx-hidden').classList.toggle('on', fx.hidden);
  $('#fx-preview-btn').classList.toggle('on', fx.previewOpen);
  fxUpdatePreview();
}

function fxRenderCrumbs() {
  const box = $('#fx-crumbs');
  box.replaceChildren();
  if (!fx.path) return;
  const parts = fx.path.split(sep).filter(Boolean);
  let acc = '';
  parts.forEach((part, i) => {
    acc = i === 0 ? part + sep : joinPath(acc, part);
    const target = acc;
    const b = document.createElement('button');
    b.className = 'fx-crumb';
    b.textContent = part;
    b.onclick = (ev) => { ev.stopPropagation(); fxGo(target); };
    if (i) box.appendChild(Object.assign(document.createElement('span'), { className: 'fx-crumb-sep', textContent: '›' }));
    box.appendChild(b);
  });
}

function fxRenderStatus() {
  const count = fx.shown.length;
  let left = fx.search.query
    ? (fx.search.done ? t('files.searchResults', { count }) : t('files.searching') + ' ' + count)
    : t('files.items', { count });
  if (fx.selected.size) {
    const bytes = [...fx.selected].map((p) => fx.shown.find((e) => e.path === p)).filter(Boolean)
      .reduce((sum, e) => sum + (e.dir ? fx.sizes.get(e.path) || 0 : e.size || 0), 0);
    left += ' · ' + t('files.selected', { count: fx.selected.size }) + (bytes ? ' · ' + fmtBytes(bytes) : '');
  }
  $('#fx-status-left').textContent = left;
}

async function fxLoadPlaces() {
  try { fx.places = await invoke('fs_places'); } catch { return; }
  const quick = $('#fx-quick');
  quick.replaceChildren(...fx.places.quick.map((q) => fxPlaceButton(t('files.place.' + q.id), q.path, 'i-folder')));
  const drives = $('#fx-drives');
  drives.replaceChildren(...fx.places.drives.map((d) => {
    const label = (d.label ? d.label + ' ' : '') + '(' + d.path.replace(/\\$/, '') + ')';
    const b = fxPlaceButton(label, d.path, 'i-drive');
    if (d.total) {
      const used = 1 - d.free / d.total;
      const info = document.createElement('div');
      info.className = 'fx-drive-info';
      info.innerHTML = `<div class="fx-drive-bar"><span style="width:${(used * 100).toFixed(1)}%" class="${used > 0.9 ? 'full' : ''}"></span></div>`;
      const txt = document.createElement('div');
      txt.className = 'muted small';
      txt.textContent = t('files.freeOf', { free: fmtBytes(d.free), total: fmtBytes(d.total) });
      info.appendChild(txt);
      b.appendChild(info);
    }
    return b;
  }));
}

function fxPlaceButton(label, path, icon) {
  const b = document.createElement('button');
  b.className = 'fx-place';
  b.dataset.path = path;
  b.innerHTML = `<div class="fx-place-row"><svg><use href="#${icon}"/></svg><span></span></div>`;
  b.querySelector('span').textContent = label;
  b.onclick = () => fxGo(path);
  return b;
}

// ------------------------------------------------------------ selection & actions

function fxSelectedEntries() {
  return fx.shown.filter((e) => fx.selected.has(e.path));
}

function fxSelect(path, { ctrl = false, shift = false } = {}) {
  if (shift && fx.anchor) {
    const paths = fx.shown.map((e) => e.path);
    const [a, b] = [paths.indexOf(fx.anchor), paths.indexOf(path)].sort((x, y) => x - y);
    if (!ctrl) fx.selected.clear();
    paths.slice(a, b + 1).forEach((p) => fx.selected.add(p));
  } else if (ctrl) {
    fx.selected.has(path) ? fx.selected.delete(path) : fx.selected.add(path);
    fx.anchor = path;
  } else {
    fx.selected = new Set([path]);
    fx.anchor = path;
  }
  $$('#fx-list .fx-row').forEach((r) => r.classList.toggle('sel', fx.selected.has(r.dataset.path)));
  fxRenderStatus();
  fxUpdatePreview();
}

function fxMoveSelection(delta, shift) {
  if (!fx.shown.length) return;
  const paths = fx.shown.map((e) => e.path);
  const current = fx.anchor ? paths.indexOf(fx.anchor) : -1;
  const next = Math.max(0, Math.min(paths.length - 1, current + delta));
  if (shift) {
    const keepAnchor = fx.anchor;
    fxSelect(paths[next], { shift: true });
    fx.anchor = keepAnchor;
  } else {
    fxSelect(paths[next]);
  }
  $(`#fx-list .fx-row[data-path="${CSS.escape(paths[next])}"]`)?.scrollIntoView({ block: 'nearest' });
}

async function fxOpen(e) {
  if (!e) return;
  if (e.dir) return fxGo(e.path);
  try { await invoke('fs_open', { path: e.path }); } catch (err) { fxStatus(String(err), true); }
}

async function fxNewFolder() {
  if (!fx.path) return;
  try {
    const created = await invoke('fs_mkdir', { path: fx.path, name: t('files.newFolderName') });
    await fxGo(fx.path, false);
    fxSelect(created);
    fxRename(created);
  } catch (e) { fxStatus(String(e), true); }
}

function fxRename(path) {
  const row = $(`#fx-list .fx-row[data-path="${CSS.escape(path)}"]`);
  const entry = fx.shown.find((e) => e.path === path);
  if (!row || !entry) return;
  const label = row.querySelector('.fx-label');
  const input = document.createElement('input');
  input.className = 'fx-rename';
  input.value = entry.name;
  label.replaceWith(input);
  input.focus();
  // select the name without the extension
  const dot = entry.dir ? -1 : entry.name.lastIndexOf('.');
  input.setSelectionRange(0, dot > 0 ? dot : entry.name.length);
  let done = false;
  const finish = async (commit) => {
    if (done) return;
    done = true;
    const name = input.value.trim();
    if (commit && name && name !== entry.name) {
      try {
        const newPath = await invoke('fs_rename', { path, name });
        await fxGo(fx.path, false);
        fxSelect(newPath);
      } catch (e) {
        fxStatus(e === 'exists' ? t('files.nameExists') : String(e), true);
        fxRender();
      }
    } else {
      fxRender();
    }
    $('#fx-list').focus();
  };
  input.addEventListener('keydown', (ev) => {
    ev.stopPropagation();
    if (ev.key === 'Enter') finish(true);
    if (ev.key === 'Escape') finish(false);
  });
  input.addEventListener('blur', () => finish(true));
}

async function fxDelete() {
  const paths = [...fx.selected];
  if (!paths.length) return;
  try {
    await invoke('fs_delete', { paths });
    fxStatus(t('files.movedToBin', { count: paths.length }));
    fxRefresh();
  } catch (e) { fxStatus(String(e), true); }
}

function fxCopy(mode) {
  const paths = [...fx.selected];
  if (!paths.length) return;
  fx.clipboard = { paths, mode };
  fxStatus(t(mode === 'move' ? 'files.cutDone' : 'files.copyDone', { count: paths.length }));
}

async function fxPaste() {
  if (!fx.clipboard || !fx.path) return;
  fxStatus(t('files.working'));
  try {
    await invoke('fs_paste', { paths: fx.clipboard.paths, dest: fx.path, mode: fx.clipboard.mode });
    if (fx.clipboard.mode === 'move') fx.clipboard = null;
    fxStatus('');
    fxRefresh();
  } catch (e) { fxStatus(String(e), true); }
}

async function fxCalcSize(paths) {
  for (const path of paths) {
    try {
      const res = await invoke('fs_size', { path });
      fx.sizes.set(path, res.size);
      fxRender();
    } catch { /* unreadable folder */ }
  }
}

function fxCalcAllSizes() {
  const dirs = fx.shown.filter((e) => e.dir && !fx.sizes.has(e.path)).map((e) => e.path);
  // two parallel workers keep the disk busy without overloading it
  const half = Math.ceil(dirs.length / 2);
  fxCalcSize(dirs.slice(0, half));
  fxCalcSize(dirs.slice(half));
}

// ------------------------------------------------------------ search

let fxSearchTimer;
function fxStartSearch(query) {
  clearTimeout(fxSearchTimer);
  if (!query.trim()) return fxClearSearch(true);
  fxSearchTimer = setTimeout(() => {
    fx.search = { id: fx.search.id + 1, query, results: [], done: false };
    fx.selected.clear();
    fxRender();
    invoke('fs_search', { path: fx.path, query, id: fx.search.id }).catch((e) => fxStatus(String(e), true));
  }, 250);
}

function fxClearSearch(render) {
  if (fx.search.query) invoke('fs_search_cancel').catch(() => {});
  fx.search = { id: fx.search.id + 1, query: '', results: [], done: true };
  if ($('#fx-search')) $('#fx-search').value = '';
  if (render) fxRender();
}

function fxOnSearchResults({ payload }) {
  if (payload.id !== fx.search.id) return; // an older search
  fx.search.results.push(...payload.results);
  fx.search.done = payload.done;
  fxRender();
}

// ------------------------------------------------------------ preview (Space)

let fxPreviewFor = null;
async function fxUpdatePreview() {
  const pane = $('#fx-preview');
  pane.hidden = !fx.previewOpen;
  if (!fx.previewOpen) return;
  const [entry] = fxSelectedEntries();
  if (!entry) {
    fxPreviewFor = null;
    pane.innerHTML = `<div class="fx-empty">${escapeHtml(t('files.previewHint'))}</div>`;
    return;
  }
  if (fxPreviewFor === entry.path) return;
  fxPreviewFor = entry.path;
  let res;
  try { res = await invoke('fs_preview', { path: entry.path }); } catch { res = { kind: 'other', entry }; }
  if (fxPreviewFor !== entry.path) return; // selection changed meanwhile

  const e = res.entry || entry;
  const head = `<div class="fx-pv-name"></div><div class="fx-pv-meta muted small"></div>`;
  let body = '';
  if (res.kind === 'image') {
    body = `<div class="fx-pv-media"><img alt="" src="orbit://app/__file?path=${encodeURIComponent(e.path)}"></div>`;
  } else if (res.kind === 'text') {
    body = `<pre class="fx-pv-text"></pre>` + (res.truncated ? `<div class="muted small">${escapeHtml(t('files.previewTruncated'))}</div>` : '');
  } else {
    body = `<div class="fx-pv-icon"><svg style="color:${KIND_COLORS[kindOf(e)]}"><use href="#${e.dir ? 'i-folder' : 'i-file'}"/></svg></div>`
      + (e.dir ? '' : `<div class="fx-empty small">${escapeHtml(t('files.noPreview'))}</div>`);
  }
  pane.innerHTML = body + head + `<button class="pill-btn fx-pv-open"><svg><use href="#i-open"/></svg><span></span></button>`;
  pane.querySelector('.fx-pv-name').textContent = e.name;
  const size = e.dir ? (fx.sizes.has(e.path) ? fmtBytes(fx.sizes.get(e.path)) : '') : fmtBytes(e.size);
  pane.querySelector('.fx-pv-meta').textContent = [typeLabel(e), size, fmtDate(e.modified)].filter(Boolean).join(' · ');
  if (res.kind === 'text') pane.querySelector('.fx-pv-text').textContent = res.text;
  const open = pane.querySelector('.fx-pv-open');
  open.querySelector('span').textContent = t('files.open');
  open.onclick = () => fxOpen(e);
}

function fxTogglePreview() {
  fx.previewOpen = !fx.previewOpen;
  save('fx.preview', fx.previewOpen);
  fxPreviewFor = null;
  fxRender();
}

// ------------------------------------------------------------ context menu

function fxShowMenu(x, y) {
  const menu = $('#fx-menu');
  const sel = fxSelectedEntries();
  const one = sel.length === 1 ? sel[0] : null;
  const items = [];
  if (one) items.push(['files.open', () => fxOpen(one)]);
  if (one) items.push(['files.rename', () => fxRename(one.path)]);
  if (sel.length) {
    items.push('-');
    items.push(['files.copy', () => fxCopy('copy')], ['files.cut', () => fxCopy('move')]);
  }
  if (fx.clipboard) items.push(['files.paste', fxPaste]);
  if (sel.some((e) => e.dir)) items.push('-', ['files.calcSize', () => fxCalcSize(sel.filter((e) => e.dir).map((e) => e.path))]);
  if (one) {
    items.push('-');
    items.push(['files.copyPath', () => navigator.clipboard.writeText(one.path).catch(() => {})]);
    items.push(['files.reveal', () => invoke('fs_reveal', { path: one.path })]);
  }
  if (!sel.length) items.push(['files.newFolder', fxNewFolder], ['files.refresh', fxRefresh]);
  if (sel.length) items.push('-', ['files.delete', fxDelete, 'danger']);

  menu.replaceChildren(...items.map((it) => {
    if (it === '-') return Object.assign(document.createElement('div'), { className: 'fx-menu-sep' });
    const b = document.createElement('button');
    b.textContent = t(it[0]);
    if (it[2]) b.classList.add(it[2]);
    b.onclick = () => { fxHideMenu(); it[1](); };
    return b;
  }));
  menu.hidden = false;
  const host = $('#view-explorer').getBoundingClientRect();
  const mw = menu.offsetWidth, mh = menu.offsetHeight;
  menu.style.left = Math.min(x - host.left, host.width - mw - 8) + 'px';
  menu.style.top = Math.min(y - host.top, host.height - mh - 8) + 'px';
}

function fxHideMenu() { $('#fx-menu').hidden = true; }

// ------------------------------------------------------------ keyboard

function fxKeydown(ev) {
  if (state.view !== 'explorer' || ev.target.matches('input')) return;
  const ctrl = ev.ctrlKey || ev.metaKey;
  const [first] = fxSelectedEntries();
  const k = ev.key;
  let handled = true;
  if (k === 'ArrowDown') fxMoveSelection(1, ev.shiftKey);
  else if (k === 'ArrowUp') fxMoveSelection(-1, ev.shiftKey);
  else if (k === 'Enter') fxOpen(first);
  else if (k === 'Backspace') fxUp();
  else if (k === 'F2' && first) fxRename(first.path);
  else if (k === 'Delete') fxDelete();
  else if (k === ' ') fxTogglePreview();
  else if (ctrl && k.toLowerCase() === 'a') { fx.selected = new Set(fx.shown.map((e) => e.path)); fxRender(); }
  else if (ctrl && k.toLowerCase() === 'c') fxCopy('copy');
  else if (ctrl && k.toLowerCase() === 'x') fxCopy('move');
  else if (ctrl && k.toLowerCase() === 'v') fxPaste();
  else if (ctrl && k.toLowerCase() === 'f') $('#fx-search').focus();
  else if (ctrl && ev.shiftKey && k.toLowerCase() === 'n') fxNewFolder();
  else if (k === 'Escape') { fxHideMenu(); if (fx.search.query) fxClearSearch(true); }
  else handled = false;
  if (handled) ev.preventDefault();
}

// Shortcuts the core sends while the UI has focus (Alt+←/→, F5)
function fxShortcut(cmd) {
  if (cmd === 'back') fxBack();
  else if (cmd === 'forward') fxForward();
  else if (cmd === 'reload') fxRefresh();
}

// ------------------------------------------------------------ setup

function fxShow() {
  if (fx.ready) return;
  fx.ready = true;
  fxLoadPlaces().then(() => fxGo(load('fx.last', null) || fx.places?.home || 'C:\\'));
}

function fxWire() {
  fx.hidden = load('fx.hidden', false);
  fx.previewOpen = load('fx.preview', false);

  $('#fx-back').onclick = fxBack;
  $('#fx-fwd').onclick = fxForward;
  $('#fx-up').onclick = fxUp;
  $('#fx-refresh').onclick = fxRefresh;
  $('#fx-newfolder').onclick = fxNewFolder;
  $('#fx-sizes').onclick = fxCalcAllSizes;
  $('#fx-preview-btn').onclick = fxTogglePreview;
  $('#fx-hidden').onclick = () => { fx.hidden = !fx.hidden; save('fx.hidden', fx.hidden); fxRefresh(); };

  // path bar: click empty area -> editable path
  const pathbox = $('#fx-pathbox'), pathInput = $('#fx-path'), crumbs = $('#fx-crumbs');
  pathbox.onclick = () => {
    crumbs.hidden = true;
    pathInput.hidden = false;
    pathInput.value = fx.path || '';
    pathInput.focus();
    pathInput.select();
  };
  const closePath = () => { pathInput.hidden = true; crumbs.hidden = false; };
  pathInput.addEventListener('keydown', (e) => {
    e.stopPropagation();
    if (e.key === 'Enter') { closePath(); fxGo(pathInput.value.trim()); }
    if (e.key === 'Escape') closePath();
  });
  pathInput.addEventListener('blur', closePath);

  $('#fx-search-form').addEventListener('submit', (e) => e.preventDefault());
  $('#fx-search').addEventListener('input', (e) => fxStartSearch(e.target.value));
  $('#fx-search').addEventListener('keydown', (e) => { if (e.key === 'Escape') fxClearSearch(true); });

  $$('#fx-head [data-sort]').forEach((b) => (b.onclick = () => {
    const key = b.dataset.sort;
    fx.sort = { key, dir: fx.sort.key === key ? -fx.sort.dir : 1 };
    fxRender();
  }));

  const list = $('#fx-list');
  list.addEventListener('mousedown', (e) => {
    const row = e.target.closest('.fx-row');
    if (e.target.matches('.fx-rename')) return;
    if (!row) { if (e.button === 0) { fx.selected.clear(); fxRender(); } return; }
    if (e.button === 2 && fx.selected.has(row.dataset.path)) return; // keep multi-selection for the menu
    fxSelect(row.dataset.path, { ctrl: e.ctrlKey, shift: e.shiftKey });
  });
  list.addEventListener('dblclick', (e) => {
    const row = e.target.closest('.fx-row');
    if (row) fxOpen(fx.shown.find((x) => x.path === row.dataset.path));
  });
  list.addEventListener('contextmenu', (e) => { e.preventDefault(); fxShowMenu(e.clientX, e.clientY); });
  document.addEventListener('mousedown', (e) => { if (!e.target.closest('#fx-menu')) fxHideMenu(); });
  document.addEventListener('keydown', fxKeydown);

  listen('fs-search', fxOnSearchResults);
}
