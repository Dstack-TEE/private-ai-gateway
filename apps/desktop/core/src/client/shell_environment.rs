//! Capture the login shell once for desktop launches outside a terminal.

use std::{
    env,
    ffi::{CStr, OsString},
    io::{self, IsTerminal},
    mem::MaybeUninit,
    os::unix::ffi::OsStringExt,
    path::PathBuf,
    process::Stdio,
    time::Duration,
};

use tokio::process::Command;

use crate::paths::agent_environment::{capture, ClientEnvironment};

pub(super) fn resolve() -> ClientEnvironment {
    if !io::stdin().is_terminal() && !io::stdout().is_terminal() && !io::stderr().is_terminal() {
        let shell = env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|shell| shell.is_absolute())
            .or_else(account_shell);
        if let Some(shell) = shell {
            match super::block_on(read(Command::new(shell), Duration::from_secs(10))) {
                Ok(Ok(values)) => return values,
                Ok(Err(error)) => {
                    tracing::warn!("Cannot read the login shell environment: {}", error.kind())
                }
                Err(error) => tracing::warn!("Cannot read the login shell environment: {error}"),
            }
        }
    }
    capture(env::var_os)
}

/// Graphical sessions can omit SHELL. Read the account's login shell.
fn account_shell() -> Option<PathBuf> {
    // SAFETY: sysconf has no pointer arguments.
    let size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let mut buffer = vec![0_u8; size.max(16_384) as usize];
    let mut user = MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    // SAFETY: getpwuid_r writes to the provided record and buffer.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            user.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return None;
    }
    // SAFETY: A successful lookup initialized the record. Its strings
    // live in buffer, which stays alive until the path is copied.
    let shell = unsafe { user.assume_init().pw_shell };
    if shell.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r returned a non-null, NUL-terminated shell.
    let bytes = unsafe { CStr::from_ptr(shell) }.to_bytes();
    let shell = PathBuf::from(OsString::from_vec(bytes.to_vec()));
    shell.is_absolute().then_some(shell)
}

async fn read(mut shell: Command, timeout: Duration) -> io::Result<ClientEnvironment> {
    // Delimit the NUL-separated environment so profile output is harmless.
    let marker = uuid::Uuid::new_v4().simple().to_string();
    let print_marker = format!("printf '\\000{marker}\\000'");
    let command = format!("{print_marker}; env -0 && {print_marker}");
    let child = shell
        .args(["-ilc", &command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    let group = child.id();
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(output) => output?,
        Err(_) => {
            if let Some(group) = group {
                // SAFETY: the shell was started as leader of its own group.
                // Kill its startup children too if one inherited stdout.
                unsafe { libc::kill(-(group as libc::pid_t), libc::SIGKILL) };
            }
            return Err(io::ErrorKind::TimedOut.into());
        }
    };

    let marker = format!("\0{marker}\0");
    let marker = marker.as_bytes();
    let find_marker = |bytes: &[u8]| {
        bytes
            .windows(marker.len())
            .position(|window| window == marker)
    };
    let start = find_marker(&output.stdout).ok_or(io::ErrorKind::InvalidData)? + marker.len();
    let bytes = &output.stdout[start..];
    let end = find_marker(bytes).ok_or(io::ErrorKind::InvalidData)?;
    let mut values = bytes[..end]
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let key = entry.iter().position(|byte| *byte == b'=')?;
            Some((std::str::from_utf8(&entry[..key]).ok()?, &entry[key + 1..]))
        })
        .collect::<std::collections::HashMap<_, _>>();
    Ok(capture(|name| {
        values
            .remove(name)
            .map(|value| OsString::from_vec(value.to_vec()))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_script(script: &str) -> Command {
        // A concurrent fork can inherit a freshly written script's writable
        // descriptor and make executing the script fail with ETXTBSY.
        let mut shell = Command::new("/bin/sh");
        shell.args(["-c", script, "test-shell"]);
        shell
    }

    #[tokio::test]
    async fn resolves_multiline_environment_amid_shell_output() {
        let shell = shell_script(
            r#"printf 'profile output\n'
export CODEX_HOME='first
second=third'
export OPENCODE_CONFIG_CONTENT=private
eval "$2"
printf 'more output\n'"#,
        );
        let values = read(shell, Duration::from_secs(2)).await.unwrap();
        assert_eq!(values["CODEX_HOME"], "first\nsecond=third");
        assert!(values["OPENCODE_CONFIG_CONTENT"].is_empty());
    }

    #[tokio::test]
    async fn gives_up_when_shell_startup_hangs() {
        let shell = shell_script("sleep 30");
        let error = read(shell, Duration::from_millis(100)).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
