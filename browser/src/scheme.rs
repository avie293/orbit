//! `orbit://app/...` – serves Orbit's own UI (the `ui/` folder).
//!
//! A custom scheme (instead of file://) lets the UI use fetch() for its
//! translation files and keeps it in its own origin, separate from websites.

use std::{
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use cef::*;

pub const SCHEME: &str = "orbit";
pub const HOST: &str = "app";
pub const SHELL_URL: &str = "orbit://app/index.html";

/// Scheme flags: STANDARD | SECURE | CORS_ENABLED | FETCH_ENABLED
pub const SCHEME_OPTIONS: i32 = 1 | 8 | 16 | 64;

/// Folder with index.html, app.js, locales/ …
/// Next to the exe in a bundle, otherwise the repo's `ui/` folder (development).
pub fn ui_dir() -> PathBuf {
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("ui"))) {
        if dir.join("index.html").exists() {
            return dir;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("ui")
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html",
        "css" => "text/css",
        "js" => "text/javascript",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Maps a request URL to a file inside the UI folder (no `..` escapes).
fn resolve(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("orbit://app/")?;
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let rest = if rest.is_empty() { "index.html" } else { rest };
    let rel = Path::new(rest);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(ui_dir().join(rel))
}

wrap_resource_handler! {
    struct FileHandler {
        status: i32,
        mime: String,
        data: Vec<u8>,
        offset: Arc<Mutex<usize>>,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut i32>,
            _callback: Option<&mut Callback>,
        ) -> i32 {
            if let Some(h) = handle_request {
                *h = 1;
            }
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            if let Some(response) = response {
                response.set_status(self.status);
                response.set_mime_type(Some(&CefString::from(self.mime.as_str())));
            }
            if let Some(len) = response_length {
                *len = self.data.len() as i64;
            }
        }

        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: i32,
            bytes_read: Option<&mut i32>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> i32 {
            let Some(bytes_read) = bytes_read else { return 0 };
            let mut offset = self.offset.lock().unwrap();
            let remaining = self.data.len().saturating_sub(*offset);
            let n = remaining.min(bytes_to_read.max(0) as usize);
            if n == 0 {
                *bytes_read = 0;
                return 0;
            }
            unsafe { std::ptr::copy_nonoverlapping(self.data.as_ptr().add(*offset), data_out, n) };
            *offset += n;
            *bytes_read = n as i32;
            1
        }
    }
}

wrap_scheme_handler_factory! {
    pub struct UiSchemeFactory;

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let url = CefString::from(&request?.url()).to_string();
            let (status, mime, data) = match resolve(&url).and_then(|p| std::fs::read(&p).ok().map(|d| (p, d))) {
                Some((path, data)) => (200, mime_for(&path).to_string(), data),
                None => (404, "text/plain".to_string(), b"Not found".to_vec()),
            };
            Some(FileHandler::new(status, mime, data, Arc::new(Mutex::new(0))))
        }
    }
}
