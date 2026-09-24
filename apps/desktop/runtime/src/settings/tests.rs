use super::*;
use desktop_core::{config::Appearance, contracts::ServiceProvider};

fn open(dir: &Path) -> Settings {
    let (settings, problems) = Settings::open(dir.join("config"), &dir.join("data"));
    assert!(problems.is_empty(), "{problems:?}");
    settings
}

fn config_text(dir: &Path) -> String {
    fs::read_to_string(dir.join("config").join(CONFIG_FILE)).unwrap()
}

#[test]
fn a_new_settings_directory_gets_a_commented_file_and_its_schema() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());
    assert_eq!(config_text(dir.path()), CONFIG_HEADER);
    let schema = fs::read_to_string(dir.path().join("config").join(SCHEMA_FILE)).unwrap();
    assert_eq!(schema, config::schema());
    assert!(!dir.path().join("config").join(CREDENTIALS_FILE).exists());
    assert_eq!(settings.config().unwrap(), Config::default());
    let files = settings.files();
    assert!(files.config_path.ends_with(CONFIG_FILE));
    assert!(files.credentials_path.ends_with(CREDENTIALS_FILE));
    assert_eq!(files.error, None);
}

#[test]
fn programmatic_writes_keep_comments_order_and_untouched_keys() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());
    let original = "# My settings\n\
        appearance = \"dark\" # night owl\n\
        \n\
        # The listener agents use.\n\
        [localApi]\n\
        port = 4190 # moved off the default\n";
    fs::write(dir.path().join("config").join(CONFIG_FILE), original).unwrap();
    settings.reload().unwrap();
    assert_eq!(settings.config().unwrap().appearance, Appearance::Dark);

    settings
        .update_config(|config| {
            config.appearance = Appearance::Light;
            config.web_ui.enabled = true;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        config_text(dir.path()),
        "# My settings\n\
         appearance = \"light\" # night owl\n\
         \n\
         # The listener agents use.\n\
         [localApi]\n\
         port = 4190 # moved off the default\n\
         \n\
         [webUi]\n\
         enabled = true\n"
    );
    // A no-op change does not rewrite the file.
    settings.update_config(|_| Ok(())).unwrap();
    assert!(config_text(dir.path()).contains("night owl"));
}

#[test]
fn profiles_are_added_and_removed_as_tables() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());
    let profile = |name: &str| config::Profile {
        name: name.into(),
        provider: ServiceProvider::Custom,
        remote_url: "https://gateway.example".into(),
        auth: Default::default(),
        verified_at: None,
    };
    settings
        .update_config(|config| {
            config.upsert("work".into(), profile("Work"))?;
            config.upsert("home".into(), profile("Home"))
        })
        .unwrap();
    let text = config_text(dir.path());
    assert!(text.contains("activeProfile = \"work\""), "{text}");
    assert!(text.contains("[profiles.work]\nname = \"Work\""), "{text}");
    assert!(text.contains("[profiles.home]"), "{text}");
    settings
        .update_config(|config| {
            config.profiles.shift_remove("work");
            config.active_profile.clear();
            Ok(())
        })
        .unwrap();
    let text = config_text(dir.path());
    assert!(!text.contains("[profiles.work]"), "{text}");
    assert!(text.contains("activeProfile = \"home\""), "{text}");
    assert_eq!(config::parse(&text).unwrap(), settings.config().unwrap());
}

#[test]
fn an_invalid_edit_keeps_the_last_good_settings_and_blocks_writes() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());
    let path = dir.path().join("config").join(CONFIG_FILE);
    fs::write(&path, "[localApi]\nport = 5180\n").unwrap();
    let (_, current) = settings.reload().unwrap();
    assert_eq!(current.config.local_api.port, 5180);

    fs::write(&path, "[localApi]\nport = \"oops\"\n").unwrap();
    let (previous, current) = settings.reload().unwrap();
    assert_eq!(previous, current);
    assert_eq!(settings.config().unwrap().local_api.port, 5180);
    let error = settings.files().error.unwrap();
    assert!(error.starts_with("config.toml:2:8: "), "{error}");
    let refused = settings
        .update_config(|config| {
            config.appearance = Appearance::Dark;
            Ok(())
        })
        .unwrap_err();
    assert!(refused.contains("Fix config.toml"), "{refused}");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "[localApi]\nport = \"oops\"\n"
    );
    assert!(settings.reload().is_none(), "nothing new to apply");

    fs::write(&path, "[localApi]\nport = 5181\n").unwrap();
    let (_, current) = settings.reload().unwrap();
    assert_eq!(current.config.local_api.port, 5181);
    assert_eq!(settings.files().error, None);
}

#[test]
fn a_concurrent_edit_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());
    let path = dir.path().join("config").join(CONFIG_FILE);
    // The user saves between our read and our write.
    let current = fs::read_to_string(&path).unwrap();
    fs::write(&path, "appearance = \"dark\"\n").unwrap();
    let error = private_fs::write_atomic(&path, "", Some(Some(&current))).unwrap_err();
    assert!(private_fs::ChangedOnDisk::is(&error));
    // An edit made before our read is kept, and the watcher applies it later.
    settings
        .update_config(|config| {
            config.connect_on_launch = true;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "appearance = \"dark\"\nconnectOnLaunch = true\n"
    );
    let (_, current) = settings.reload().unwrap();
    assert_eq!(current.config.appearance, Appearance::Dark);
}

#[test]
fn credentials_stay_owner_only_and_errors_never_quote_values() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());
    settings
        .update_credentials(|credentials| {
            credentials.profiles.insert(
                "work".into(),
                ProfileCredential {
                    api_key: "sk-first".into(),
                },
            );
            Ok(())
        })
        .unwrap();
    let path = dir.path().join("config").join(CREDENTIALS_FILE);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with(CREDENTIALS_HEADER));
    assert!(
        text.ends_with("[profiles.work]\napiKey = \"sk-first\"\n"),
        "{text}"
    );
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    settings
        .update_credentials(|credentials| {
            credentials.web_ui.password_hash =
                Some(crate::web_ui::password::hash("correct horse battery").unwrap());
            Ok(())
        })
        .unwrap();
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    fs::write(&path, "[profiles]\nwork = \"sk-secret-value\"\n").unwrap();
    settings.reload().unwrap();
    let error = settings.files().error.unwrap();
    assert_eq!(error, "credentials.toml:2:8: invalid entry");
    assert_eq!(
        settings.profile_key("work").unwrap().as_deref(),
        Some("sk-first")
    );
    fs::write(&path, "[profiles.work]\napiKey = \"sk two\"\n").unwrap();
    settings.reload();
    let error = settings.files().error.unwrap();
    assert!(!error.contains("sk two"), "{error}");
    assert!(error.contains("profiles.work.apiKey"), "{error}");
}

#[test]
fn credential_identities_change_with_the_key_and_hide_it() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());
    let mut snapshot = Snapshot::default();
    snapshot.config.profiles.insert(
        "work".into(),
        config::Profile {
            name: "Work".into(),
            provider: ServiceProvider::Custom,
            remote_url: "https://gateway.example".into(),
            auth: Default::default(),
            verified_at: None,
        },
    );
    assert_eq!(settings.profile_views(&snapshot)[0].credential_ref, None);
    let with_key = |key: &str| {
        let mut snapshot = snapshot.clone();
        snapshot.credentials.profiles.insert(
            "work".into(),
            ProfileCredential {
                api_key: key.into(),
            },
        );
        settings.profile_views(&snapshot).remove(0)
    };
    let first = with_key("sk-first");
    assert!(first.credential_saved);
    let identity = first.credential_ref.unwrap();
    assert!(!identity.contains("sk-first"));
    assert_eq!(with_key("sk-first").credential_ref.unwrap(), identity);
    assert_ne!(with_key("sk-second").credential_ref.unwrap(), identity);
}
