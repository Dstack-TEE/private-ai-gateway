//! Unix domain socket endpoints: a private per-user directory, a `0600`
//! socket, and the peer's UID checked on both ends (`SO_PEERCRED` or
//! `getpeereid`, through tokio's `peer_cred`), as Tailscale's `safesocket`
//! authenticates its LocalAPI peers.

use std::{
    env, fs, io,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(any(test, all(target_os = "macos", feature = "mac-app-store")))]
use std::ffi::OsStr;

use crate::{lock::InstanceLock, private_fs};
use socket2::{Domain, SockAddr, Socket, Type};
use tokio::net::{UnixListener, UnixStream};

use super::endpoint_hash;

pub(super) const SOCKET_FILE: &str = "api.sock";
/// The endpoint of 0.1.4 to 0.2 beta backends; see `client::legacy`.
pub(super) const LEGACY_SOCKET_FILE: &str = "backend.sock";
#[cfg(any(test, all(target_os = "macos", feature = "mac-app-store")))]
const MAS_RUNTIME_DIR: &str = "pap-ipc";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub type Stream = UnixStream;
pub type ClientStream = UnixStream;

#[derive(Debug)]
pub struct Listener {
    inner: UnixListener,
    endpoint: PathBuf,
    socket_identity: FileIdentity,
}

impl Listener {
    /// Binds the per-user endpoint; needs a runtime context.
    ///
    /// The lock capability proves the caller established backend ownership
    /// before this method is allowed to remove a stale socket.
    pub fn bind(owner: &InstanceLock) -> io::Result<Self> {
        Self::bind_at(owner, super::endpoint_path()?)
    }

    /// The next connection; one from another user fails with `PermissionDenied`.
    pub async fn accept(&self) -> io::Result<Stream> {
        let (stream, _) = self.inner.accept().await?;
        authenticate_peer(&stream)?;
        Ok(stream)
    }

    pub fn endpoint_path(&self) -> &Path {
        &self.endpoint
    }

    fn bind_at(_owner: &InstanceLock, endpoint: PathBuf) -> io::Result<Self> {
        let dir = endpoint.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "IPC endpoint has no parent")
        })?;
        ensure_private_dir(dir).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "cannot prepare IPC runtime directory {}: {error}",
                    dir.display()
                ),
            )
        })?;
        remove_stale_socket(&endpoint).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot clean IPC endpoint {}: {error}", endpoint.display()),
            )
        })?;

        let inner = UnixListener::bind(&endpoint).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot bind IPC endpoint {}: {error}", endpoint.display()),
            )
        })?;
        if let Err(error) = fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)) {
            let _ = fs::remove_file(&endpoint);
            return Err(io::Error::new(
                error.kind(),
                format!("cannot secure IPC endpoint {}: {error}", endpoint.display()),
            ));
        }
        let socket_identity = FileIdentity::read(&endpoint).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "cannot inspect IPC endpoint {}: {error}",
                    endpoint.display()
                ),
            )
        })?;
        Ok(Self {
            inner,
            endpoint,
            socket_identity,
        })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if FileIdentity::read(&self.endpoint).ok() == Some(self.socket_identity) {
            let _ = fs::remove_file(&self.endpoint);
            if let Some(parent) = self.endpoint.parent() {
                // Only remove an empty directory; never recursively remove runtime state.
                let _ = fs::remove_dir(parent);
            }
        }
    }
}

/// Connects to the endpoint of a backend running as this user.
pub async fn connect(endpoint: &Path) -> io::Result<ClientStream> {
    let dir = endpoint.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "IPC endpoint has no parent")
    })?;
    validate_private_dir(dir)?;
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(endpoint))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    authenticate_peer(&stream)?;
    Ok(stream)
}

pub(super) fn endpoint_path(data_dir: &Path, file: &str) -> io::Result<PathBuf> {
    let hash = endpoint_hash(data_dir.as_os_str().as_bytes());
    if env::var_os(crate::paths::HOME_OVERRIDE_ENV).is_some()
        || (env::var_os(crate::paths::APP_DATA_OVERRIDE_ENV).is_some()
            && !cfg!(all(target_os = "macos", feature = "mac-app-store")))
    {
        let endpoint = data_dir.join("runtime").join(file);
        if socket_path_fits(&endpoint) {
            return Ok(endpoint);
        }
    }

    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    if env::var_os(crate::paths::APP_DATA_OVERRIDE_ENV).is_some() {
        let base = app_container_runtime_base(data_dir).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the MAS app container runtime directory is unavailable",
            )
        })?;
        let endpoint = base.join(MAS_RUNTIME_DIR).join(file);
        if socket_path_fits(&endpoint) {
            return Ok(endpoint);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the MAS app container runtime path is too long for Unix IPC",
        ));
    }

    #[cfg(target_os = "linux")]
    if let Some(runtime) = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        if is_private_runtime_base(&runtime) {
            let endpoint = runtime_endpoint(&runtime, hash, file);
            if socket_path_fits(&endpoint) {
                return Ok(endpoint);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let runtime = env::temp_dir();
        let endpoint = runtime_endpoint(&runtime, hash, file);
        if is_safe_temporary_base(&runtime) && socket_path_fits(&endpoint) {
            return Ok(endpoint);
        }
    }

    let fallback = if cfg!(target_os = "macos") {
        Path::new("/private/tmp")
    } else {
        Path::new("/tmp")
    };
    let endpoint = runtime_endpoint(fallback, hash, file);
    if is_safe_temporary_base(fallback) && socket_path_fits(&endpoint) {
        return Ok(endpoint);
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "no safe per-user runtime location is available for local IPC",
    ))
}

#[cfg(any(test, all(target_os = "macos", feature = "mac-app-store")))]
fn app_container_runtime_base(data_dir: &Path) -> Option<PathBuf> {
    let application_support = data_dir.parent()?;
    if application_support.file_name()? != OsStr::new("Application Support") {
        return None;
    }
    let library = application_support.parent()?;
    if library.file_name()? != OsStr::new("Library") {
        return None;
    }
    let container_data = library.parent()?;
    (container_data.file_name()? == OsStr::new("Data")).then(|| container_data.to_path_buf())
}

fn runtime_endpoint(base: &Path, hash: u64, file: &str) -> PathBuf {
    base.join(format!("pap-{hash:016x}")).join(file)
}

fn socket_path_fits(path: &Path) -> bool {
    SockAddr::unix(path).is_ok()
}

fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    private_fs::create_private_dir(dir)?;
    validate_private_dir(dir)
}

fn validate_private_dir(dir: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(dir)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "IPC runtime path is not a directory",
        ));
    }
    if metadata.uid() != effective_uid() || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "IPC runtime directory is not private to the current user",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn is_private_runtime_base(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        !metadata.file_type().is_symlink()
            && metadata.is_dir()
            && metadata.uid() == effective_uid()
            && metadata.mode() & 0o077 == 0
    })
}

fn is_safe_temporary_base(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        if !path.is_absolute() || metadata.file_type().is_symlink() || !metadata.is_dir() {
            return false;
        }
        let mode = metadata.mode();
        (metadata.uid() == effective_uid() && mode & 0o077 == 0) || mode & 0o1000 != 0
    })
}

fn remove_stale_socket(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "refusing to replace a non-socket IPC endpoint",
        ));
    }

    if socket_is_live(path)? {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "the IPC endpoint is already accepting connections",
        ));
    }

    let before = FileIdentity::from_metadata(&metadata);
    if FileIdentity::read(path)? != before {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "the IPC endpoint changed during stale-socket cleanup",
        ));
    }
    fs::remove_file(path)
}

fn socket_is_live(path: &Path) -> io::Result<bool> {
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    socket.set_nonblocking(true)?;
    match socket.connect(&SockAddr::unix(path)?) {
        Ok(()) => Ok(true),
        Err(error) => match error.raw_os_error() {
            Some(libc::ECONNREFUSED) | Some(libc::ENOENT) => Ok(false),
            Some(libc::EINPROGRESS) | Some(libc::EAGAIN) => Ok(true),
            _ => Err(error),
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn read(path: &Path) -> io::Result<Self> {
        fs::symlink_metadata(path).map(|metadata| Self::from_metadata(&metadata))
    }

    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

fn authenticate_peer(stream: &UnixStream) -> io::Result<()> {
    let peer = stream.peer_cred()?.uid();
    let current = effective_uid();
    if peer != current {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("IPC peer belongs to UID {peer}, expected UID {current}"),
        ));
    }
    Ok(())
}

fn effective_uid() -> libc::uid_t {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn authenticated_stream_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let owner = crate::lock::instance(temp.path()).unwrap().unwrap();
        let endpoint = temp.path().join("ipc").join(SOCKET_FILE);
        let listener = Listener::bind_at(&owner, endpoint.clone()).unwrap();
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

    #[tokio::test]
    async fn bind_replaces_only_a_stale_socket() {
        let temp = tempfile::tempdir().unwrap();
        let owner = crate::lock::instance(temp.path()).unwrap().unwrap();
        let endpoint = temp.path().join("ipc").join(SOCKET_FILE);
        fs::create_dir_all(endpoint.parent().unwrap()).unwrap();
        fs::set_permissions(
            endpoint.parent().unwrap(),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let stale = std::os::unix::net::UnixListener::bind(&endpoint).unwrap();
        drop(stale);

        let listener = Listener::bind_at(&owner, endpoint.clone()).unwrap();
        assert_eq!(listener.endpoint_path(), endpoint);
        assert_eq!(
            Listener::bind_at(&owner, endpoint).unwrap_err().kind(),
            io::ErrorKind::AddrInUse
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn full_accept_queue_cannot_block_a_client_indefinitely() {
        let temp = tempfile::tempdir().unwrap();
        let endpoint = temp.path().join(SOCKET_FILE);
        let address = SockAddr::unix(&endpoint).unwrap();
        let listener = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
        listener.bind(&address).unwrap();
        listener.listen(1).unwrap();
        let mut queued = Vec::new();
        for _ in 0..16 {
            let socket = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
            socket.set_nonblocking(true).unwrap();
            match socket.connect(&address) {
                Ok(()) => queued.push(socket),
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
                    let start = Instant::now();
                    assert!(connect(&endpoint).await.is_err());
                    assert!(start.elapsed() < Duration::from_secs(6));
                    return;
                }
            }
        }
        panic!("The bounded test listener did not fill its accept queue");
    }

    #[tokio::test]
    async fn bind_refuses_an_insecure_runtime_directory() {
        let temp = tempfile::tempdir().unwrap();
        let owner = crate::lock::instance(temp.path()).unwrap().unwrap();
        let runtime = temp.path().join("ipc");
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
        let endpoint = runtime.join(SOCKET_FILE);
        let raw_listener = std::os::unix::net::UnixListener::bind(&endpoint).unwrap();

        let error = Listener::bind_at(&owner, endpoint.clone()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let error = connect(&endpoint).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        drop(raw_listener);
    }

    #[test]
    fn app_container_runtime_base_shortens_mas_application_support_path() {
        let data_dir = Path::new(
            "/Users/test/Library/Containers/org.dstack.private-ai-proxy/Data/Library/Application Support/org.dstack.private-ai-proxy",
        );
        let base = app_container_runtime_base(data_dir).unwrap();
        let endpoint = base.join(MAS_RUNTIME_DIR).join(SOCKET_FILE);
        assert_eq!(
            base,
            Path::new("/Users/test/Library/Containers/org.dstack.private-ai-proxy/Data")
        );
        assert!(socket_path_fits(&endpoint));
    }

    #[test]
    fn app_container_runtime_base_rejects_non_container_paths() {
        assert!(app_container_runtime_base(Path::new("/tmp/app-data")).is_none());
    }
}
