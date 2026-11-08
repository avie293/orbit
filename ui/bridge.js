// Orbit – bridge between the UI and the native browser core.
//
// The browser core (Rust + Chromium/CEF) injects `window.cefQuery` into
// Orbit's own UI only. This file wraps it into a small API:
//   invoke(cmd, args)      -> Promise with the result
//   listen(event, handler) -> handler({ payload }) for events from the core
//   appWindow.minimize() / toggleMaximize() / close()

window.OrbitBridge = (() => {
  // Legacy Tauri/WebView2 build (kept until the CEF build is complete)
  if (window.__TAURI__) {
    const w = window.__TAURI__.window.getCurrentWindow();
    return {
      invoke: window.__TAURI__.core.invoke,
      listen: window.__TAURI__.event.listen,
      appWindow: { minimize: () => w.minimize(), toggleMaximize: () => w.toggleMaximize(), close: () => w.close() },
    };
  }

  const handlers = {};
  // Called by the core: window.__orbitEmit(name, payload)
  window.__orbitEmit = (name, payload) => {
    for (const handler of handlers[name] || []) {
      try { handler({ payload }); } catch (e) { console.error(e); }
    }
  };

  function invoke(cmd, args = {}) {
    return new Promise((resolve, reject) => {
      if (typeof window.cefQuery !== 'function') {
        reject('Orbit core not available');
        return;
      }
      window.cefQuery({
        request: JSON.stringify({ cmd, args }),
        persistent: false,
        onSuccess: (response) => resolve(response ? JSON.parse(response) : null),
        onFailure: (_code, message) => reject(message),
      });
    });
  }

  function listen(name, handler) {
    (handlers[name] ||= []).push(handler);
    return Promise.resolve(() => {
      handlers[name] = handlers[name].filter((h) => h !== handler);
    });
  }

  const win = (action) => invoke('win', { action });
  return {
    invoke,
    listen,
    appWindow: { minimize: () => win('minimize'), toggleMaximize: () => win('maximize'), close: () => win('close') },
  };
})();
