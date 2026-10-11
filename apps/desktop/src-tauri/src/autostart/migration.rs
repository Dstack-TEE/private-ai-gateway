//! Direct macOS upgrade bridge, compiled out of MAS and other production targets.
//! Remove this module and legacy argument handling when upgrades from the
//! tauri-plugin-autostart releases are no longer supported. No migration marker
//! is needed: successful removal makes every subsequent launch a no-op.

pub(super) trait Backend {
    fn legacy_enabled(&self) -> Result<bool, String>;
    fn set_native_enabled(&self, enabled: bool) -> Result<(), String>;
    fn remove_legacy(&self) -> Result<(), String>;
}

pub(super) fn migrate(backend: &impl Backend) -> Result<(), String> {
    if backend.legacy_enabled()? {
        backend.set_native_enabled(true)?;
        backend.remove_legacy()?;
    }
    Ok(())
}

pub(super) fn disable(backend: &impl Backend) -> Result<(), String> {
    // Attempt both even when native unregister fails; never silently leave the
    // legacy entry enabled after the user's explicit Disable action.
    let native = backend.set_native_enabled(false);
    let legacy = backend.remove_legacy();
    native.and(legacy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct LoginItems {
        legacy: Cell<bool>,
        native: Cell<bool>,
        fail_native: bool,
        calls: RefCell<Vec<&'static str>>,
    }

    impl LoginItems {
        fn new(fail_native: bool) -> Self {
            Self {
                legacy: Cell::new(true),
                native: Cell::new(false),
                fail_native,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl Backend for LoginItems {
        fn legacy_enabled(&self) -> Result<bool, String> {
            Ok(self.legacy.get())
        }

        fn set_native_enabled(&self, enabled: bool) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(if enabled { "register" } else { "unregister" });
            if self.fail_native {
                return Err("Native registration unavailable".into());
            }
            self.native.set(enabled);
            Ok(())
        }

        fn remove_legacy(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("remove legacy");
            self.legacy.set(false);
            Ok(())
        }
    }

    #[test]
    fn migration_preserves_intent_and_is_idempotent() {
        let backend = LoginItems::new(false);
        migrate(&backend).unwrap();
        migrate(&backend).unwrap();
        assert!(backend.native.get());
        assert!(!backend.legacy.get());
        assert_eq!(*backend.calls.borrow(), ["register", "remove legacy"]);
    }

    #[test]
    fn failed_registration_retains_legacy() {
        let backend = LoginItems::new(true);
        assert!(migrate(&backend).is_err());
        assert!(backend.legacy.get());
        assert_eq!(*backend.calls.borrow(), ["register"]);
    }

    #[test]
    fn absent_legacy_registration_does_not_enable_login() {
        let backend = LoginItems::new(false);
        backend.legacy.set(false);
        migrate(&backend).unwrap();
        assert!(!backend.native.get());
        assert!(backend.calls.borrow().is_empty());
    }

    #[test]
    fn disable_cleans_both_and_still_cleans_legacy_on_native_failure() {
        for fail_native in [false, true] {
            let backend = LoginItems::new(fail_native);
            backend.native.set(true);
            assert_eq!(disable(&backend).is_err(), fail_native);
            assert_eq!(backend.native.get(), fail_native);
            assert!(!backend.legacy.get());
            assert_eq!(*backend.calls.borrow(), ["unregister", "remove legacy"]);
        }
    }
}
