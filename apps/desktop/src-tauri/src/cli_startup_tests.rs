use super::transient_macos_app_path;

#[test]
fn automatic_registration_rejects_transient_macos_locations() {
    assert!(transient_macos_app_path(std::path::Path::new(
        "/Volumes/Private AI Proxy/Private AI Proxy.app/Contents/MacOS/app"
    )));
    assert!(transient_macos_app_path(std::path::Path::new(
        "/private/var/folders/x/AppTranslocation/id/d/Private AI Proxy.app/Contents/MacOS/app"
    )));
    assert!(!transient_macos_app_path(std::path::Path::new(
        "/Applications/Private AI Proxy.app/Contents/MacOS/app"
    )));
}
