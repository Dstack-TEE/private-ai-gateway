//! Windows named pipes with a protected current-user DACL, remote clients
//! rejected and the first instance claimed (`FILE_FLAG_FIRST_PIPE_INSTANCE`),
//! as Tailscale's `safesocket` and the tokio named-pipe server loop do. Both
//! ends check that the peer process runs as the current user.

use std::{
    ffi::{c_void, OsStr},
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    ptr,
    time::{Duration, Instant},
};

use crate::{brand::APP_IDENTIFIER, lock::InstanceLock};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::{
    Foundation::{
        GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_PIPE_BUSY, FALSE, HANDLE,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        CopySid, EqualSid, GetLengthSid, GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR,
        PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    },
    System::{
        Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

use super::endpoint_hash;

pub(super) const SOCKET_FILE: &str = "api";
/// The endpoint of 0.1.4 to 0.2 beta backends; see `client::legacy`.
pub(super) const LEGACY_SOCKET_FILE: &str = "";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(20);

pub type Stream = NamedPipeServer;
pub type ClientStream = NamedPipeClient;

#[derive(Debug)]
pub struct Listener {
    endpoint: PathBuf,
    /// Always one instance waits, so the name can never be claimed by another process.
    next: NamedPipeServer,
}

impl Listener {
    /// Claims the first pipe instance; needs a runtime context.
    ///
    /// The lock capability ensures endpoint ownership was established before
    /// the first-instance claim is attempted.
    pub fn bind(owner: &InstanceLock) -> io::Result<Self> {
        Self::bind_at(owner, super::endpoint_path()?)
    }

    /// The next connection; one from another user fails with `PermissionDenied`.
    pub async fn accept(&mut self) -> io::Result<Stream> {
        if let Err(error) = self.next.connect().await {
            // A failed instance is not reused.
            self.next = instance(&self.endpoint, false)?;
            return Err(error);
        }
        let connected = std::mem::replace(&mut self.next, instance(&self.endpoint, false)?);
        let mut process_id = 0;
        // SAFETY: the handle is a connected pipe instance owned by `connected`.
        if unsafe { GetNamedPipeClientProcessId(connected.as_raw_handle().cast(), &mut process_id) }
            == FALSE
        {
            return Err(io::Error::last_os_error());
        }
        authenticate_process(process_id)?;
        Ok(connected)
    }

    pub fn endpoint_path(&self) -> &Path {
        &self.endpoint
    }

    fn bind_at(_owner: &InstanceLock, endpoint: PathBuf) -> io::Result<Self> {
        let next = instance(&endpoint, true)?;
        Ok(Self { endpoint, next })
    }
}

fn instance(endpoint: &Path, first: bool) -> io::Result<NamedPipeServer> {
    let security = SecurityDescriptor::current_user()?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.as_ptr(),
        bInheritHandle: FALSE,
    };
    // SAFETY: `attributes` and the descriptor it points to outlive the call.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                endpoint,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
            )
    }
}

/// Connects to the endpoint of a backend running as this user, waiting out
/// `ERROR_PIPE_BUSY` as the tokio client loop does.
pub async fn connect(endpoint: &Path) -> io::Result<ClientStream> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let client = loop {
        match ClientOptions::new().open(endpoint) {
            Ok(client) => break client,
            Err(error)
                if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                    && Instant::now() < deadline => {}
            Err(error) => return Err(error),
        }
        tokio::time::sleep(CONNECT_RETRY_DELAY).await;
    };
    let mut process_id = 0;
    // SAFETY: the handle is an open pipe client owned by `client`.
    if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle().cast(), &mut process_id) }
        == FALSE
    {
        return Err(io::Error::last_os_error());
    }
    authenticate_process(process_id)?;
    Ok(client)
}

pub(super) fn endpoint_path(data_dir: &Path, file: &str) -> io::Result<PathBuf> {
    let mut bytes = Vec::new();
    for unit in data_dir.as_os_str().encode_wide() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let hash = endpoint_hash(&bytes);
    let suffix = if file.is_empty() {
        String::new()
    } else {
        format!("-{file}")
    };
    Ok(PathBuf::from(format!(
        r"\\.\pipe\{APP_IDENTIFIER}-{hash:016x}{suffix}"
    )))
}

fn authenticate_process(process_id: u32) -> io::Result<()> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, process_id) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcess returned a unique owned kernel handle.
    let process = unsafe { OwnedHandle::from_raw_handle(process.cast()) };
    let token = open_process_token(process.as_raw_handle().cast())?;
    let server = Sid::from_token(token.as_raw_handle().cast())?;
    let current = current_user()?;
    require_same_user(&server, &current)
}

fn require_same_user(peer: &Sid, current: &Sid) -> io::Result<()> {
    if unsafe { EqualSid(peer.as_ptr(), current.as_ptr()) } == FALSE {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "local IPC peer is not owned by the current Windows user",
        ));
    }
    Ok(())
}

fn open_process_token(process: HANDLE) -> io::Result<OwnedHandle> {
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken returned a unique owned kernel handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(token.cast()) })
}

/// The current Windows user's SID in string form (`S-1-5-…`).
pub fn current_user_sid() -> io::Result<String> {
    current_user()?.string()
}

fn current_user() -> io::Result<Sid> {
    let token = open_process_token(unsafe { GetCurrentProcess() })?;
    Sid::from_token(token.as_raw_handle().cast())
}

struct Sid {
    storage: Vec<usize>,
}

impl Sid {
    fn from_token(token: HANDLE) -> io::Result<Self> {
        let mut required = 0;
        unsafe {
            GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut required);
        }
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER || required == 0 {
            return Err(io::Error::last_os_error());
        }

        let words = (required as usize).div_ceil(std::mem::size_of::<usize>());
        let mut token_info = vec![0usize; words];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                token_info.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        } == FALSE
        {
            return Err(io::Error::last_os_error());
        }
        let source = unsafe { (*(token_info.as_ptr().cast::<TOKEN_USER>())).User.Sid };
        if source.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows returned an invalid user SID",
            ));
        }
        let sid_length = unsafe { GetLengthSid(source) };
        if sid_length == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows returned an invalid user SID",
            ));
        }
        let sid_words = (sid_length as usize).div_ceil(std::mem::size_of::<usize>());
        let mut storage = vec![0usize; sid_words];
        if unsafe { CopySid(sid_length, storage.as_mut_ptr().cast(), source) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { storage })
    }

    fn as_ptr(&self) -> PSID {
        self.storage.as_ptr().cast_mut().cast()
    }

    fn sddl(&self) -> io::Result<String> {
        Ok(format!("D:P(A;;GA;;;{})", self.string()?))
    }

    fn string(&self) -> io::Result<String> {
        let mut string_sid = ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(self.as_ptr(), &mut string_sid) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        let allocation = LocalAllocation(string_sid.cast());
        let mut length = 0;
        while unsafe { *string_sid.add(length) } != 0 {
            length += 1;
        }
        let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(string_sid, length) })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Windows returned an invalid SID",
                )
            })?;
        drop(allocation);
        Ok(sid)
    }
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn current_user() -> io::Result<Self> {
        let sddl = current_user()?.sddl()?;
        let wide = wide_string(OsStr::new(&sddl));
        let mut descriptor = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == FALSE
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0.cast());
        }
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn wide_string(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn test_endpoint() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(format!(
            r"\\.\pipe\pap-transport-test-{}-{sequence}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn authenticated_stream_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let owner = crate::lock::instance(temp.path()).unwrap().unwrap();
        let endpoint = test_endpoint();
        let mut listener = Listener::bind_at(&owner, endpoint.clone()).unwrap();
        // The name is claimed: a second first instance fails.
        assert!(Listener::bind_at(&owner, endpoint.clone()).is_err());
        let client = tokio::spawn(async move {
            let mut stream = connect(&endpoint).await.unwrap();
            let mut byte = [0];
            stream.read_exact(&mut byte).await.unwrap();
            stream.write_all(&[byte[0] + 1]).await.unwrap();
        });
        let mut server = listener.accept().await.unwrap();
        server.write_all(&[41]).await.unwrap();
        let mut response = [0];
        server.read_exact(&mut response).await.unwrap();
        assert_eq!(response, [42]);
        client.await.unwrap();
    }
}
