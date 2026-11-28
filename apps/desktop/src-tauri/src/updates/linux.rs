use std::{
    io::{self, Write},
    path::Path,
    process::{Command, ExitStatus, Stdio},
};

use desktop_core::updates::prepare_rpm_signing_key;
use tauri::utils::{config::BundleType, platform::bundle_type};

const IMPORT_ARGS: [&str; 2] = ["rpm", "--import"];

pub(super) fn install(install_package: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    // Match Tauri's installer selection. The plugin owns its privileged package
    // installation, so importing a key can require a separate authorization.
    install_for_bundle(
        bundle_type(),
        || {
            let scratch = tempfile::tempdir().map_err(|_| "Could not prepare the RPM update.")?;
            if let Some(key) = prepare_rpm_signing_key(scratch.path())? {
                import_rpm_key(&Command::new, &key)?;
            }
            Ok(())
        },
        install_package,
    )
}

fn install_for_bundle(
    bundle: Option<BundleType>,
    prepare_rpm: impl FnOnce() -> Result<(), String>,
    install_package: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if matches!(bundle, Some(BundleType::Rpm)) {
        prepare_rpm()?;
    }
    install_package()
}

fn import_rpm_key(command: &impl Fn(&'static str) -> Command, key: &Path) -> Result<(), String> {
    if command("pkexec")
        .args(IMPORT_ARGS)
        .arg(key)
        .status()
        .is_ok_and(|status| status.success())
    {
        return Ok(());
    }

    let dialogs: [(&str, &[&str]); 2] = [
        (
            "zenity",
            &[
                "--password",
                "--title=Import the Private AI Proxy RPM signing key",
            ],
        ),
        (
            "kdialog",
            &[
                "--password",
                "Enter your password to import the Private AI Proxy RPM signing key",
            ],
        ),
    ];
    let password = dialogs.into_iter().find_map(|(program, args)| {
        command(program)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| output.stdout)
    });
    if password.is_some_and(|password| {
        sudo_with_password(command("sudo"), key, password).is_ok_and(|status| status.success())
    }) {
        return Ok(());
    }

    if command("sudo")
        .args(IMPORT_ARGS)
        .arg(key)
        .status()
        .is_ok_and(|status| status.success())
    {
        Ok(())
    } else {
        Err("Could not import the Private AI Proxy RPM signing key.".into())
    }
}

fn sudo_with_password(
    mut sudo: Command,
    key: &Path,
    mut password: Vec<u8>,
) -> io::Result<ExitStatus> {
    if !password.ends_with(b"\n") {
        password.push(b'\n');
    }
    let mut child = sudo
        .arg("-S")
        .args(IMPORT_ARGS)
        .arg(key)
        .stdin(Stdio::piped())
        .spawn()?;
    let sent = child
        .stdin
        .take()
        .map(|mut stdin| stdin.write_all(&password));
    let status = child.wait()?;
    if !status.success() {
        sent.transpose()?;
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const KEY: &str = "/test/linux-signing-key.asc";

    fn recorder(
        log: &Path,
        pkexec_status: &'static str,
        sudo_status: &'static str,
        dialog_password: &'static str,
    ) -> impl Fn(&'static str) -> Command {
        let log = log.to_owned();
        move |program| {
            let mut command = Command::new("sh");
            command
                .args([
                    "-c",
                    r#"
                printf '%s\n' "$0 $*" >> "$UPDATE_TEST_LOG"
                case "$0" in
                    pkexec) exit "$UPDATE_TEST_PKEXEC_STATUS" ;;
                    kdialog) [ -n "$UPDATE_TEST_PASSWORD" ] && printf '%s' "$UPDATE_TEST_PASSWORD" ;;
                    sudo)
                        if [ "$1" = "-S" ]; then
                            IFS= read -r password && [ "$password" = "$UPDATE_TEST_PASSWORD" ] || exit 1
                        fi
                        exit "$UPDATE_TEST_SUDO_STATUS"
                        ;;
                    *) exit 1 ;;
                esac
            "#,
                    program,
                ])
                .env("UPDATE_TEST_LOG", &log)
                .env("UPDATE_TEST_PKEXEC_STATUS", pkexec_status)
                .env("UPDATE_TEST_SUDO_STATUS", sudo_status)
                .env("UPDATE_TEST_PASSWORD", dialog_password);
            command
        }
    }

    #[test]
    fn imports_the_rpm_key_before_installing() {
        let scratch = tempfile::tempdir().unwrap();
        let log = scratch.path().join("commands");
        let import = || import_rpm_key(&recorder(&log, "0", "1", ""), Path::new(KEY));
        install_for_bundle(Some(BundleType::Rpm), import, || {
            assert_eq!(
                fs::read_to_string(&log).unwrap(),
                format!("pkexec rpm --import {KEY}\n")
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn installs_deb_without_an_rpm_key_import() {
        let scratch = tempfile::tempdir().unwrap();
        let log = scratch.path().join("commands");
        let import = || import_rpm_key(&recorder(&log, "1", "1", ""), Path::new(KEY));
        install_for_bundle(Some(BundleType::Deb), import, || {
            assert!(!log.exists());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn does_not_install_when_rpm_key_import_fails() {
        let scratch = tempfile::tempdir().unwrap();
        let log = scratch.path().join("commands");
        let import = || import_rpm_key(&recorder(&log, "1", "1", ""), Path::new(KEY));
        let error = install_for_bundle(Some(BundleType::Rpm), import, || {
            panic!("installation must wait for a successful import")
        })
        .unwrap_err();
        assert_eq!(
            error,
            "Could not import the Private AI Proxy RPM signing key."
        );
        let commands = fs::read_to_string(&log).unwrap();
        assert!(commands.ends_with(&format!("sudo rpm --import {KEY}\n")));
    }

    #[test]
    fn can_import_with_graphical_sudo_when_pkexec_fails() {
        let scratch = tempfile::tempdir().unwrap();
        let log = scratch.path().join("commands");
        let import = || import_rpm_key(&recorder(&log, "1", "0", "test password"), Path::new(KEY));
        install_for_bundle(Some(BundleType::Rpm), import, || {
            let commands = fs::read_to_string(&log).unwrap();
            assert!(commands.ends_with(&format!("sudo -S rpm --import {KEY}\n")));
            Ok(())
        })
        .unwrap();
    }
}
