// The renderer methods, as `desktop_core::renderer_methods!` lists them, and
// the shell's own commands, as `native_commands!` lists them.
include!("../core/src/ui_methods.rs");
include!("src/native_commands.rs");

fn main() {
    let commands: Vec<&'static str> = renderer_methods!(renderer_names)
        .into_iter()
        .chain(native_commands!(native_names))
        .collect();
    let manifest = tauri_build::AppManifest::new().commands(commands.leak());
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("failed to build the Tauri application");
}
