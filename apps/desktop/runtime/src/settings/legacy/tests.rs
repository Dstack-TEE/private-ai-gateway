use std::{cell::RefCell, collections::BTreeMap};

use super::*;

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

/// One start of 0.2: step 1 while the settings open, then step 2 as the
/// service does once it is listening.
fn start(config_dir: &Path, data: &Path, keychain: &FakeKeychain) -> (Settings, Vec<String>) {
    let (settings, mut problems) = Settings::open(config_dir.to_path_buf(), data);
    if settings.import_ready() && secrets_pending(data) {
        let local = LocalState::open(data);
        let notices = import_secrets(&settings, &local, data, keychain).unwrap();
        settings.add_import_notices(&notices);
        problems.extend(notices);
    }
    (settings, problems)
}

fn read_config(config_dir: &Path) -> Config {
    config::parse(&fs::read_to_string(config_dir.join(CONFIG_FILE)).unwrap())
        .unwrap()
        .value
}

fn read_credentials(config_dir: &Path) -> Credentials {
    parse_credentials(&fs::read_to_string(config_dir.join(CREDENTIALS_FILE)).unwrap())
        .unwrap()
        .value
}

fn assert_fully_migrated(config_dir: &Path, data: &Path, keychain: &FakeKeychain) {
    let config = read_config(config_dir);
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
    let credentials = parse_credentials(&text).unwrap().value;
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
    for secret in ["sk-", "argon2", "credential-ref", "credential-saved"] {
        assert!(!config_text.contains(secret), "{secret} in {config_text}");
    }
    // The old files are moved to the backup, which records the import; state stays.
    for name in LEGACY_FILES {
        assert!(!data.join(name).exists(), "{name}");
        assert!(data.join(BACKUP_DIR).join(name).exists(), "{name}");
    }
    assert!(data.join(AGENTS_FILE).exists());
    assert!(!secrets_pending(data));
}

#[test]
fn a_linux_layout_moves_settings_to_the_config_directory() {
    let root = tempfile::tempdir().unwrap();
    let (config_dir, data) = (root.path().join("config"), root.path().join("data"));
    write_legacy(&data);
    let keychain = keychain();
    let (settings, problems) = start(&config_dir, &data, &keychain);
    assert!(problems.is_empty(), "{problems:?}");
    assert_fully_migrated(&config_dir, &data, &keychain);
    assert_eq!(
        settings.profile_key("home").unwrap().as_deref(),
        Some("sk-home")
    );
    assert_eq!(settings.files().error, None);

    // Done: a second start neither reads the store nor rewrites anything.
    let before = fs::read_to_string(config_dir.join(CONFIG_FILE)).unwrap();
    keychain.reads.borrow_mut().clear();
    let (_, problems) = start(&config_dir, &data, &keychain);
    assert!(problems.is_empty(), "{problems:?}");
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
    let (settings, problems) = start(&settings_dir, &container, &keychain);
    assert!(problems.is_empty(), "{problems:?}");
    assert_fully_migrated(&settings_dir, &container, &keychain);
    let mut names: Vec<_> = fs::read_dir(&settings_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [config::SCHEMA_FILE, CONFIG_FILE, CREDENTIALS_FILE],
        "state never lands in Config"
    );
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
        &[CLEANUP_FILE],
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
        let (_, problems) = start(&config_dir, &data, &keychain);
        assert!(problems.is_empty(), "{present:?}: {problems:?}");
        let config = read_config(&config_dir);
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
        if !present.contains(&SERVICE_FILE) && !present.contains(&CLEANUP_FILE) {
            // No keys to import: the store is asked only for agent restore values.
            assert!(keychain
                .reads
                .borrow()
                .iter()
                .all(|entry| entry.starts_with("restore:")));
        }
        if present.contains(&SERVICE_FILE) {
            assert_eq!(
                read_credentials(&config_dir).profiles["home"].api_key,
                "sk-home"
            );
        }
        for name in present {
            assert!(data.join(BACKUP_DIR).join(name).exists());
        }
        assert!(!secrets_pending(&data));
    }
}

#[test]
fn a_fresh_install_imports_nothing_and_never_asks_the_credential_store() {
    let root = tempfile::tempdir().unwrap();
    let (config_dir, data) = (root.path().join("config"), root.path().join("data"));
    let keychain = FakeKeychain {
        unavailable: true,
        ..FakeKeychain::default()
    };
    let (settings, problems) = start(&config_dir, &data, &keychain);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(keychain.reads.borrow().is_empty());
    assert!(!data.join(BACKUP_DIR).exists());
    assert_eq!(read_config(&config_dir), Config::default());
    assert!(settings.files().warnings.is_empty());
}

#[test]
fn an_unavailable_credential_store_imports_settings_and_retries_the_keys() {
    let root = tempfile::tempdir().unwrap();
    let (config_dir, data) = (root.path().join("config"), root.path().join("data"));
    write_legacy(&data);
    let locked = FakeKeychain {
        unavailable: true,
        ..keychain()
    };
    let (settings, problems) = start(&config_dir, &data, &locked);
    assert_eq!(locked.reads.borrow().len(), 1, "asked once, not per entry");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("locked"), "{}", problems[0]);
    assert!(
        problems[0].contains("Re-enter the API key for: Work, Home"),
        "{}",
        problems[0]
    );
    assert!(problems[0].contains("retried on the next start"));
    // The settings are in effect; the web UI password hash was never in the store.
    assert_eq!(read_config(&config_dir).profiles.len(), 3);
    let credentials = read_credentials(&config_dir);
    assert!(credentials.profiles.is_empty());
    assert_eq!(
        credentials.web_ui.password_hash.as_deref(),
        Some(PASSWORD_HASH)
    );
    assert_eq!(settings.files().error, None);
    // Nothing was deleted from the store and the step is still pending.
    assert_eq!(locked.entries.borrow().len(), 5);
    assert!(secrets_pending(&data));

    // Meanwhile the user re-enters one key; the next start imports the rest
    // and keeps what the user entered.
    settings
        .update_credentials(|credentials| {
            credentials.profiles.insert(
                "home".into(),
                ProfileCredential {
                    api_key: "sk-home-new".into(),
                },
            );
            Ok(())
        })
        .unwrap();
    drop(settings);
    let unlocked = FakeKeychain {
        entries: RefCell::new(locked.entries.borrow().clone()),
        ..FakeKeychain::default()
    };
    let (settings, problems) = start(&config_dir, &data, &unlocked);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        settings.profile_key("home").unwrap().as_deref(),
        Some("sk-home-new")
    );
    assert_eq!(
        settings.profile_key("work").unwrap().as_deref(),
        Some("sk-work")
    );
    assert!(!secrets_pending(&data));
    // The home entry was not needed, so it stays; everything imported is gone.
    assert_eq!(
        unlocked.entries.borrow().keys().collect::<Vec<_>>(),
        ["service-profile-home-api-key"]
    );
}

#[test]
fn a_failed_import_writes_nothing_and_is_retried_on_the_next_start() {
    let root = tempfile::tempdir().unwrap();
    let (config_dir, data) = (root.path().join("config"), root.path().join("data"));
    write_legacy(&data);
    // An old file that cannot be read fails step 1.
    let preferences = data.join(PREFERENCES_FILE);
    fs::remove_file(&preferences).unwrap();
    fs::create_dir(&preferences).unwrap();
    let keychain = keychain();
    let (settings, problems) = start(&config_dir, &data, &keychain);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains(PREFERENCES_FILE), "{}", problems[0]);
    // Nothing that looks like a finished import: no config.toml, no backup,
    // the old files and store entries untouched, the store never asked.
    assert!(!config_dir.join(CONFIG_FILE).exists());
    assert!(!config_dir.join(CREDENTIALS_FILE).exists());
    assert!(!data.join(BACKUP_DIR).exists());
    for name in [SERVICE_FILE, LOCAL_API_FILE, CLEANUP_FILE] {
        assert!(data.join(name).exists(), "{name}");
    }
    assert!(keychain.reads.borrow().is_empty());
    assert_eq!(keychain.entries.borrow().len(), 5);
    // The error stays in the state (and `pap doctor`), and nothing can be
    // saved that would make the next start think the import happened.
    let error = settings.files().error.unwrap();
    assert!(error.contains("could not be imported"), "{error}");
    let refused = settings
        .update_config(|config| {
            config.appearance = Appearance::Light;
            Ok(())
        })
        .unwrap_err();
    assert!(
        refused.starts_with("Settings from 0.1 are not imported yet"),
        "{refused}"
    );
    assert!(settings.update_credentials(|_| Ok(())).is_err());
    assert!(!config_dir.join(CONFIG_FILE).exists());
    drop(settings);

    // Fixed: the next start imports everything.
    fs::remove_dir(&preferences).unwrap();
    fs::write(&preferences, preferences_json()).unwrap();
    let (settings, problems) = start(&config_dir, &data, &keychain);
    assert!(problems.is_empty(), "{problems:?}");
    assert_fully_migrated(&config_dir, &data, &keychain);
    assert_eq!(settings.files().error, None);
}

#[test]
fn a_synced_config_on_a_second_device_keeps_its_values_and_gets_this_devices_secrets() {
    let root = tempfile::tempdir().unwrap();
    let (config_dir, data) = (root.path().join("config"), root.path().join("data"));
    write_legacy(&data);
    // Another device already upgraded and synced the settings directory. Its
    // `work` profile is the same service signed in to another account, `home`
    // is the same service with a manual key, and it has a password but no
    // API keys.
    fs::create_dir_all(&config_dir).unwrap();
    let synced = "# Synced from my laptop\n\
        active-profile = \"work\"\n\
        appearance = \"light\"\n\
        \n\
        [profiles.work]\n\
        name = \"Work laptop\"\n\
        provider = \"redpill\"\n\
        remote-url = \"https://tee.redpill.ai\"\n\
        auth = { kind = \"oauth\", account-id = \"user_2\" }\n\
        \n\
        [profiles.home]\n\
        name = \"Home\"\n\
        provider = \"custom\"\n\
        remote-url = \"https://gateway.example\"\n";
    fs::write(config_dir.join(CONFIG_FILE), synced).unwrap();
    let laptop_hash = crate::web_ui::password::hash("laptop password").unwrap();
    fs::write(
        config_dir.join(CREDENTIALS_FILE),
        format!("[web-ui]\npassword-hash = \"{laptop_hash}\"\n"),
    )
    .unwrap();
    let keychain = keychain();
    let (settings, problems) = start(&config_dir, &data, &keychain);

    // config.toml: every synced value stays; only this device's missing profile is added.
    let config = read_config(&config_dir);
    assert_eq!(config.active_profile, "work");
    assert_eq!(config.appearance, Appearance::Light);
    assert_eq!(config.local_api.port, 4180);
    assert!(!config.connect_on_launch);
    assert_eq!(config.profiles["work"].name, "Work laptop");
    assert!(config.profiles["home"].auth.is_api_key());
    assert_eq!(
        config.profiles.keys().collect::<Vec<_>>(),
        ["work", "home", "spare"]
    );
    let text = fs::read_to_string(config_dir.join(CONFIG_FILE)).unwrap();
    assert!(text.starts_with(synced), "{text}");
    // credentials.toml: the synced password stays; this device's key for the
    // same service and account is imported, the one for another account is not.
    let credentials = read_credentials(&config_dir);
    assert_eq!(
        credentials.web_ui.password_hash.as_deref(),
        Some(laptop_hash.as_str())
    );
    assert_eq!(credentials.profiles["home"].api_key, "sk-home");
    assert!(!credentials.profiles.contains_key("work"));
    assert_eq!(
        settings.profile_key("home").unwrap().as_deref(),
        Some("sk-home")
    );
    // Device-local state is always this device's.
    let local = local_state::load(&data.join(LOCAL_STATE_FILE)).unwrap();
    assert_eq!(
        local.agent_restore["restore:claude-code:ab12"],
        "sk-ant-original"
    );
    assert!(local.account_cleanup.contains_key("7f3e"));
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].contains("already existed"), "{}", problems[0]);
    assert!(problems[1].contains("Work"), "{}", problems[1]);
    assert_eq!(settings.files().warnings, problems);
    assert!(!secrets_pending(&data));
    for name in LEGACY_FILES {
        assert!(data.join(BACKUP_DIR).join(name).exists(), "{name}");
    }
    // The key that was not imported stays in the store.
    assert_eq!(
        keychain.entries.borrow().keys().collect::<Vec<_>>(),
        ["service-profile-credential-0b1c-api-key"]
    );
}

#[test]
fn a_crash_between_steps_resumes_without_losing_anything() {
    let root = tempfile::tempdir().unwrap();
    let (config_dir, data) = (root.path().join("config"), root.path().join("data"));
    write_legacy(&data);
    let keychain = keychain();

    // Crash in step 1 after the settings files were written, before the old
    // files moved. The user changed a setting before the next start.
    let (settings, _) = Settings::open(config_dir.clone(), &data);
    settings
        .update_config(|config| {
            config.appearance = Appearance::Light;
            Ok(())
        })
        .unwrap();
    drop(settings);
    for name in LEGACY_FILES {
        fs::rename(data.join(BACKUP_DIR).join(name), data.join(name)).unwrap();
    }
    let (_, problems) = start(&config_dir, &data, &keychain);
    assert!(problems[0].contains("already existed"), "{problems:?}");
    let config = read_config(&config_dir);
    assert_eq!(
        config.appearance,
        Appearance::Light,
        "the user's change stays"
    );
    assert_eq!(config.profiles.len(), 3);
    assert_eq!(
        read_credentials(&config_dir).profiles["work"].api_key,
        "sk-work"
    );
    assert!(!secrets_pending(&data));

    // Crash in step 2 after the secrets were written, before the store
    // entries were deleted and the step was recorded; meanwhile the user
    // replaced the home key. The rerun deletes every entry whose value the
    // files already hold, so no plaintext copy stays behind, and keeps the one
    // that no longer matches.
    fs::remove_file(data.join(BACKUP_DIR).join(COMPLETE_FILE)).unwrap();
    let path = config_dir.join(CREDENTIALS_FILE);
    let replaced = fs::read_to_string(&path)
        .unwrap()
        .replace("\"sk-home\"", "\"sk-home-new\"");
    fs::write(&path, &replaced).unwrap();
    let restored = self::keychain();
    let (_, problems) = start(&config_dir, &data, &restored);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), replaced);
    assert_eq!(
        restored.entries.borrow().keys().collect::<Vec<_>>(),
        ["service-profile-home-api-key"]
    );
    assert!(!secrets_pending(&data));

    // Crash after the entries were deleted, before the step was recorded.
    fs::remove_file(data.join(BACKUP_DIR).join(COMPLETE_FILE)).unwrap();
    let empty = FakeKeychain::default();
    let (_, problems) = start(&config_dir, &data, &empty);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(!secrets_pending(&data));
    assert_eq!(
        read_credentials(&config_dir).profiles["home"].api_key,
        "sk-home-new"
    );
}

#[test]
fn unreadable_old_files_are_reported_and_kept() {
    let root = tempfile::tempdir().unwrap();
    let (config_dir, data) = (root.path().join("config"), root.path().join("data"));
    write_legacy(&data);
    fs::write(data.join(SERVICE_FILE), "{ not json").unwrap();
    let (_, problems) = start(&config_dir, &data, &keychain());
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains(SERVICE_FILE), "{}", problems[0]);
    assert_eq!(
        fs::read_to_string(data.join(BACKUP_DIR).join(SERVICE_FILE)).unwrap(),
        "{ not json"
    );
    let config = read_config(&config_dir);
    assert!(config.profiles.is_empty());
    assert_eq!(config.appearance, Appearance::Dark);
}

#[test]
fn a_disconnect_before_the_credential_import_fails_instead_of_dropping_the_key() {
    use agent_bridge::{
        agents::{helper_binary_name, AgentError, Projector},
        catalog::Catalog,
    };
    use desktop_core::agents::{Agent, ConnectOptions};
    use std::sync::Arc;

    let root = tempfile::tempdir().unwrap();
    let (home, config_dir, data) = (
        root.path().join("home"),
        root.path().join("config"),
        root.path().join("data"),
    );
    let helper = home.join("Private AI Proxy.app").join(helper_binary_name());
    for path in [&helper, &data.join("helpers").join(helper_binary_name())] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let local = Arc::new(LocalState::open(&data));
    let projector = Projector::new_for_home(
        home.clone(),
        data.clone(),
        helper,
        "http://127.0.0.1:4180",
        local.clone(),
    )
    .unwrap();
    let claude = home.join(".claude").join("settings.json");
    fs::create_dir_all(claude.parent().unwrap()).unwrap();
    fs::write(
        &claude,
        r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-ant-original"}}"#,
    )
    .unwrap();
    let catalog = Catalog::from_remote(
        &serde_json::json!({ "data": [{ "id": "openai/gpt-oss-20b", "context_length": 131072 }] }),
        1,
    )
    .unwrap();
    let options = ConnectOptions {
        default_model: Some("openai/gpt-oss-20b".into()),
    };
    let apply = |connect: bool| {
        let catalog = connect.then_some(&catalog);
        projector
            .preview(Agent::ClaudeCode, connect, catalog, &options)
            .and_then(|preview| {
                projector.apply(
                    Agent::ClaudeCode,
                    connect,
                    &preview.revision,
                    catalog,
                    &options,
                )
            })
    };
    apply(true).unwrap();

    // As in 0.1: the original key is parked in the credential store, not in
    // local-state.json, and the store times out on the first 0.2 start.
    let parked = std::mem::take(&mut local.read().unwrap().agent_restore);
    assert_eq!(parked.values().collect::<Vec<_>>(), ["sk-ant-original"]);
    local
        .update(|secrets| {
            secrets.agent_restore.clear();
            Ok(())
        })
        .unwrap();
    fs::write(data.join(PREFERENCES_FILE), "{}").unwrap();
    let entries: Vec<(&str, &str)> = parked
        .iter()
        .map(|(entry, value)| (entry.as_str(), value.as_str()))
        .collect();
    let locked = FakeKeychain {
        unavailable: true,
        ..FakeKeychain::with(&entries)
    };
    let (settings, _) = Settings::open(config_dir.clone(), &data);
    local.set_importing(secrets_pending(&data));
    let notices = import_secrets(&settings, &local, &data, &locked).unwrap();
    assert!(
        notices[0].contains("retried on the next start"),
        "{notices:?}"
    );

    // The disconnect fails like one while 0.1's store was unavailable, and
    // the agent's configuration keeps what it had.
    let connected = fs::read(&claude).unwrap();
    assert!(matches!(apply(false), Err(AgentError::CredentialStore)));
    assert_eq!(fs::read(&claude).unwrap(), connected);

    // The next start imports the key; the disconnect then puts it back.
    drop(settings);
    let (settings, _) = Settings::open(config_dir, &data);
    let unlocked = FakeKeychain::with(&entries);
    import_secrets(&settings, &local, &data, &unlocked).unwrap();
    assert!(!local.importing());
    apply(false).unwrap();
    let restored: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&claude).unwrap()).unwrap();
    assert_eq!(restored["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-ant-original");
    assert!(unlocked.entries.borrow().is_empty());
}

#[test]
fn restore_values_still_importing_are_never_read_as_absent() {
    use agent_bridge::secrets::SecretStore;
    let root = tempfile::tempdir().unwrap();
    let local = LocalState::open(root.path());
    local.set_importing(true);
    assert!(local.get("restore:codex:1").is_err());
    local.set("restore:codex:2", "sk-new").unwrap();
    assert_eq!(
        local.get("restore:codex:2").unwrap().as_deref(),
        Some("sk-new")
    );
    local.set_importing(false);
    assert_eq!(local.get("restore:codex:1").unwrap(), None);
}

/// Real OS credential store round trip, including the deadline every store
/// operation runs under. Run explicitly on a desktop OS; CI runs it on
/// Linux (Secret Service), Windows (Credential Manager) and macOS (Keychain).
#[test]
#[ignore = "touches the OS credential store"]
fn legacy_keychain_import_round_trip() {
    let entry = format!(
        "service-profile-import-check-{}-api-key",
        std::process::id()
    );
    let name = entry.clone();
    OsKeychain::run(move || {
        OsKeychain::entry(&name)?
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
