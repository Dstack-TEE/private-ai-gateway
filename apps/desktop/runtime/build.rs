use std::path::PathBuf;

fn main() {
    if std::env::var_os("CARGO_FEATURE_WEB_UI").is_none() {
        return;
    }
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let index = root.join("web-dist/index.html");
    println!("cargo:rerun-if-changed={}", root.join("web-dist").display());
    if !index.is_file() {
        panic!(
            "Private AI Proxy web assets are missing. Run `npm run build:web` in apps/desktop before Cargo builds."
        );
    }
}
