//! Chrome-Erweiterungen für Orbit.
//!
//! Ablauf beim Installieren:
//! 1. Erweiterungs-ID aus dem Chrome-Web-Store-Link lesen
//! 2. CRX-Paket bei Google herunterladen (gleicher Weg wie Chrome selbst)
//! 3. CRX-Header abschneiden -> ZIP -> in `<AppData>/extensions/<store-id>/` entpacken
//! 4. Über die WebView2-API ins Browser-Profil laden (gilt dann für alle Tabs)
//!
//! Beim Start lädt Tauri/wry automatisch alle Ordner in `extensions/` (siehe
//! `extensions_path` in main.rs).

use std::{
    collections::HashMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use base64::Engine;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};
use webview2_com::{
    BrowserExtensionRemoveCompletedHandler,
    Microsoft::Web::WebView2::Win32::{ICoreWebView2BrowserExtension, ICoreWebView2Profile7, ICoreWebView2_13},
    ProfileAddBrowserExtensionCompletedHandler, ProfileGetBrowserExtensionsCompletedHandler,
};
use windows_core::{Interface, HSTRING, PWSTR};

/// Speicherort für entpackte Erweiterungen.
pub fn ext_dir(app: &AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("orbit"))
        .join("extensions");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Zuordnung Ordner -> von WebView2 vergebene Erweiterungs-ID.
fn meta_path(app: &AppHandle) -> PathBuf {
    ext_dir(app).with_file_name("extensions.json")
}

fn load_meta(app: &AppHandle) -> HashMap<String, String> {
    fs::read_to_string(meta_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_meta(app: &AppHandle, meta: &HashMap<String, String>) {
    if let Ok(s) = serde_json::to_string_pretty(meta) {
        let _ = fs::write(meta_path(app), s);
    }
}

#[derive(Serialize)]
pub struct ExtInfo {
    folder: String,
    /// Von WebView2 vergebene ID (für chrome-extension://<id>/...)
    id: Option<String>,
    name: String,
    description: String,
    version: String,
    icon: Option<String>,
    popup: Option<String>,
    options: Option<String>,
}

// ------------------------------------------------------------ Manifest lesen

fn localized(dir: &Path, manifest: &Value, raw: &str) -> String {
    let Some(key) = raw.strip_prefix("__MSG_").and_then(|s| s.strip_suffix("__")) else {
        return raw.to_string();
    };
    let default = manifest["default_locale"].as_str().unwrap_or("en");
    for locale in ["de", default, "en", "en_US"] {
        let Ok(text) = fs::read_to_string(dir.join("_locales").join(locale).join("messages.json")) else {
            continue;
        };
        let Ok(msgs) = serde_json::from_str::<serde_json::Map<String, Value>>(&text) else { continue };
        if let Some((_, v)) = msgs.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            if let Some(m) = v["message"].as_str() {
                return m.to_string();
            }
        }
    }
    raw.to_string()
}

fn icon_data_url(dir: &Path, manifest: &Value) -> Option<String> {
    let icons = manifest["icons"].as_object()?;
    let (_, rel) = icons
        .iter()
        .filter_map(|(size, p)| Some((size.parse::<u32>().ok()?, p.as_str()?)))
        .max_by_key(|(size, _)| *size)?;
    let bytes = fs::read(dir.join(rel.trim_start_matches('/'))).ok()?;
    let mime = if rel.ends_with(".svg") { "image/svg+xml" } else { "image/png" };
    Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

fn read_info(dir: &Path, id: Option<String>) -> Option<ExtInfo> {
    let manifest: Value = serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).ok()?).ok()?;
    let s = |v: &Value| v.as_str().map(|x| x.trim_start_matches('/').to_string());
    Some(ExtInfo {
        folder: dir.file_name()?.to_string_lossy().into_owned(),
        id,
        name: localized(dir, &manifest, manifest["name"].as_str().unwrap_or("Untitled")),
        description: localized(dir, &manifest, manifest["description"].as_str().unwrap_or("")),
        version: manifest["version"].as_str().unwrap_or("").to_string(),
        icon: icon_data_url(dir, &manifest),
        popup: s(&manifest["action"]["default_popup"]).or_else(|| s(&manifest["browser_action"]["default_popup"])),
        options: s(&manifest["options_ui"]["page"]).or_else(|| s(&manifest["options_page"])),
    })
}

pub fn list(app: &AppHandle) -> Vec<ExtInfo> {
    let meta = load_meta(app);
    let Ok(entries) = fs::read_dir(ext_dir(app)) else { return vec![] };
    let mut out: Vec<ExtInfo> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let folder = e.file_name().to_string_lossy().into_owned();
            read_info(&e.path(), meta.get(&folder).cloned())
        })
        .collect();
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

// ------------------------------------------------------------ Installieren

/// Sucht die 32-stellige Erweiterungs-ID (nur Buchstaben a–p) in einem Link oder Text.
fn parse_store_id(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut run = 0;
    for (i, b) in bytes.iter().enumerate() {
        if (b'a'..=b'p').contains(b) {
            run += 1;
            let next_ok = bytes.get(i + 1).map_or(true, |n| !(b'a'..=b'z').contains(n));
            if run == 32 && next_ok && (i < 32 || !(b'a'..=b'z').contains(&bytes[i - 32])) {
                return Some(input[i + 1 - 32..=i].to_string());
            }
        } else {
            run = 0;
        }
    }
    None
}

fn download_crx(id: &str) -> Result<Vec<u8>, String> {
    let url = format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&prodversion=140.0.0.0\
         &acceptformat=crx2,crx3&x=id%3D{id}%26installsource%3Dondemand%26uc"
    );
    let mut res = ureq::get(&url).call().map_err(|e| format!("Download failed: {e}"))?;
    let bytes = res
        .body_mut()
        .with_config()
        .limit(200 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("Download failed: {e}"))?;
    if bytes.is_empty() {
        return Err("Extension not found – is the link correct?".into());
    }
    Ok(bytes)
}

/// CRX = kleiner Header + normales ZIP. Wir geben nur den ZIP-Teil zurück.
fn crx_to_zip(data: &[u8]) -> Result<&[u8], String> {
    let u32_at = |o: usize| -> Result<usize, String> {
        data.get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or_else(|| "Broken CRX package".to_string())
    };
    if data.starts_with(b"PK") {
        return Ok(data);
    }
    if !data.starts_with(b"Cr24") {
        return Err("Did not receive a valid extension package".into());
    }
    let start = match u32_at(4)? {
        3 => 12 + u32_at(8)?,
        2 => 16 + u32_at(8)? + u32_at(12)?,
        v => return Err(format!("Unknown CRX version {v}")),
    };
    data.get(start..).ok_or_else(|| "Broken CRX package".into())
}

pub fn install(app: &AppHandle, input: &str) -> Result<ExtInfo, String> {
    let store_id = parse_store_id(input)
        .ok_or("No extension ID found. Copy the extension's link from the Chrome Web Store.")?;
    let crx = download_crx(&store_id)?;
    let zip = crx_to_zip(&crx)?;

    let target = ext_dir(app).join(&store_id);
    // ggf. alte Version vorher sauber entfernen
    if target.exists() {
        let _ = remove(app, &store_id);
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(zip)).map_err(|e| format!("Unpacking failed: {e}"))?;
    archive.extract(&target).map_err(|e| format!("Unpacking failed: {e}"))?;
    // Ordner mit "_" am Anfang (außer _locales) lehnt WebView2 bei entpackten Erweiterungen ab
    if let Ok(entries) = fs::read_dir(&target) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('_') && name != "_locales" {
                let _ = fs::remove_dir_all(e.path()).or_else(|_| fs::remove_file(e.path()));
            }
        }
    }

    let id = match profile_add(app, target.clone()) {
        Ok(id) => id,
        Err(e) => {
            let _ = fs::remove_dir_all(&target);
            return Err(format!("The browser engine rejected the extension: {e}"));
        }
    };
    let mut meta = load_meta(app);
    meta.insert(store_id.clone(), id.clone());
    save_meta(app, &meta);

    read_info(&target, Some(id)).ok_or_else(|| "manifest.json is missing".into())
}

pub fn remove(app: &AppHandle, folder: &str) -> Result<(), String> {
    if folder.contains(['/', '\\', '.']) {
        return Err("Invalid folder name".into());
    }
    let mut meta = load_meta(app);
    if let Some(id) = meta.remove(folder) {
        // Fehler hier nicht fatal: Ordner wird trotzdem gelöscht
        let _ = profile_remove(app, id);
    }
    save_meta(app, &meta);
    let dir = ext_dir(app).join(folder);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("Could not delete folder: {e}"))?;
    }
    Ok(())
}

// ------------------------------------------------------------ WebView2-Profil (COM)

fn ext_id(ext: &ICoreWebView2BrowserExtension) -> String {
    let mut p = PWSTR::null();
    unsafe {
        let _ = ext.Id(&mut p);
    }
    webview2_com::take_pwstr(p)
}

fn profile(pw: &tauri::webview::PlatformWebview) -> windows_core::Result<ICoreWebView2Profile7> {
    unsafe { pw.controller().CoreWebView2()?.cast::<ICoreWebView2_13>()?.Profile()?.cast::<ICoreWebView2Profile7>() }
}

/// Führt `f` auf dem UI-Thread mit dem Profil der Shell-Webview aus und wartet
/// (auf dem aktuellen Hintergrund-Thread) auf das Ergebnis aus dem Callback.
fn with_profile<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce(ICoreWebView2Profile7, mpsc::Sender<Result<T, String>>) -> windows_core::Result<()> + Send + 'static,
) -> Result<T, String> {
    let shell = app.get_webview("shell").ok_or("Shell webview is missing")?;
    let (tx, rx) = mpsc::channel();
    shell
        .with_webview(move |pw| {
            let res = profile(&pw).and_then(|p| f(p, tx.clone()));
            if let Err(e) = res {
                let _ = tx.send(Err(e.to_string()));
            }
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(30))
        .map_err(|_| "The browser engine timed out".to_string())?
}

fn profile_add(app: &AppHandle, path: PathBuf) -> Result<String, String> {
    with_profile(app, move |profile, tx| unsafe {
        let handler = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |hr, ext| {
            let res = match (hr, ext) {
                (Ok(()), Some(ext)) => Ok(ext_id(&ext)),
                (Err(e), _) => Err(e.to_string()),
                _ => Err("Unknown error".to_string()),
            };
            let _ = tx.send(res);
            Ok(())
        }));
        profile.AddBrowserExtension(&HSTRING::from(path.as_path()), &handler)
    })
}

fn profile_remove(app: &AppHandle, id: String) -> Result<(), String> {
    with_profile(app, move |profile, tx| unsafe {
        let handler = ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |hr, list| {
            let inner = || -> windows_core::Result<bool> {
                hr?;
                let Some(list) = list else { return Ok(false) };
                let mut count = 0u32;
                list.Count(&mut count)?;
                for i in 0..count {
                    let ext = list.GetValueAtIndex(i)?;
                    if ext_id(&ext) == id {
                        let tx2 = tx.clone();
                        ext.Remove(&BrowserExtensionRemoveCompletedHandler::create(Box::new(move |hr| {
                            let _ = tx2.send(hr.map_err(|e| e.to_string()));
                            Ok(())
                        })))?;
                        return Ok(true);
                    }
                }
                Ok(false)
            };
            match inner() {
                Ok(true) => {} // Ergebnis kommt aus dem Remove-Callback
                Ok(false) => {
                    let _ = tx.send(Ok(()));
                }
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                }
            }
            Ok(())
        }));
        profile.GetBrowserExtensions(&handler)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_id_in_store_link() {
        let id = "cjpalhdlnbpafiamejdnhcphjbkeiagm";
        assert_eq!(
            parse_store_id(&format!("https://chromewebstore.google.com/detail/ublock-origin/{id}?hl=de")).as_deref(),
            Some(id)
        );
        assert_eq!(parse_store_id(id).as_deref(), Some(id));
        assert_eq!(parse_store_id("https://example.com/nothing-here"), None);
    }
}
