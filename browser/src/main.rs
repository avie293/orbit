//! Orbit – a standalone browser built on Chromium (CEF).
//!
//! The same executable runs as the main browser process and as Chromium's
//! renderer / GPU / utility sub-processes.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod files;
mod media;
mod orbit;
mod scheme;

use std::sync::Arc;

use cef::wrapper::message_router::{
    MessageRouterConfig, MessageRouterRendererSide, MessageRouterRendererSideHandlerCallbacks,
    RendererSideRouter,
};
use cef::*;

// ------------------------------------------------------------------ browser process

wrap_browser_process_handler! {
    struct OrbitBrowserProcessHandler;

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            let mut factory = scheme::UiSchemeFactory::new();
            register_scheme_handler_factory(
                Some(&CefString::from(scheme::SCHEME)),
                Some(&CefString::from(scheme::HOST)),
                Some(&mut factory),
            );
            orbit::start();
        }
    }
}

// ------------------------------------------------------------------ renderer process

thread_local! {
    static RENDER_ROUTER: Arc<RendererSideRouter> = RendererSideRouter::new(MessageRouterConfig::default());
}

/// Only Orbit's own UI gets `window.cefQuery` – websites never see it.
fn is_orbit_ui(frame: &Option<&mut Frame>) -> bool {
    frame
        .as_ref()
        .map(|f| CefString::from(&f.url()).to_string().starts_with("orbit://app/"))
        .unwrap_or(false)
}

wrap_render_process_handler! {
    struct OrbitRenderProcessHandler;

    impl RenderProcessHandler {
        fn on_context_created(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            if is_orbit_ui(&frame) {
                RENDER_ROUTER.with(|r| r.on_context_created(browser.cloned(), frame.cloned(), context.cloned()));
            } else if let (Some(frame), Some(context)) = (frame, context) {
                // websites: media watcher for Orbit's global media bar
                media::install(frame, context);
            }
        }

        fn on_context_released(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let mut frame = frame;
            if !is_orbit_ui(&frame) {
                if let Some(frame) = frame.as_deref_mut() {
                    media::uninstall(frame);
                }
            }
            RENDER_ROUTER.with(|r| r.on_context_released(browser.cloned(), frame.cloned(), context.cloned()));
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            RENDER_ROUTER.with(|r| {
                r.on_process_message_received(browser.cloned(), frame.cloned(), Some(source_process), message.cloned())
            }) as _
        }
    }
}

// ------------------------------------------------------------------ app (all processes)

wrap_app! {
    struct OrbitApp;

    impl App {
        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            if let Some(registrar) = registrar {
                registrar.add_custom_scheme(Some(&CefString::from(scheme::SCHEME)), scheme::SCHEME_OPTIONS);
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(OrbitBrowserProcessHandler::new())
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(OrbitRenderProcessHandler::new())
        }
    }
}

fn main() {
    // Initialize the CEF API version.
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let args = args::Args::new();
    let mut app = OrbitApp::new();

    // Sub-processes (renderer, GPU, …) end here.
    let code = execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    if code >= 0 {
        std::process::exit(code);
    }

    // Persistent profile: logins, cookies, history and extensions survive restarts.
    let root = std::env::var("LOCALAPPDATA")
        .map(|p| format!("{p}\\Orbit\\User Data"))
        .unwrap_or_else(|_| "OrbitUserData".into());
    let profile = format!("{root}\\Default");
    let settings = Settings {
        no_sandbox: 1,
        root_cache_path: CefString::from(root.as_str()),
        cache_path: CefString::from(profile.as_str()),
        persist_session_cookies: 1,
        background_color: 0xff0a0a0f,
        ..Default::default()
    };
    assert_eq!(
        initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()),
        1,
        "CEF failed to initialize"
    );
    run_message_loop();
    shutdown();
}
