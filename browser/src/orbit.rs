//! Orbit's browser-process logic: main window, shell UI, tabs, YouTube view,
//! keyboard shortcuts and the bridge (IPC) between the UI and Rust.
//!
//! Layout model (same as before): the shell (our HTML UI) fills the window.
//! Every web page – each tab and the YouTube view – is its own Chromium
//! BrowserView, placed as an overlay exactly over the placeholder rectangles
//! the shell reports via `set_layout`.
//!
//! All CEF objects live on the browser UI thread, so state is thread-local.

use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, Mutex},
};

use cef::wrapper::message_router::{
    BrowserSideCallback, BrowserSideHandler, BrowserSideRouter, MessageRouterBrowserSide,
    MessageRouterBrowserSideHandlerCallbacks, MessageRouterConfig,
};
use cef::*;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::scheme::SHELL_URL;

/// Page id of the YouTube view (UI tab ids start at 1).
const YT_ID: u32 = 0;
const YT_HOME: &str = "https://www.youtube.com/";
const BG_COLOR: u32 = 0xff0a0a0f;

// Event-flag bits from cef_event_flags_t
const FLAG_SHIFT: u32 = 2;
const FLAG_CTRL: u32 = 4;
const FLAG_ALT: u32 = 8;

// ------------------------------------------------------------------ state

struct Page {
    view: BrowserView,
    /// None while the view lives in the YouTube pop-out window
    overlay: Option<OverlayController>,
}

#[derive(Default)]
struct State {
    window: Option<Window>,
    shell: Option<BrowserView>,
    pages: HashMap<u32, Page>,
    active: Option<u32>,
    content: Option<Rect>,
    yt_rect: Option<Rect>,
    browser_visible: bool,
    yt_popup: Option<Window>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static ROUTER: RefCell<Option<Arc<BrowserSideRouter>>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

fn cef_str(s: &str) -> CefString {
    CefString::from(s)
}

// ------------------------------------------------------------------ helpers

/// Sends an event to the shell UI (`listen(name, ...)` in app.js).
pub fn emit(name: &str, payload: Value) {
    let shell = with_state(|s| s.shell.clone());
    let Some(frame) = shell.and_then(|v| v.browser()).and_then(|b| b.main_frame()) else { return };
    let js = format!("window.__orbitEmit&&window.__orbitEmit({},{})", json!(name), payload);
    frame.execute_java_script(Some(&cef_str(&js)), Some(&cef_str(SHELL_URL)), 0);
}

fn shell_browser_id() -> Option<i32> {
    with_state(|s| s.shell.clone()).and_then(|v| v.browser()).map(|b| b.identifier())
}

fn page_browser(id: u32) -> Option<Browser> {
    with_state(|s| s.pages.get(&id).map(|p| p.view.clone())).and_then(|v| v.browser())
}

fn create_page_view(id: u32, url: &str) -> Option<BrowserView> {
    let mut client = PageClient::new(id);
    let mut delegate = PageViewDelegate::new();
    let settings = BrowserSettings { background_color: BG_COLOR, ..Default::default() };
    browser_view_create(Some(&mut client), Some(&cef_str(url)), Some(&settings), None, None, Some(&mut delegate))
}

fn attach_overlay(view: &BrowserView) -> Option<OverlayController> {
    let window = with_state(|s| s.window.clone())?;
    let mut v = View::from(view);
    window.add_overlay_view(Some(&mut v), DockingMode::CUSTOM, 1)
}

/// Shows the active tab (and YouTube) at the reported rectangles, hides the rest.
fn apply_layout() {
    let ops: Vec<(OverlayController, Option<Rect>)> = with_state(|s| {
        s.pages
            .iter()
            .filter_map(|(id, p)| {
                let overlay = p.overlay.clone()?;
                let rect = if *id == YT_ID {
                    s.yt_rect.clone()
                } else if s.browser_visible && s.active == Some(*id) {
                    s.content.clone()
                } else {
                    None
                };
                Some((overlay, rect))
            })
            .collect()
    });
    for (overlay, rect) in ops {
        match rect {
            Some(r) => {
                overlay.set_bounds(Some(&r));
                overlay.set_visible(1);
            }
            None => overlay.set_visible(0),
        }
    }
}

#[derive(Deserialize, Clone, Copy)]
struct JsRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl From<JsRect> for Rect {
    fn from(r: JsRect) -> Self {
        Rect {
            x: r.x.round() as i32,
            y: r.y.round() as i32,
            width: r.w.max(1.0).round() as i32,
            height: r.h.max(1.0).round() as i32,
        }
    }
}

// ------------------------------------------------------------------ commands from the UI

fn arg_u32(args: &Value, key: &str) -> Result<u32, String> {
    args[key].as_u64().map(|v| v as u32).ok_or_else(|| format!("missing argument '{key}'"))
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key].as_str().ok_or_else(|| format!("missing argument '{key}'"))
}

fn handle_command(cmd: &str, args: &Value) -> Result<Value, String> {
    match cmd {
        "tab_create" => {
            let id = arg_u32(args, "id")?;
            if id == YT_ID || with_state(|s| s.pages.contains_key(&id)) {
                return Err(format!("Tab {id} already exists"));
            }
            let view = create_page_view(id, arg_str(args, "url")?).ok_or("Could not create browser")?;
            let overlay = attach_overlay(&view);
            with_state(|s| {
                s.pages.insert(id, Page { view, overlay });
                if args["activate"].as_bool().unwrap_or(true) {
                    s.active = Some(id);
                }
            });
            apply_layout();
        }
        "tab_close" => {
            let id = arg_u32(args, "id")?;
            let page = with_state(|s| {
                if s.active == Some(id) {
                    s.active = None;
                }
                s.pages.remove(&id)
            });
            if let Some(page) = page {
                if let Some(host) = page.view.browser().and_then(|b| b.host()) {
                    host.close_browser(1);
                }
                if let Some(overlay) = page.overlay {
                    overlay.destroy();
                }
            }
        }
        "tab_activate" => {
            let id = args["id"].as_u64().map(|v| v as u32);
            with_state(|s| s.active = id.filter(|i| s.pages.contains_key(i)));
            apply_layout();
        }
        "tab_navigate" => {
            let browser = page_browser(arg_u32(args, "id")?).ok_or("Tab does not exist")?;
            let frame = browser.main_frame().ok_or("Tab has no frame")?;
            frame.load_url(Some(&cef_str(arg_str(args, "url")?)));
        }
        "tab_action" => {
            let browser = page_browser(arg_u32(args, "id")?).ok_or("Tab does not exist")?;
            page_action(&browser, arg_str(args, "action")?)?;
        }
        "set_layout" => {
            let content: Option<JsRect> = serde_json::from_value(args["content"].clone()).ok();
            let yt: Option<JsRect> = serde_json::from_value(args["yt"].clone()).ok();
            let (need_yt, popped) = with_state(|s| {
                s.content = content.map(Rect::from);
                s.yt_rect = yt.map(Rect::from);
                s.browser_visible = args["browserVisible"].as_bool().unwrap_or(false);
                (yt.is_some() && !s.pages.contains_key(&YT_ID), s.yt_popup.is_some())
            });
            // The YouTube view is only created the first time it is opened (saves RAM).
            if need_yt && !popped {
                let view = create_page_view(YT_ID, YT_HOME).ok_or("Could not create YouTube view")?;
                let overlay = attach_overlay(&view);
                with_state(|s| s.pages.insert(YT_ID, Page { view, overlay }));
            }
            apply_layout();
        }
        "yt_action" => {
            let browser = page_browser(YT_ID).ok_or("YouTube is not open")?;
            match arg_str(args, "action")? {
                "home" => browser.main_frame().ok_or("no frame")?.load_url(Some(&cef_str(YT_HOME))),
                other => page_action(&browser, other)?,
            }
        }
        "yt_popout" => yt_popout()?,
        "yt_dock" => {
            if let Some(popup) = with_state(|s| s.yt_popup.clone()) {
                popup.close(); // can_close() of the pop-out docks YouTube back
            }
        }
        "win" => {
            let window = with_state(|s| s.window.clone()).ok_or("no window")?;
            match arg_str(args, "action")? {
                "minimize" => window.minimize(),
                "maximize" => {
                    if window.is_maximized() != 0 {
                        window.restore()
                    } else {
                        window.maximize()
                    }
                }
                "close" => window.close(),
                other => return Err(format!("Unknown window action: {other}")),
            }
        }
        other => return Err(format!("Unknown command: {other}")),
    }
    Ok(Value::Null)
}

fn page_action(browser: &Browser, action: &str) -> Result<(), String> {
    match action {
        "back" => browser.go_back(),
        "forward" => browser.go_forward(),
        "reload" => browser.reload(),
        "stop" => browser.stop_load(),
        other => return Err(format!("Unknown action: {other}")),
    }
    Ok(())
}

// ------------------------------------------------------------------ YouTube pop-out

/// Moves the running YouTube view into its own normal window – the video keeps playing.
fn yt_popout() -> Result<(), String> {
    if let Some(popup) = with_state(|s| s.yt_popup.clone()) {
        popup.show();
        popup.activate();
        return Ok(());
    }
    let (view, overlay) = with_state(|s| {
        let page = s.pages.get_mut(&YT_ID)?;
        Some((page.view.clone(), page.overlay.take()))
    })
    .ok_or("YouTube is not open")?;
    if let Some(overlay) = overlay {
        overlay.destroy();
    }
    let mut delegate = PopupWindowDelegate::new(RefCell::new(Some(view)));
    window_create_top_level(Some(&mut delegate));
    emit("yt-popout", json!(true));
    Ok(())
}

fn yt_dock(popup: &Window) {
    let view = with_state(|s| {
        s.yt_popup = None;
        s.pages.get(&YT_ID).map(|p| p.view.clone())
    });
    if let Some(view) = view {
        let mut v = View::from(&view);
        popup.remove_child_view(Some(&mut v));
        let overlay = attach_overlay(&view);
        with_state(|s| {
            if let Some(page) = s.pages.get_mut(&YT_ID) {
                page.overlay = overlay;
            }
        });
    }
    apply_layout();
    emit("yt-popout", json!(false));
}

// ------------------------------------------------------------------ IPC (cefQuery)

struct IpcHandler;

impl BrowserSideHandler for IpcHandler {
    fn on_query_str(
        &self,
        browser: Option<Browser>,
        frame: Option<Frame>,
        _query_id: i64,
        request: &str,
        _persistent: bool,
        callback: Arc<Mutex<dyn BrowserSideCallback>>,
    ) -> bool {
        // Only Orbit's own UI may control the browser – never a website.
        let from_shell = browser.map(|b| b.identifier()) == shell_browser_id() && shell_browser_id().is_some();
        let from_ui = frame
            .map(|f| CefString::from(&f.url()).to_string().starts_with("orbit://app/"))
            .unwrap_or(false);
        if !(from_shell && from_ui) {
            return false;
        }
        let result = serde_json::from_str::<Value>(request)
            .map_err(|e| e.to_string())
            .and_then(|v| handle_command(v["cmd"].as_str().unwrap_or(""), &v["args"]));
        let cb = callback.lock().unwrap();
        match result {
            Ok(v) => cb.success_str(&v.to_string()),
            Err(e) => cb.failure(1, &e),
        }
        true
    }
}

fn router() -> Option<Arc<BrowserSideRouter>> {
    ROUTER.with(|r| r.borrow().clone())
}

// ------------------------------------------------------------------ keyboard shortcuts

/// Maps a key press to a shell command. `in_shell`: the shell itself has focus,
/// so browser keys (F5, Alt+←/→) are forwarded to the active tab instead of
/// reloading/navigating the UI.
fn shortcut_for(event: &KeyEvent, in_shell: bool) -> Option<&'static str> {
    if event.type_ != KeyEventType::RAWKEYDOWN {
        return None;
    }
    let ctrl = event.modifiers & FLAG_CTRL != 0;
    let shift = event.modifiers & FLAG_SHIFT != 0;
    let alt = event.modifiers & FLAG_ALT != 0;
    let key = event.windows_key_code;
    Some(match (ctrl, shift, alt, key) {
        (true, true, false, 0x09) => "prev-tab",
        (true, true, false, 0x59) => "toggle-yt", // Y
        (true, false, false, 0x54) => "new-tab",  // T
        (true, false, false, 0x57) => "close-tab", // W
        (true, false, false, 0x4C) => "focus-address", // L
        (true, false, false, 0x09) => "next-tab",
        (true, false, false, 0x52) if in_shell => "reload", // R
        (false, false, false, 0x74) if in_shell => "reload", // F5
        (false, false, true, 0x25) if in_shell => "back",
        (false, false, true, 0x27) if in_shell => "forward",
        _ => return None,
    })
}

wrap_keyboard_handler! {
    struct Keys {
        in_shell: bool,
    }

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            _browser: Option<&mut Browser>,
            event: Option<&KeyEvent>,
            _os_event: Option<&mut sys::MSG>,
            _is_keyboard_shortcut: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            match event.and_then(|e| shortcut_for(e, self.in_shell)) {
                Some(cmd) => {
                    emit("shortcut", json!(cmd));
                    1
                }
                None => 0,
            }
        }
    }
}

// ------------------------------------------------------------------ web page client (tabs + YouTube)

wrap_client! {
    struct PageClient {
        id: u32,
    }

    impl Client {
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(PageDisplay::new(self.id))
        }
        fn load_handler(&self) -> Option<LoadHandler> {
            Some(PageLoad::new(self.id))
        }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(PageLifeSpan::new())
        }
        fn keyboard_handler(&self) -> Option<KeyboardHandler> {
            Some(Keys::new(false))
        }
    }
}

wrap_display_handler! {
    struct PageDisplay {
        id: u32,
    }

    impl DisplayHandler {
        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            if frame.map(|f| f.is_main() != 0).unwrap_or(false) {
                let url = url.map(|u| u.to_string()).unwrap_or_default();
                emit("tab-updated", json!({ "id": self.id, "url": url }));
            }
        }

        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            let title = title.map(|t| t.to_string()).unwrap_or_default();
            emit("tab-updated", json!({ "id": self.id, "title": title }));
        }
    }
}

wrap_load_handler! {
    struct PageLoad {
        id: u32,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            _can_go_back: ::std::os::raw::c_int,
            _can_go_forward: ::std::os::raw::c_int,
        ) {
            emit("tab-updated", json!({ "id": self.id, "loading": is_loading != 0 }));
        }
    }
}

wrap_life_span_handler! {
    struct PageLifeSpan;

    impl LifeSpanHandler {
        /// Links with target=_blank / window.open become a new Orbit tab.
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            if let Some(url) = target_url.map(|u| u.to_string()).filter(|u| !u.is_empty()) {
                emit("open-tab", json!(url));
            }
            1 // cancel the native popup
        }
    }
}

wrap_browser_view_delegate! {
    struct PageViewDelegate {}

    impl ViewDelegate {}

    impl BrowserViewDelegate {
        // Several web views share one window -> Alloy style (Chrome style allows only one per window).
        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

// ------------------------------------------------------------------ shell (our UI)

wrap_client! {
    struct ShellClient;

    impl Client {
        fn keyboard_handler(&self) -> Option<KeyboardHandler> {
            Some(Keys::new(true))
        }
        fn drag_handler(&self) -> Option<DragHandler> {
            Some(ShellDrag::new())
        }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(ShellLifeSpan::new())
        }
        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            router()
                .map(|r| r.on_process_message_received(browser.cloned(), frame.cloned(), source_process, message.cloned()))
                .unwrap_or(false) as _
        }
    }
}

wrap_drag_handler! {
    struct ShellDrag;

    impl DragHandler {
        /// CSS `-webkit-app-region: drag` areas of the UI move the frameless window.
        fn on_draggable_regions_changed(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            regions: Option<&[DraggableRegion]>,
        ) {
            if let Some(window) = with_state(|s| s.window.clone()) {
                window.set_draggable_regions(regions);
            }
        }
    }
}

wrap_life_span_handler! {
    struct ShellLifeSpan;

    impl LifeSpanHandler {
        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(r) = router() {
                r.on_before_close(browser.cloned());
            }
        }
    }
}

wrap_browser_view_delegate! {
    struct ShellViewDelegate {}

    impl ViewDelegate {}

    impl BrowserViewDelegate {
        // Several web views share one window -> Alloy style (Chrome style allows only one per window).
        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

// ------------------------------------------------------------------ windows

wrap_window_delegate! {
    struct MainWindowDelegate {}

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            Size { width: 1440, height: 900 }
        }
        fn minimum_size(&self, _view: Option<&mut View>) -> Size {
            Size { width: 960, height: 600 }
        }
    }

    impl PanelDelegate {}

    impl WindowDelegate {
        fn window_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }

        fn on_window_created(&self, window: Option<&mut Window>) {
            let Some(window) = window else { return };
            let shell = with_state(|s| {
                s.window = Some(window.clone());
                s.shell.clone()
            });
            if let Some(shell) = shell {
                let mut v = View::from(&shell);
                window.add_child_view(Some(&mut v));
            }
            window.set_title(Some(&cef_str("Orbit")));
            window.show();
        }

        fn is_frameless(&self, _window: Option<&mut Window>) -> ::std::os::raw::c_int {
            1
        }

        fn can_close(&self, _window: Option<&mut Window>) -> ::std::os::raw::c_int {
            // Close every page, the pop-out and finally the shell itself.
            let (pages, popup, shell) = with_state(|s| {
                let pages: Vec<BrowserView> = s.pages.values().map(|p| p.view.clone()).collect();
                (pages, s.yt_popup.take(), s.shell.clone())
            });
            if let Some(popup) = popup {
                popup.close();
            }
            for view in pages {
                if let Some(host) = view.browser().and_then(|b| b.host()) {
                    host.close_browser(1);
                }
            }
            shell
                .and_then(|v| v.browser())
                .and_then(|b| b.host())
                .map(|h| h.try_close_browser())
                .unwrap_or(1)
        }

        fn on_window_destroyed(&self, _window: Option<&mut Window>) {
            with_state(|s| {
                s.window = None;
                s.pages.clear();
            });
            quit_message_loop();
        }
    }
}

wrap_window_delegate! {
    struct PopupWindowDelegate {
        view: RefCell<Option<BrowserView>>,
    }

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            Size { width: 1100, height: 680 }
        }
        fn minimum_size(&self, _view: Option<&mut View>) -> Size {
            Size { width: 420, height: 260 }
        }
    }

    impl PanelDelegate {}

    impl WindowDelegate {
        fn window_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }

        fn on_window_created(&self, window: Option<&mut Window>) {
            let Some(window) = window else { return };
            if let Some(view) = self.view.borrow().as_ref() {
                let mut v = View::from(view);
                window.add_child_view(Some(&mut v));
            }
            with_state(|s| s.yt_popup = Some(window.clone()));
            window.set_title(Some(&cef_str("YouTube – Orbit")));
            window.show();
        }

        fn can_close(&self, window: Option<&mut Window>) -> ::std::os::raw::c_int {
            // Closing the pop-out moves YouTube back into Orbit instead of destroying it.
            let is_current = with_state(|s| s.yt_popup.is_some());
            if let (Some(window), true) = (window, is_current) {
                yt_dock(window);
            }
            *self.view.borrow_mut() = None;
            1
        }
    }
}

// ------------------------------------------------------------------ startup

/// Called once CEF is ready (browser process, UI thread).
pub fn start() {
    let router = BrowserSideRouter::new(MessageRouterConfig::default());
    router.add_handler(Arc::new(IpcHandler), false);
    ROUTER.with(|r| *r.borrow_mut() = Some(router));

    let mut client = ShellClient::new();
    let mut delegate = ShellViewDelegate::new();
    let settings = BrowserSettings { background_color: BG_COLOR, ..Default::default() };
    let shell = browser_view_create(
        Some(&mut client),
        Some(&cef_str(SHELL_URL)),
        Some(&settings),
        None,
        None,
        Some(&mut delegate),
    );
    with_state(|s| s.shell = shell);

    let mut window_delegate = MainWindowDelegate::new();
    window_create_top_level(Some(&mut window_delegate));
}
