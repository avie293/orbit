//! File explorer backend. Everything here runs on worker threads (never on
//! Chromium's UI thread), so big folders or searches can't freeze Orbit.

use std::{
    fs,
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, UNIX_EPOCH},
};

use serde_json::{json, Value};

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;

/// Current search generation – starting a new search cancels the old one.
static SEARCH_GEN: AtomicU64 = AtomicU64::new(0);

fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key].as_str().ok_or_else(|| format!("missing argument '{key}'"))
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Dispatches an `fs_*` command (called on a worker thread).
pub fn handle(cmd: &str, args: &Value) -> Result<Value, String> {
    match cmd {
        "fs_places" => Ok(places()),
        "fs_list" => list(Path::new(arg(args, "path")?), args["hidden"].as_bool().unwrap_or(false)),
        "fs_open" => open(arg(args, "path")?),
        "fs_reveal" => {
            std::process::Command::new("explorer.exe")
                .arg(format!("/select,{}", arg(args, "path")?))
                .spawn()
                .map_err(err)?;
            Ok(Value::Null)
        }
        "fs_mkdir" => {
            let dir = unique_path(&Path::new(arg(args, "path")?).join(arg(args, "name")?));
            fs::create_dir(&dir).map_err(err)?;
            Ok(json!(dir.to_string_lossy()))
        }
        "fs_rename" => {
            let from = PathBuf::from(arg(args, "path")?);
            let name = arg(args, "name")?.trim();
            if name.is_empty() || name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']) {
                return Err("invalid name".into());
            }
            let to = from.with_file_name(name);
            if to.exists() && to != from {
                return Err("exists".into());
            }
            fs::rename(&from, &to).map_err(err)?;
            Ok(json!(to.to_string_lossy()))
        }
        "fs_delete" => {
            let paths = string_list(&args["paths"]);
            trash::delete_all(&paths).map_err(err)?; // -> Recycle Bin, can be restored
            Ok(Value::Null)
        }
        "fs_paste" => {
            let dest = PathBuf::from(arg(args, "dest")?);
            let moving = arg(args, "mode")? == "move";
            for src in string_list(&args["paths"]).iter().map(PathBuf::from) {
                let name = src.file_name().ok_or("invalid path")?;
                if moving && src.parent() == Some(dest.as_path()) {
                    continue; // cut + paste into the same folder: nothing to do
                }
                if dest.starts_with(&src) {
                    return Err("cannot copy a folder into itself".into());
                }
                let target = unique_path(&dest.join(name));
                if moving {
                    if fs::rename(&src, &target).is_err() {
                        // different drive: copy, then remove the original
                        copy_recursive(&src, &target).map_err(err)?;
                        remove_recursive(&src).map_err(err)?;
                    }
                } else {
                    copy_recursive(&src, &target).map_err(err)?;
                }
            }
            Ok(Value::Null)
        }
        "fs_size" => {
            let (size, files, dirs) = dir_size(Path::new(arg(args, "path")?));
            Ok(json!({ "size": size, "files": files, "dirs": dirs }))
        }
        "fs_preview" => preview(Path::new(arg(args, "path")?)),
        "fs_search" => search(arg(args, "path")?, arg(args, "query")?, args["id"].as_u64().unwrap_or(0)),
        "fs_search_cancel" => {
            SEARCH_GEN.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Null)
        }
        other => Err(format!("Unknown command: {other}")),
    }
}

fn string_list(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

// ------------------------------------------------------------------ places (sidebar)

/// Real location of a Windows known folder (respects OneDrive / custom redirection).
fn known_folder(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    use windows_sys::Win32::{System::Com::CoTaskMemFree, UI::Shell::SHGetKnownFolderPath};
    let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
    let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut raw) };
    let path = if hr == 0 && !raw.is_null() {
        let len = (0..).take_while(|&i| unsafe { *raw.add(i) } != 0).count();
        Some(PathBuf::from(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(raw, len) })))
    } else {
        None
    };
    unsafe { CoTaskMemFree(raw as _) };
    path
}

fn places() -> Value {
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile,
        FOLDERID_Videos,
    };
    let home = known_folder(&FOLDERID_Profile)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into()));
    let quick: Vec<Value> = [
        ("home", FOLDERID_Profile),
        ("desktop", FOLDERID_Desktop),
        ("downloads", FOLDERID_Downloads),
        ("documents", FOLDERID_Documents),
        ("pictures", FOLDERID_Pictures),
        ("music", FOLDERID_Music),
        ("videos", FOLDERID_Videos),
    ]
    .iter()
    .filter_map(|(id, folder)| known_folder(folder).filter(|p| p.is_dir()).map(|p| (id, p)))
    .map(|(id, p)| json!({ "id": id, "path": p.to_string_lossy() }))
    .collect();

    let drives: Vec<Value> = (b'A'..=b'Z')
        .map(|c| format!("{}:\\", c as char))
        .filter(|d| Path::new(d).exists())
        .map(|d| {
            let (total, free) = disk_space(&d);
            json!({ "path": d, "label": volume_label(&d), "total": total, "free": free })
        })
        .collect();

    json!({ "home": home, "quick": quick, "drives": drives })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn disk_space(root: &str) -> (u64, u64) {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let (mut free, mut total, mut _total_free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(wide(root).as_ptr(), &mut free, &mut total, &mut _total_free) };
    if ok != 0 { (total, free) } else { (0, 0) }
}

fn volume_label(root: &str) -> String {
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
    let mut name = [0u16; 261];
    let ok = unsafe {
        GetVolumeInformationW(
            wide(root).as_ptr(),
            name.as_mut_ptr(),
            name.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    if ok == 0 {
        return String::new();
    }
    let len = name.iter().position(|&c| c == 0).unwrap_or(0);
    String::from_utf16_lossy(&name[..len])
}

// ------------------------------------------------------------------ listing

fn entry_json(path: &Path, meta: &fs::Metadata) -> Value {
    let attrs = meta.file_attributes();
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    json!({
        "name": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned()),
        "path": path.to_string_lossy(),
        "dir": meta.is_dir(),
        "size": if meta.is_dir() { Value::Null } else { json!(meta.len()) },
        "modified": modified,
        "ext": path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default(),
        "hidden": attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0,
    })
}

fn list(path: &Path, show_hidden: bool) -> Result<Value, String> {
    let rd = fs::read_dir(path).map_err(err)?;
    let mut entries = Vec::new();
    for e in rd.flatten() {
        // symlink-safe metadata; skip entries we can't read
        let Ok(meta) = e.metadata() else { continue };
        let item = entry_json(&e.path(), &meta);
        if !show_hidden && item["hidden"].as_bool().unwrap_or(false) {
            continue;
        }
        entries.push(item);
    }
    Ok(json!({
        "path": path.to_string_lossy(),
        "parent": path.parent().map(|p| p.to_string_lossy().into_owned()),
        "entries": entries,
    }))
}

fn open(path: &str) -> Result<Value, String> {
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    let verb = wide("open");
    let file = wide(path);
    let result = unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL)
    };
    // ShellExecute returns a value > 32 on success
    if result as usize > 32 { Ok(Value::Null) } else { Err(format!("could not open ({})", result as usize)) }
}

// ------------------------------------------------------------------ copy / move helpers

/// "file.txt" -> "file (2).txt" if the name is taken.
fn unique_path(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|i| p.with_file_name(format!("{stem} ({i}){ext}")))
        .find(|c| !c.exists())
        .unwrap()
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_recursive(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

fn remove_recursive(p: &Path) -> std::io::Result<()> {
    if p.is_dir() { fs::remove_dir_all(p) } else { fs::remove_file(p) }
}

/// Total size of a folder (iterative, doesn't follow links).
fn dir_size(root: &Path) -> (u64, u64, u64) {
    let (mut size, mut files, mut dirs) = (0u64, 0u64, 0u64);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                dirs += 1;
                stack.push(e.path());
            } else if let Ok(m) = e.metadata() {
                files += 1;
                size += m.len();
            }
        }
    }
    (size, files, dirs)
}

// ------------------------------------------------------------------ preview

const TEXT_EXT: &[&str] = &[
    "txt", "md", "json", "js", "ts", "jsx", "tsx", "css", "html", "htm", "xml", "yml", "yaml", "toml", "ini", "cfg",
    "log", "csv", "rs", "py", "java", "c", "h", "cpp", "hpp", "cs", "go", "php", "rb", "sh", "bat", "ps1", "sql",
    "lua", "kt", "swift", "vue", "svelte", "gitignore", "env",
];
const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "ico", "avif"];

fn preview(path: &Path) -> Result<Value, String> {
    let meta = fs::metadata(path).map_err(err)?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let base = entry_json(path, &meta);
    if meta.is_dir() {
        return Ok(json!({ "kind": "dir", "entry": base }));
    }
    if IMAGE_EXT.contains(&ext.as_str()) {
        return Ok(json!({ "kind": "image", "entry": base }));
    }
    if TEXT_EXT.contains(&ext.as_str()) || meta.len() < 256 * 1024 {
        let mut buf = vec![0u8; 64 * 1024.min(meta.len() as usize)];
        use std::io::Read;
        let n = fs::File::open(path).and_then(|mut f| f.read(&mut buf)).map_err(err)?;
        buf.truncate(n);
        // binary files contain NUL bytes – don't show them as text
        if !buf.contains(&0) {
            if let Ok(text) = String::from_utf8(buf) {
                return Ok(json!({ "kind": "text", "entry": base, "text": text, "truncated": meta.len() > n as u64 }));
            }
        }
    }
    Ok(json!({ "kind": "other", "entry": base }))
}

// ------------------------------------------------------------------ search

/// Recursive name search below `root`. Results are streamed to the UI as
/// `fs-search` events in small batches; a newer search cancels this one.
fn search(root: &str, query: &str, id: u64) -> Result<Value, String> {
    let generation = SEARCH_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let needle = query.to_lowercase();
    if needle.trim().is_empty() {
        return Ok(Value::Null);
    }
    let mut stack = vec![PathBuf::from(root)];
    let mut batch = Vec::new();
    let (mut found, mut last_send) = (0usize, Instant::now());
    const MAX_RESULTS: usize = 5000;

    let send = |batch: &mut Vec<Value>, done: bool| {
        let results = std::mem::take(batch);
        crate::orbit::emit_from_any_thread("fs-search", json!({ "id": id, "results": results, "done": done }));
    };

    while let Some(dir) = stack.pop() {
        if SEARCH_GEN.load(Ordering::SeqCst) != generation {
            return Ok(Value::Null); // cancelled
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_lowercase();
            if ft.is_dir() {
                stack.push(e.path());
            }
            if name.contains(&needle) {
                if let Ok(meta) = e.metadata() {
                    batch.push(entry_json(&e.path(), &meta));
                    found += 1;
                }
            }
        }
        if !batch.is_empty() && (batch.len() >= 200 || last_send.elapsed().as_millis() > 150) {
            send(&mut batch, false);
            last_send = Instant::now();
        }
        if found >= MAX_RESULTS {
            break;
        }
    }
    send(&mut batch, true);
    Ok(json!({ "found": found }))
}
