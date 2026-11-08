//! Global media bar: shows what is playing in any tab or the YouTube view
//! and lets the UI control it (play/pause, previous/next, seek).
//!
//! Renderer side: every web frame gets a small watcher script. It reports the
//! frame's main audible <video>/<audio> element plus the page's Media Session
//! metadata (title, artist, artwork – the same data Windows' media keys use).
//!
//! Browser side: keeps one entry per (page, frame) and picks the "primary"
//! source: the one that most recently started playing (playing beats paused).

use std::{cell::RefCell, collections::HashMap};

use cef::*;
use serde_json::{json, Value};

pub const MESSAGE: &str = "orbit-media";
const REPORT_FN: &str = "__orbitMediaReport";

/// Injected into every web frame. Reports at most once per second while
/// playing, immediately on play/pause/metadata changes.
const WATCHER_JS: &str = r#"
(() => {
  const send = window.__orbitMediaReport;
  try { delete window.__orbitMediaReport; } catch (e) {}
  if (typeof send !== 'function' || window.__orbitMediaWatcher) return;
  window.__orbitMediaWatcher = true;

  // Remember the page's own previous/next handlers (YouTube, Spotify, ...).
  const handlers = {};
  try {
    const ms = navigator.mediaSession;
    const original = ms.setActionHandler.bind(ms);
    ms.setActionHandler = (action, fn) => { handlers[action] = fn; return original(action, fn); };
  } catch (e) {}

  let current = null, last = 0, timer = null, lastJson = '';
  const audible = (m) => !m.paused && !m.ended && !m.muted && m.volume > 0;
  const area = (m) => { const r = m.getBoundingClientRect(); return r.width * r.height; };

  function pick() {
    const all = [...document.querySelectorAll('video, audio')].filter((m) => m.currentSrc || m.src);
    const playing = all.filter(audible).sort((a, b) => area(b) - area(a));
    if (playing.length) current = playing[0];
    else if (current && !current.isConnected) current = null;
    return current;
  }

  function report(force) {
    const now = Date.now();
    if (!force && now - last < 1000) {
      clearTimeout(timer);
      timer = setTimeout(() => report(true), 1000 - (now - last));
      return;
    }
    last = now;
    const el = pick();
    let data = null;
    if (el) {
      const md = navigator.mediaSession && navigator.mediaSession.metadata;
      const art = md && md.artwork && md.artwork.length ? md.artwork[md.artwork.length - 1].src : null;
      data = {
        playing: !el.paused && !el.ended,
        title: (md && md.title) || document.title || location.hostname,
        artist: (md && md.artist) || location.hostname.replace(/^www\./, ''),
        artwork: art,
        position: el.currentTime || 0,
        duration: isFinite(el.duration) ? el.duration : 0,
        video: el.tagName === 'VIDEO',
        hasNext: !!handlers.nexttrack,
        hasPrev: !!handlers.previoustrack,
      };
    }
    const json = JSON.stringify(data);
    if (json !== lastJson) { lastJson = json; send(json); }
  }

  for (const ev of ['play', 'playing', 'pause', 'ended', 'loadedmetadata', 'emptied', 'volumechange', 'seeked']) {
    document.addEventListener(ev, () => report(true), true);
  }
  document.addEventListener('timeupdate', () => report(false), true);

  window.__orbitMediaControl = (action, value) => {
    const el = current;
    if (!el) return;
    switch (action) {
      case 'toggle': el.paused ? el.play() : el.pause(); break;
      case 'seek': el.currentTime = value; break;
      case 'next':
        if (handlers.nexttrack) handlers.nexttrack({ action: 'nexttrack' });
        else if (isFinite(el.duration)) el.currentTime = el.duration;
        break;
      case 'prev':
        if (el.currentTime > 3 || !handlers.previoustrack) el.currentTime = 0;
        else handlers.previoustrack({ action: 'previoustrack' });
        break;
    }
  };
})();
"#;

// ------------------------------------------------------------------ renderer process

fn send_report(frame: &Frame, json: &str) {
    let Some(mut message) = process_message_create(Some(&CefString::from(MESSAGE))) else { return };
    if let Some(args) = message.argument_list() {
        args.set_string(0, Some(&CefString::from(json)));
    }
    frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
}

wrap_v8_handler! {
    struct ReportHandler {
        frame: Frame,
    }

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> i32 {
            let json = arguments
                .and_then(|a| a.first().cloned().flatten())
                .filter(|v| v.is_string() != 0)
                .map(|v| CefString::from(&v.string_value()).to_string());
            if let Some(json) = json {
                // Keep reports small – they only carry a few fields.
                if json.len() <= 4096 {
                    send_report(&self.frame, &json);
                }
            }
            1
        }
    }
}

/// Renderer: a web frame got a JavaScript context – install the watcher.
pub fn install(frame: &mut Frame, context: &mut V8Context) {
    let Some(global) = context.global() else { return };
    let mut handler = ReportHandler::new(frame.clone());
    let name = CefString::from(REPORT_FN);
    let mut func = v8_value_create_function(Some(&name), Some(&mut handler));
    global.set_value_bykey(Some(&name), func.as_mut(), Default::default());
    context.eval(Some(&CefString::from(WATCHER_JS)), None, 0, None, None);
}

/// Renderer: the frame's page is going away – its media is gone.
pub fn uninstall(frame: &mut Frame) {
    send_report(frame, "null");
}

// ------------------------------------------------------------------ browser process

struct Source {
    info: Value,
    playing: bool,
    /// increases every time a source starts playing -> "most recent" wins
    started: u64,
}

#[derive(Default)]
struct Media {
    sources: HashMap<(u32, String), Source>,
    seq: u64,
    primary: Option<(u32, String)>,
}

thread_local! {
    static MEDIA: RefCell<Media> = RefCell::new(Media::default());
}

/// Browser: a report from page `page` / frame `frame_id` arrived.
pub fn on_report(page: u32, frame_id: String, json: &str) {
    let info: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    MEDIA.with(|m| {
        let mut m = m.borrow_mut();
        let key = (page, frame_id);
        if info.is_null() {
            m.sources.remove(&key);
        } else {
            let playing = info["playing"].as_bool().unwrap_or(false);
            let was_playing = m.sources.get(&key).map(|s| s.playing).unwrap_or(false);
            let started = if playing && !was_playing {
                m.seq += 1;
                m.seq
            } else {
                m.sources.get(&key).map(|s| s.started).unwrap_or(0)
            };
            m.sources.insert(key, Source { info, playing, started });
        }
    });
    publish();
}

/// Browser: a tab was closed.
pub fn forget_page(page: u32) {
    MEDIA.with(|m| m.borrow_mut().sources.retain(|(p, _), _| *p != page));
    publish();
}

/// Picks the primary source and sends it to the UI.
fn publish() {
    let payload = MEDIA.with(|m| {
        let mut m = m.borrow_mut();
        let best = m
            .sources
            .iter()
            .max_by_key(|(_, s)| (s.playing, s.started))
            .map(|(k, s)| (k.clone(), s.info.clone()));
        m.primary = best.as_ref().map(|(k, _)| k.clone());
        best.map(|((page, _), mut info)| {
            info["page"] = json!(page);
            info
        })
    });
    crate::orbit::emit("media", payload.unwrap_or(Value::Null));
}

/// Browser: the UI pressed a control button.
pub fn control(action: &str, value: f64) -> Result<(), String> {
    if !matches!(action, "toggle" | "seek" | "next" | "prev") {
        return Err(format!("Unknown media action: {action}"));
    }
    let (page, frame_id) = MEDIA.with(|m| m.borrow().primary.clone()).ok_or("Nothing is playing")?;
    let browser = crate::orbit::page_browser(page).ok_or("Source is gone")?;
    let frame = browser
        .frame_by_identifier(Some(&CefString::from(frame_id.as_str())))
        .ok_or("Source frame is gone")?;
    let js = format!(
        "window.__orbitMediaControl&&window.__orbitMediaControl({},{})",
        json!(action),
        if value.is_finite() { value } else { 0.0 }
    );
    frame.execute_java_script(Some(&CefString::from(js.as_str())), None, 0);
    Ok(())
}
