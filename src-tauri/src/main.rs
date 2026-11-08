// Orbit – Browser, Explorer & Media in einer leichten App.
//
// Aufbau: Ein Fenster ("main"). Darin liegt die Shell-Webview (unsere UI aus /ui)
// über die ganze Fläche. Jeder Browser-Tab und das YouTube-Panel sind eigene
// Chromium-Webviews (WebView2), die wir exakt über die Platzhalter der Shell legen.
// Die Shell meldet per `set_layout`, wo diese Platzhalter gerade sind.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod extensions;

use std::{collections::HashSet, sync::Mutex};

use serde::{Deserialize, Serialize};
use tauri::{
    webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder},
    window::WindowBuilder,
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, State, Url, Webview, WebviewUrl,
    WindowEvent,
};

const YT_LABEL: &str = "yt";
const YT_POPUP: &str = "yt-popup";
const YT_HOME: &str = "https://www.youtube.com/";
/// Webseiten können keine IPC-Aufrufe machen. Damit Shortcuts (Strg+T usw.) auch
/// funktionieren, wenn der Fokus in einer Webseite liegt, "navigiert" ein kleines
/// Skript zu dieser Fake-Adresse – wir fangen das ab, brechen ab und melden es der Shell.
const CMD_HOST: &str = "orbit-cmd.invalid";

const SHORTCUT_SCRIPT: &str = r#"
(() => {
  if (window.top !== window) return;
  addEventListener('keydown', (e) => {
    const k = e.key.toLowerCase();
    let c = null;
    if (e.ctrlKey && e.shiftKey && k === 'tab') c = 'prev-tab';
    else if (e.ctrlKey && e.shiftKey && k === 'y') c = 'toggle-yt';
    else if (e.ctrlKey && !e.shiftKey && !e.altKey) {
      if (k === 't') c = 'new-tab';
      else if (k === 'w') c = 'close-tab';
      else if (k === 'l') c = 'focus-address';
      else if (k === 'tab') c = 'next-tab';
    }
    if (c) {
      e.preventDefault();
      e.stopPropagation();
      location.href = 'https://orbit-cmd.invalid/' + c;
    }
  }, true);
})();
"#;

#[derive(Clone, Copy, Default, Deserialize, Debug)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Default)]
struct Inner {
    tabs: HashSet<u32>,
    active: Option<u32>,
    content: Rect,
    yt: Option<Rect>,
    /// YouTube läuft gerade im eigenen Pop-out-Fenster
    yt_popped: bool,
    browser_visible: bool,
}

#[derive(Default)]
struct AppState(Mutex<Inner>);

#[derive(Clone, Serialize)]
struct TabUpdate {
    id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    loading: Option<bool>,
}

fn tab_label(id: u32) -> String {
    format!("tab-{id}")
}

fn place(wv: &Webview, r: Rect) -> tauri::Result<()> {
    wv.set_position(LogicalPosition::new(r.x, r.y))?;
    wv.set_size(LogicalSize::new(r.w.max(1.0), r.h.max(1.0)))?;
    Ok(())
}

/// Zeigt nur den aktiven Tab (und ggf. YouTube) an der richtigen Stelle, versteckt den Rest.
fn apply_layout(app: &AppHandle, s: &Inner) {
    for id in &s.tabs {
        let Some(wv) = app.get_webview(&tab_label(*id)) else { continue };
        if s.browser_visible && s.active == Some(*id) {
            let _ = place(&wv, s.content);
            let _ = wv.show();
        } else {
            let _ = wv.hide();
        }
    }
    if s.yt_popped {
        return; // im Pop-out-Fenster kümmert sich auto_resize um die Größe
    }
    if let Some(wv) = app.get_webview(YT_LABEL) {
        match s.yt {
            Some(r) => {
                let _ = place(&wv, r);
                let _ = wv.show();
            }
            None => {
                let _ = wv.hide();
            }
        }
    }
}

/// Gemeinsame Navigations-Logik: Shortcut-Befehle abfangen, sonst erlauben.
fn intercept_command(app: &AppHandle, url: &Url) -> bool {
    if url.host_str() == Some(CMD_HOST) {
        let cmd = url.path().trim_start_matches('/').to_string();
        let _ = app.emit("shortcut", cmd);
        return false;
    }
    true
}

/// Alle Webviews teilen sich eine WebView2-Umgebung – das Erweiterungs-Flag muss
/// deshalb überall gleich gesetzt sein (auch bei der Shell), sonst schlägt die Erstellung fehl.
fn with_extensions(app: &AppHandle, b: WebviewBuilder<tauri::Wry>) -> WebviewBuilder<tauri::Wry> {
    b.browser_extensions_enabled(true).extensions_path(extensions::ext_dir(app))
}

fn tab_builder(app: &AppHandle, id: u32, url: Url) -> WebviewBuilder<tauri::Wry> {
    let (a1, a2, a3, a4) = (app.clone(), app.clone(), app.clone(), app.clone());
    with_extensions(app, WebviewBuilder::new(tab_label(id), WebviewUrl::External(url)))
        .initialization_script(SHORTCUT_SCRIPT)
        .on_navigation(move |url| {
            let ok = intercept_command(&a1, url);
            if ok {
                let _ = a1.emit(
                    "tab-updated",
                    TabUpdate { id, url: Some(url.to_string()), title: None, loading: Some(true) },
                );
            }
            ok
        })
        .on_page_load(move |_wv, p| {
            let loading = matches!(p.event(), PageLoadEvent::Started);
            let _ = a2.emit(
                "tab-updated",
                TabUpdate { id, url: Some(p.url().to_string()), title: None, loading: Some(loading) },
            );
        })
        .on_document_title_changed(move |_wv, title| {
            let _ = a3.emit("tab-updated", TabUpdate { id, url: None, title: Some(title), loading: None });
        })
        .on_new_window(move |url, _features| {
            // target=_blank / window.open -> als neuen Tab in Orbit öffnen
            let _ = a4.emit("open-tab", url.to_string());
            NewWindowResponse::Deny
        })
}

fn yt_builder(app: &AppHandle) -> WebviewBuilder<tauri::Wry> {
    let (a1, a2) = (app.clone(), app.clone());
    with_extensions(app, WebviewBuilder::new(YT_LABEL, WebviewUrl::External(Url::parse(YT_HOME).unwrap())))
        .initialization_script(SHORTCUT_SCRIPT)
        .on_navigation(move |url| intercept_command(&a1, url))
        .on_new_window(move |url, _features| {
            let _ = a2.emit("open-tab", url.to_string());
            NewWindowResponse::Deny
        })
}

fn get_tab(app: &AppHandle, id: u32) -> Result<Webview, String> {
    app.get_webview(&tab_label(id)).ok_or_else(|| format!("Tab {id} does not exist"))
}

fn parse_url(url: &str) -> Result<Url, String> {
    Url::parse(url).map_err(|e| format!("Invalid URL: {e}"))
}

// ---------------------------------------------------------------- Commands

#[tauri::command]
async fn tab_create(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u32,
    url: String,
    activate: bool,
) -> Result<(), String> {
    let url = parse_url(&url)?;
    if app.get_webview(&tab_label(id)).is_some() {
        return Err(format!("Tab {id} already exists"));
    }
    let window = app.get_window("main").ok_or("Main window is missing")?;
    let r = state.0.lock().unwrap().content;
    window
        .add_child(tab_builder(&app, id, url), LogicalPosition::new(r.x, r.y), LogicalSize::new(r.w.max(1.0), r.h.max(1.0)))
        .map_err(|e| e.to_string())?;

    let mut s = state.0.lock().unwrap();
    s.tabs.insert(id);
    if activate {
        s.active = Some(id);
    }
    apply_layout(&app, &s);
    Ok(())
}

#[tauri::command]
async fn tab_close(app: AppHandle, state: State<'_, AppState>, id: u32) -> Result<(), String> {
    {
        let mut s = state.0.lock().unwrap();
        s.tabs.remove(&id);
        if s.active == Some(id) {
            s.active = None;
        }
    }
    if let Some(wv) = app.get_webview(&tab_label(id)) {
        wv.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `id = None` heißt: kein Webview zeigen (z.B. Startseite eines neuen Tabs).
#[tauri::command]
async fn tab_activate(app: AppHandle, state: State<'_, AppState>, id: Option<u32>) -> Result<(), String> {
    let mut s = state.0.lock().unwrap();
    s.active = id.filter(|i| s.tabs.contains(i));
    apply_layout(&app, &s);
    Ok(())
}

#[tauri::command]
async fn tab_navigate(app: AppHandle, id: u32, url: String) -> Result<(), String> {
    get_tab(&app, id)?.navigate(parse_url(&url)?).map_err(|e| e.to_string())
}

#[tauri::command]
async fn tab_action(app: AppHandle, id: u32, action: String) -> Result<(), String> {
    let wv = get_tab(&app, id)?;
    let res = match action.as_str() {
        "back" => wv.eval("history.back()"),
        "forward" => wv.eval("history.forward()"),
        "reload" => wv.reload(),
        "stop" => wv.eval("window.stop()"),
        other => return Err(format!("Unknown action: {other}")),
    };
    res.map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_layout(
    app: AppHandle,
    state: State<'_, AppState>,
    content: Rect,
    yt: Option<Rect>,
    browser_visible: bool,
) -> Result<(), String> {
    // YouTube-Webview erst erzeugen, wenn die Ansicht zum ersten Mal geöffnet wird (spart RAM).
    if let Some(r) = yt {
        let popped = state.0.lock().unwrap().yt_popped;
        if !popped && app.get_webview(YT_LABEL).is_none() {
            let window = app.get_window("main").ok_or("Main window is missing")?;
            window
                .add_child(yt_builder(&app), LogicalPosition::new(r.x, r.y), LogicalSize::new(r.w.max(1.0), r.h.max(1.0)))
                .map_err(|e| e.to_string())?;
        }
    }
    let mut s = state.0.lock().unwrap();
    s.content = content;
    s.yt = yt;
    s.browser_visible = browser_visible;
    apply_layout(&app, &s);
    Ok(())
}

#[tauri::command]
async fn yt_action(app: AppHandle, action: String) -> Result<(), String> {
    let wv = app.get_webview(YT_LABEL).ok_or("YouTube is not open")?;
    let res = match action.as_str() {
        "home" => wv.navigate(Url::parse(YT_HOME).unwrap()),
        "back" => wv.eval("history.back()"),
        "reload" => wv.reload(),
        other => return Err(format!("Unknown action: {other}")),
    };
    res.map_err(|e| e.to_string())
}

/// YouTube in ein eigenes, normales Fenster verschieben. Die Webview wird nur
/// umgehängt (reparent), nicht neu geladen – ein laufendes Video spielt weiter.
#[tauri::command]
async fn yt_popout(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(p) = app.get_window(YT_POPUP) {
        let _ = p.unminimize();
        return p.set_focus().map_err(|e| e.to_string());
    }
    let popup = WindowBuilder::new(&app, YT_POPUP)
        .title("YouTube – Orbit")
        .inner_size(1100.0, 680.0)
        .min_inner_size(420.0, 260.0)
        .center()
        .build()
        .map_err(|e| e.to_string())?;
    let scale = popup.scale_factor().map_err(|e| e.to_string())?;
    let size = popup.inner_size().map_err(|e| e.to_string())?.to_logical::<f64>(scale);

    match app.get_webview(YT_LABEL) {
        Some(wv) => {
            wv.reparent(&popup).map_err(|e| e.to_string())?;
            let _ = wv.set_position(LogicalPosition::new(0.0, 0.0));
            let _ = wv.set_size(size);
            let _ = wv.set_auto_resize(true);
            let _ = wv.show();
        }
        None => {
            popup
                .add_child(yt_builder(&app).auto_resize(), LogicalPosition::new(0.0, 0.0), size)
                .map_err(|e| e.to_string())?;
        }
    }
    state.0.lock().unwrap().yt_popped = true;

    // Fenster schließen = YouTube zurück in Orbit holen (statt die Webview zu zerstören)
    let handle = app.clone();
    popup.on_window_event(move |e| {
        if let WindowEvent::CloseRequested { api, .. } = e {
            api.prevent_close();
            let h = handle.clone();
            tauri::async_runtime::spawn(async move {
                let _ = dock_youtube(&h);
            });
        }
    });
    let _ = app.emit("yt-popout", true);
    Ok(())
}

fn dock_youtube(app: &AppHandle) -> Result<(), String> {
    let main = app.get_window("main").ok_or("Main window is missing")?;
    if let Some(wv) = app.get_webview(YT_LABEL) {
        let _ = wv.set_auto_resize(false);
        wv.reparent(&main).map_err(|e| e.to_string())?;
    }
    {
        let state = app.state::<AppState>();
        let mut s = state.0.lock().unwrap();
        s.yt_popped = false;
        apply_layout(app, &s);
    }
    if let Some(p) = app.get_window(YT_POPUP) {
        let _ = p.destroy();
    }
    let _ = app.emit("yt-popout", false);
    Ok(())
}

#[tauri::command]
async fn yt_dock(app: AppHandle) -> Result<(), String> {
    dock_youtube(&app)
}

// ---------------------------------------------------------------- Erweiterungen

#[tauri::command]
async fn ext_list(app: AppHandle) -> Vec<extensions::ExtInfo> {
    extensions::list(&app)
}

#[tauri::command]
async fn ext_install(app: AppHandle, input: String) -> Result<extensions::ExtInfo, String> {
    tauri::async_runtime::spawn_blocking(move || extensions::install(&app, &input))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn ext_remove(app: AppHandle, folder: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || extensions::remove(&app, &folder))
        .await
        .map_err(|e| e.to_string())?
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::default())
        .setup(|app| {
            let window = WindowBuilder::new(app, "main")
                .title("Orbit")
                .inner_size(1440.0, 900.0)
                .min_inner_size(960.0, 600.0)
                .decorations(false)
                .center()
                .build()?;
            // Hauptfenster zu = ganze App beenden (auch ein offenes YouTube-Pop-out)
            let handle = app.handle().clone();
            window.on_window_event(move |e| {
                if let WindowEvent::CloseRequested { .. } = e {
                    handle.exit(0);
                }
            });
            let size = window.inner_size()?.to_logical::<f64>(window.scale_factor()?);
            window.add_child(
                with_extensions(app.handle(), WebviewBuilder::new("shell", WebviewUrl::App("index.html".into())))
                    .auto_resize(),
                LogicalPosition::new(0.0, 0.0),
                size,
            )?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            tab_create,
            tab_close,
            tab_activate,
            tab_navigate,
            tab_action,
            set_layout,
            yt_action,
            yt_popout,
            yt_dock,
            ext_list,
            ext_install,
            ext_remove
        ])
        .run(tauri::generate_context!())
        .expect("Orbit failed to start");
}
