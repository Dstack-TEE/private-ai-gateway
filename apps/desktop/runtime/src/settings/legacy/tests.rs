use std::{cell::RefCell, collections::BTreeMap};

use super::*;
use crate::settings::Settings;

/// A credential store holding what 0.1 saved; it can be made unavailable.
#[derive(Default)]
struct FakeKeychain {
    entries: RefCell<BTreeMap<String, String>>,
    reads: RefCell<Vec<String>>,
    unavailable: bool,
}

impl FakeKeychain {
    fn with(entries: &[(&str, &str)]) -> Self {
        Self {
            entries: RefCell::new(
                entries
                    .iter()
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .collect(),
            ),
            ..Self::default()
        }
    }
}

impl Keychain for FakeKeychain {
    fn get(&self, entry: &str) -> Result<Option<String>, String> {
        self.reads.borrow_mut().push(entry.to_string());
        if self.unavailable {
            return Err("the system credential store is unavailable (locked)".into());
        }
        Ok(self.entries.borrow().get(entry).cloned())
    }

    fn delete(&self, entry: &str) -> Result<(), String> {
        if self.unavailable {
            return Err("locked".into());
        }
        self.entries.borrow_mut().remove(entry);
        Ok(())
    }
}

const PASSWORD_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$yDfPOVa8oT6MYl9yP0ZvGnJpeEYnMe0OOi0q4mQvAsE";

fn service_json() -> String {
    serde_json::json!({
        "version": 1,
        "activeProfileId": "home",
        "requireProductionOs": false,
        "profiles": [
            {
                "id": "work",
                "credentialRef": "credential-0b1c",
                "name": "Work",
                "provider": "redpill",
                "remoteUrl": "https://tee.redpill.ai",
                "auth": {"kind": "oauth", "accountId": "user_1", "accountName": "Me"},
                "credentialSaved": true,
                "verifiedAt": 1700000000
            },
            {
                "id": "home",
                "name": "Home",
                "provider": "custom",
                "remoteUrl": "https://gateway.example",
                "auth": {"kind": "apiKey"},
                "credentialSaved": true
            },
            {
                "id": "spare",
                "name": "Spare",
                "provider": "custom",
                "remoteUrl": "http://127.0.0.1:8090",
                "auth": {"kind": "apiKey"},
                "credentialSaved": false
            }
        ]
    })
    .to_string()
}

fn preferences_json() -> String {
    serde_json::json!({
        "connectOnLaunch": true,
        "appearance": "dark",
        "updateChannel": "beta",
        "autoCliRegistration": false,
        "notifications": {"enabled": true, "gateway": false, "localApi": true, "verification": true},
        "webUi": {"enabled": true, "listenAddress": "127.0.0.1", "allowNetworkAccess": false, "port": 4190},
        "webUiPasswordHash": PASSWORD_HASH
    })
    .to_string()
}

fn keychain() -> FakeKeychain {
    FakeKeychain::with(&[
        ("service-profile-credential-0b1c-api-key", "sk-work"),
        ("service-profile-credential-old-api-key", "sk-retired"),
        ("service-profile-home-api-key", "sk-home"),
        (
            "account-cleanup-7f3e",
            r#"{"profile_id":"work","action":"revoke","provider":"redpill","key":"sk-retired","entry":"service-profile-credential-old-api-key","revoke":true}"#,
        ),
        ("restore:claude-code:ab12", "sk-ant-original"),
    ])
}

/// Writes a complete 0.1 data directory.
fn write_legacy(data: &Path) {
    fs::create_dir_all(data).unwrap();
    fs::write(data.join(SERVICE_FILE), service_json()).unwrap();
    fs::write(
        data.join(LOCAL_API_FILE),
        r#"{"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":5180}"#,
    )
    .unwrap();
    fs::write(data.join(PREFERENCES_FILE), preferences_json()).unwrap();
    fs::write(data.join(CLEANUP_FILE), r#"["account-cleanup-7f3e"]"#).unwrap();
    fs::write(
        data.join(AGENTS_FILE),
        serde_json::json!({
            "claude-code": {
                "config_path": "/home/user/.claude/settings.json",
                "fields": [
                    {"path": ["env", "ANTHROPIC_AUTH_TOKEN"], "value": null, "previous": {"secret_ref": "restore:claude-code:ab12"}},
                    {"path": ["env", "ANTHROPIC_BASE_URL"], "value": null, "previous": "https://api.anthropic.com"}
                ],
                "suspended": false,
                "options": {},
                "disabled": false,
                "cleanup_pending": false
            }
        })
        .to_string(),
    )
    .unwrap();
}

fn assert_fully_migrated(config_dir: &Path, data: &Path, keychain: &FakeKeychain) {
    let config = config::parse(&fs::read_to_string(config_dir.join(CONFIG_FILE)).unwrap()).unwrap();
    assert_eq!(config.active_profile, "home");
    assert!(!config.require_production_os);
    assert!(config.connect_on_launch);
    assert_eq!(config.appearance, Appearance::Dark);
    assert_eq!(config.update_channel, Some(UpdateChannel::Beta));
    assert_eq!(config.auto_cli_registration, Some(false));
    assert!(!config.notifications.gateway);
    assert_eq!(config.local_api.port, 5180);
    assert!(config.web_ui.enabled && config.web_ui.port == 4190);
    assert_eq!(
        config.profiles.keys().collect::<Vec<_>>(),
        ["work", "home", "spare"]
    );
    assert_eq!(config.profiles["work"].verified_at, Some(1700000000));
    assert!(!config.profiles["work"].auth.is_api_key());

    let text = fs::read_to_string(config_dir.join(CREDENTIALS_FILE)).unwrap();
    let credentials = parse_credentials(&text).unwrap();
    assert_eq!(credentials.profiles["work"].api_key, "sk-work");
    assert_eq!(credentials.profiles["home"].api_key, "sk-home");
    assert!(!credentials.profiles.contains_key("spare"));
    assert_eq!(
        credentials.web_ui.password_hash.as_deref(),
        Some(PASSWORD_HASH)
    );
    // Device-local secrets go with this machine's state, not the syncable credentials.
    let local = local_state::load(&data.join(LOCAL_STATE_FILE)).unwrap();
    assert_eq!(
        local.agent_restore["restore:claude-code:ab12"],
        "sk-ant-original"
    );
    assert!(!text.contains("sk-ant-original") && !text.contains("sk-retired"));
    assert_eq!(
        local.account_cleanup["7f3e"],
        RetiredCredential {
            profile_id: "work".into(),
            action: "revoke".into(),
            provider: ServiceProvider::Redpill,
            key: "sk-retired".into(),
            revoke: true,
        }
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(config_dir.join(CREDENTIALS_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Secrets live only in credentials.toml now; nothing leaks into config.toml.
    assert!(
        keychain.entries.borrow().is_empty(),
        "{:?}",
        keychain.entries
    );
    let config_text = fs::read_to_string(config_dir.join(CONFIG_FILE)).unwrap();
    assert!(config_text.starts_with(CONFIG_HEADER));
    for secret in ["sk-", "argon2", "credentialRef", "credentialSaved"] {
        assert!(!config_text.contains(secret), "{secret} in {config_text}");
    }
    // The old files are moved to the one-time backup; state stays.
    for name in LEGACY_FILES {
        assert!(!data.join(name).exists(), "{name}");
        assert!(data.join(BACKUP_DIR).join(name).exists(), "{name}");
    }
    assert!(data.join(AGENTS_FILE).exists());
}

#[test]
fn a_linux_layout_moves_settings_to_the_config_directory() {
    let root = tempfile::tempdir().unwrap();
    let (config_dir, data) = (root.path().join("config"), root.path().join("data"));
    write_legacy(&data);
    let keychain = keychain();
    let notices = migrate(&config_dir, &data, &keychain).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_fully_migrated(&config_dir, &data, &keychain);

    // Idempotent: a second start neither reads the store nor rewrites anything.
    let before = fs::read_to_string(config_dir.join(CONFIG_FILE)).unwrap();
    keychain.reads.borrow_mut().clear();
    assert!(migrate(&config_dir, &data, &keychain).unwrap().is_empty());
    assert!(keychain.reads.borrow().is_empty());
    assert_eq!(
        fs::read_to_string(config_dir.join(CONFIG_FILE)).unwrap(),
        before
    );
}

#[test]
fn macos_windows_and_mac_app_store_layouts_use_a_config_subdirectory() {
    // Direct macOS, Windows and the MAS container keep one app directory;
    // settings move into its `Config` subdirectory, state stays put.
    let root = tempfile::tempdir().unwrap();
    let container = root
        .path()
        .join("Library/Containers/org.dstack.private-ai-proxy/Data/Library/Application Support/org.dstack.private-ai-proxy");
    let settings_dir = container.join("Config");
    write_legacy(&container);
    let keychain = keychain();
    assert!(migrate(&settings_dir, &container, &keychain)
        .unwrap()
        .is_empty());
    assert_fully_migrated(&settings_dir, &container, &keychain);
    let mut names: Vec<_> = fs::read_dir(&settings_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [CONFIG_FILE, CREDENTIALS_FILE],
        "state never lands in Config"
    );
    let (settings, problems) = Settings::open(settings_dir, &container);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        settings.profile_key("work").unwrap().as_deref(),
        Some("sk-work")
    );
}

#[test]
fn partial_layouts_import_what_exists() {
    for present in [
        &[PREFERENCES_FILE][..],
        &[LOCAL_API_FILE],
        &[SERVICE_FILE],
        &[SERVICE_FILE, LOCAL_API_FILE],
    ] {
        let root = tempfile::tempdir().unwrap();
        let (config_dir, data) = (root.path().join("config"), root.path().join("data"));
        write_legacy(&data);
        for name in LEGACY_FILES {
            if !present.contains(&name) {
                let _ = fs::remove_file(data.join(name));
            }
        }
        let keychain = keychain();
        assert!(migrate(&config_dir, &data, &keychain).unwrap().is_empty());
        let config =
            config::parse(&fs::read_to_string(config_dir.join(CONFIG_FILE)).unwrap()).unwrap();
        assert_eq!(
            config.local_api.port,
            if present.contains(&LOCAL_API_FILE) {
                5180
            } else {
                4180
            },
            "{present:?}"
        );
        assert_eq!(
            config.connect_on_launch,
            present.contains(&PREFERENCES_FILE)
        );
        assert_eq!(
            config.profiles.len(),
            if present.contains(&SERVICE_FILE) {
                3
            } else {
                0
            }
        );
        let credentials = config_dir.join(CREDENTIALS_FILE);
        if present == [LOCAL_API_FILE] {
            // No secrets to import: the credential store is never touched.
            assert!(keychain
                .reads
                .borrow()
                .iter()
                .all(|entry| entry.starts_with("restore:")));
        }
        if present.contains(&SERVICE_FILE) {
            let saved = parse_credentials(&fs::read_to_string(&credentials).unwrap()).unwrap();
            assert_eq!(saved.profiles["home"].api_key, "sk-home");
        }
        for name in present {
            assert!(data.join(BACKUP_DIR).join(name).exists());
        }
    }
}

#[test]
fn a_fresh_install_imports_nothing_and_never_asks_the_credential_store() {
    let root = tempfile::tempdir().unwrap();
    let keychain = FakeKeychain {
        unavailable: true,
        ..FakeKeychain::default()
    };
    let notices = migrate(
        &root.path().join("config"),
        &root.path().join("data"),
        &keychain,
    )
    .unwrap();
    assert!(notices.is_empty());
    assert!(keychain.reads.borrow().is_empty());
    assert!(!root.path().join("config").join(CONFIG_FILE).exists());
}

#[test]
fn an_unavailable_credential_store_imports_settings_and_asks_to_reenter_keys() {
    let root = tempfile::tempdir().unwrap();
    let (config_dir, data) = (root.path().join("config"), root.path().join("data"));
    write_legacy(&data);
    let keychain = FakeKeychain {
        unavailable: true,
        ..keychain()
    };
    let notices = migrate(&config_dir, &data, &keychain).unwrap();
    assert_eq!(
        keychain.reads.borrow().len(),
        1,
        "asked once, not per entry"
    );
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("locked"), "{}", notices[0]);
    assert!(
        notices[0].contains("Re-enter the API key for: Work, Home"),
        "{}",
        notices[0]
    );
    let config = config::parse(&fs::read_to_string(config_dir.join(CONFIG_FILE)).unwrap()).unwrap();
    assert_eq!(config.profiles.len(), 3);
    // The web UI password hash was never in the credential store.
    let credentials =
        parse_credentials(&fs::read_to_string(config_dir.join(CREDENTIALS_FILE)).unwrap()).unwrap();
    assert!(credentials.profiles.is_empty());
    assert_eq!(
        credentials.web_ui.password_hash.as_deref(),
        Some(PASSWORD_HASH)
    );
    // Nothing was deleted from the store.
    assert_eq!(keychain.entries.borrow().len(), 5);
}

#[test]
fn an_interrupted_import_resumes_without_losing_secrets() {
    let root = tempfile::tempdir().unwrap();
    let (config_dir, data) = (root.path().join("config"), root.path().join("data"));
    write_legacy(&data);
    // Crash after credentials.toml was written and the store entries deleted,
    // before config.toml: the rerun must keep the secrets it already moved.
    let keychain = keychain();
    migrate(&config_dir, &data, &keychain).unwrap();
    fs::remove_file(config_dir.join(CONFIG_FILE)).unwrap();
    for name in LEGACY_FILES {
        fs::rename(data.join(BACKUP_DIR).join(name), data.join(name)).unwrap();
    }
    assert!(keychain.entries.borrow().is_empty());
    assert!(migrate(&config_dir, &data, &keychain).unwrap().is_empty());
    assert_fully_migrated(&config_dir, &data, &keychain);

    // Crash after config.toml, before the backup: the next start only moves files.
    for name in LEGACY_FILES {
        fs::rename(data.join(BACKUP_DIR).join(name), data.join(name)).unwrap();
    }
    let before = fs::read_to_string(config_dir.join(CREDENTIALS_FILE)).unwrap();
    assert!(migrate(&config_dir, &data, &keychain).unwrap().is_empty());
    assert_fully_migrated(&config_dir, &data, &keychain);
    assert_eq!(
        fs::read_to_string(config_dir.join(CREDENTIALS_FILE)).unwrap(),
        before
    );
}

#[test]
fn unreadable_old_files_are_reported_and_kept() {
    let root = tempfile::tempdir().unwrap();
    let (config_dir, data) = (root.path().join("config"), root.path().join("data"));
    write_legacy(&data);
    fs::write(data.join(SERVICE_FILE), "{ not json").unwrap();
    let notices = migrate(&config_dir, &data, &keychain()).unwrap();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains(SERVICE_FILE), "{}", notices[0]);
    assert_eq!(
        fs::read_to_string(data.join(BACKUP_DIR).join(SERVICE_FILE)).unwrap(),
        "{ not json"
    );
    let config = config::parse(&fs::read_to_string(config_dir.join(CONFIG_FILE)).unwrap()).unwrap();
    assert!(config.profiles.is_empty());
    assert_eq!(config.appearance, Appearance::Dark);
}

/// Real OS credential store round trip; run explicitly on a desktop OS (CI
/// runs it on every packaged platform).
#[test]
#[ignore = "touches the OS credential store"]
fn legacy_keychain_import_round_trip() {
    let entry = format!(
        "service-profile-import-check-{}-api-key",
        std::process::id()
    );
    OsKeychain::run(|| {
        OsKeychain::entry(&entry)?
            .set_password("sk-import-check")
            .map_err(store_error)
    })
    .unwrap();
    assert_eq!(
        OsKeychain.get(&entry).unwrap().as_deref(),
        Some("sk-import-check")
    );
    OsKeychain.delete(&entry).unwrap();
    assert_eq!(OsKeychain.get(&entry).unwrap(), None);
}
