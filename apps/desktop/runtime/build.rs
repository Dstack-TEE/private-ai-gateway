use std::path::PathBuf;

fn main() {
    if std::env::var_os("CARGO_FEATURE_WEB_UI").is_none() {
        return;
    }
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    println!("cargo:rerun-if-changed={}", root.join("web-dist").display());
    // Development builds may skip the renderer; enabling the web UI then reports it in state.
    // Release packaging builds the bundle first and asserts it exists.
    if !root.join("web-dist/index.html").is_file() {
        println!(
            "cargo:warning=Web UI assets are not built; run `npm run build:web` in apps/desktop to include them."
        );
    }
}
