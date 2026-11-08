//! Builds Orbit and copies the CEF/Chromium runtime (libcef.dll, resources,
//! locales, …) next to the executable in `target/bundle`.

use std::path::PathBuf;

fn main() {
    let release = std::env::args().any(|a| a == "--release");
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("bundle");
    std::fs::create_dir_all(&out).expect("cannot create bundle dir");
    match cef::build_util::win::build_bundle(&out, "orbit", release) {
        Ok(exe) => println!("Bundled: {}", exe.display()),
        Err(e) => {
            eprintln!("Bundling failed: {e}");
            std::process::exit(1);
        }
    }
}
