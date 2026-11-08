// Orbit – translations
//
// Source language is English (locales/en.json). Translators work on Crowdin,
// which writes locales/<language>.json (e.g. de.json, fr.json, pt-BR.json).
//
// In HTML:  data-i18n="key"              -> textContent
//           data-i18n-title="key"        -> title attribute (tooltip)
//           data-i18n-placeholder="key"  -> placeholder attribute
// In JS:    t('key', { name: 'value' })  -> "{name}" gets replaced

const I18N = { lang: 'en', strings: {}, fallback: {} };

// Languages shown in Settings (native names). Add a line here when Crowdin
// delivers a new language file.
const LANGUAGES = [
  ['en', 'English'],
  ['de', 'Deutsch'],
  ['es', 'Español'],
  ['fr', 'Français'],
  ['it', 'Italiano'],
  ['nl', 'Nederlands'],
  ['pl', 'Polski'],
  ['pt-BR', 'Português (Brasil)'],
  ['tr', 'Türkçe'],
  ['ru', 'Русский'],
  ['ja', '日本語'],
  ['ko', '한국어'],
  ['zh-CN', '简体中文'],
];

/** 'system' or a language code from LANGUAGES */
function savedLanguage() {
  try {
    return JSON.parse(localStorage.getItem('orbit.lang')) || 'system';
  } catch {
    return 'system';
  }
}

function setLanguage(code) {
  try { localStorage.setItem('orbit.lang', JSON.stringify(code)); } catch { /* ignore */ }
  location.reload();
}

async function fetchJson(path) {
  try {
    const res = await fetch(path);
    return res.ok ? await res.json() : null;
  } catch {
    return null;
  }
}

function preferredLanguage() {
  const saved = savedLanguage();
  return saved === 'system' ? navigator.language || 'en' : saved;
}

async function initI18n() {
  I18N.fallback = (await fetchJson('locales/en.json')) || {};
  const pref = preferredLanguage();
  // e.g. "pt-BR" -> try pt-BR.json, then pt.json
  for (const code of [pref, pref.split('-')[0]]) {
    if (code === 'en') break;
    const data = await fetchJson(`locales/${code}.json`);
    if (data) {
      I18N.strings = data;
      I18N.lang = code;
      break;
    }
  }
  document.documentElement.lang = I18N.lang;
  applyI18n(document);
}

function t(key, vars = {}) {
  const text = I18N.strings[key] || I18N.fallback[key] || key;
  return text.replace(/\{(\w+)\}/g, (m, name) => (name in vars ? vars[name] : m));
}

function applyI18n(root) {
  root.querySelectorAll('[data-i18n]').forEach((el) => (el.textContent = t(el.dataset.i18n)));
  root.querySelectorAll('[data-i18n-title]').forEach((el) => (el.title = t(el.dataset.i18nTitle)));
  root.querySelectorAll('[data-i18n-placeholder]').forEach((el) => (el.placeholder = t(el.dataset.i18nPlaceholder)));
}
