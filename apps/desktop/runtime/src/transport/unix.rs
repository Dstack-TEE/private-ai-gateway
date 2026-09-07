use std::{
    env, fs, io,
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};

use desktop_gateway::{lock::InstanceLock, tokens};
use socket2::{Domain, SockAddr, Socket, Type};

use super::endpoint_hash;

const SOCKET_FILE: &str = "backend.sock";

#[derive(Debug)]
pub struct Listener {
    inner: UnixListener,
    endpoint: PathBuf,
    socket_identity: FileIdentity,
}

impl Listener {
    /// Bind the authenticated per-user endpoint.
    ///
    /// The lock capability proves the caller established backend ownership
    /// before this method is allowed to remove a stale socket.
    pub fn bind(owner: &InstanceLock) -> io::Result<Self> {
        Self::bind_at(owner, super::endpoint_path()?)
    }

    pub fn accept(&self) -> io::Result<Stream> {
        let (stream, _) = self.inner.accept()?;
        // BSD sockets may inherit O_NONBLOCK from the listener; frame I/O uses deadlines.
        stream.set_nonblocking(false)?;
        authenticate_peer(&stream)?;
        Ok(Stream { inner: stream })
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.inner.set_nonblocking(nonblocking)
    }

    pub fn endpoint_path(&self) -> &Path {
        &self.endpoint
    }

    fn bind_at(_owner: &InstanceLock, endpoint: PathBuf) -> io::Result<Self> {
        let dir = endpoint.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "IPC endpoint has no parent")
        })?;
        ensure_private_dir(dir)?;
        remove_stale_socket(&endpoint)?;

        let inner = UnixListener::bind(&endpoint)?;
        if let Err(error) = fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)) {
            let _ = fs::remove_file(&endpoint);
            return Err(error);
        }
        let socket_identity = FileIdentity::read(&endpoint)?;
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

#[derive(Debug)]
pub struct Stream {
    inner: UnixStream,
}

impl Stream {
    pub fn connect() -> io::Result<Self> {
        Self::connect_at(super::endpoint_path()?)
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.set_read_timeout(timeout)
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.set_write_timeout(timeout)
    }

    fn connect_at(endpoint: PathBuf) -> io::Result<Self> {
        let dir = endpoint.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "IPC endpoint has no parent")
        })?;
        validate_private_dir(dir)?;
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        socket.connect_timeout(&SockAddr::unix(endpoint)?, Duration::from_secs(5))?;
        let stream = UnixStream::from(socket);
        authenticate_peer(&stream)?;
        Ok(Self { inner: stream })
    }
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub(super) fn endpoint_path(data_dir: &Path) -> io::Result<PathBuf> {
    let hash = endpoint_hash(data_dir.as_os_str().as_bytes());
    if env::var_os(desktop_gateway::agents::HOME_OVERRIDE_ENV).is_some() {
        let endpoint = data_dir.join("runtime").join(SOCKET_FILE);
        if socket_path_fits(&endpoint) {
            return Ok(endpoint);
        }
    }

    #[cfg(target_os = "linux")]
    if let Some(runtime) = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        if is_private_runtime_base(&runtime) {
            let endpoint = runtime_endpoint(&runtime, hash);
            if socket_path_fits(&endpoint) {
                return Ok(endpoint);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let runtime = env::temp_dir();
        let endpoint = runtime_endpoint(&runtime, hash);
        if is_safe_temporary_base(&runtime) && socket_path_fits(&endpoint) {
            return Ok(endpoint);
        }
    }

    let fallback = if cfg!(target_os = "macos") {
        Path::new("/private/tmp")
    } else {
        Path::new("/tmp")
    };
    let endpoint = runtime_endpoint(fallback, hash);
    if is_safe_temporary_base(fallback) && socket_path_fits(&endpoint) {
        return Ok(endpoint);
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "no safe per-user runtime location is available for local IPC",
    ))
}

fn runtime_endpoint(base: &Path, hash: u64) -> PathBuf {
    base.join(format!("pag-{hash:016x}")).join(SOCKET_FILE)
}

fn socket_path_fits(path: &Path) -> bool {
    SockAddr::unix(path).is_ok()
}

fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    tokens::create_private_dir(dir)?;
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
    let peer = peer_uid(stream)?;
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

#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(stream: &UnixStream) -> io::Result<libc::uid_t> {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: credentials and length point to writable storage of the stated
    // size, and the stream owns a valid socket descriptor for this call.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "kernel returned malformed IPC peer credentials",
        ));
    }
    // SAFETY: getsockopt succeeded and reported the complete ucred size.
    Ok(unsafe { credentials.assume_init() }.uid)
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn peer_uid(stream: &UnixStream) -> io::Result<libc::uid_t> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: uid and gid are valid writable pointers and the stream owns a
    // connected Unix-domain socket descriptor.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    fn accept_with_deadline(listener: &Listener) -> Stream {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match listener.accept() {
                Ok(stream) => return stream,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        }
    }

    #[test]
    fn authenticated_stream_round_trip_and_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let owner = desktop_gateway::lock::instance(temp.path())
            .unwrap()
            .unwrap();
        let endpoint = temp.path().join("ipc").join(SOCKET_FILE);
        let listener = Listener::bind_at(&owner, endpoint.clone()).unwrap();
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        let (tx, rx) = mpsc::channel();
        let client = thread::spawn(move || {
            let mut stream = Stream::connect_at(endpoint).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            let mut byte = [0];
            tx.send(stream.read(&mut byte).unwrap_err().kind()).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            stream.read_exact(&mut byte).unwrap();
            stream.write_all(&[byte[0] + 1]).unwrap();
        });

        let mut server = accept_with_deadline(&listener);
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
        server.write_all(&[41]).unwrap();
        let mut response = [0];
        server.read_exact(&mut response).unwrap();
        assert_eq!(response, [42]);
        client.join().unwrap();
    }

    #[test]
    fn bind_replaces_only_a_stale_socket() {
        let temp = tempfile::tempdir().unwrap();
        let owner = desktop_gateway::lock::instance(temp.path())
            .unwrap()
            .unwrap();
        let endpoint = temp.path().join("ipc").join(SOCKET_FILE);
        fs::create_dir_all(endpoint.parent().unwrap()).unwrap();
        fs::set_permissions(
            endpoint.parent().unwrap(),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let stale = UnixListener::bind(&endpoint).unwrap();
        drop(stale);

        let listener = Listener::bind_at(&owner, endpoint.clone()).unwrap();
        assert_eq!(listener.endpoint_path(), endpoint);
        assert_eq!(
            Listener::bind_at(&owner, endpoint).unwrap_err().kind(),
            io::ErrorKind::AddrInUse
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn full_accept_queue_cannot_block_a_client_indefinitely() {
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
                    assert!(Stream::connect_at(endpoint).is_err());
                    assert!(start.elapsed() < Duration::from_secs(6));
                    return;
                }
            }
        }
        panic!("The bounded test listener did not fill its accept queue");
    }

    #[test]
    fn bind_refuses_an_insecure_runtime_directory() {
        let temp = tempfile::tempdir().unwrap();
        let owner = desktop_gateway::lock::instance(temp.path())
            .unwrap()
            .unwrap();
        let runtime = temp.path().join("ipc");
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
        let endpoint = runtime.join(SOCKET_FILE);
        let raw_listener = UnixListener::bind(&endpoint).unwrap();

        let error = Listener::bind_at(&owner, endpoint.clone()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let error = Stream::connect_at(endpoint).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        drop(raw_listener);
    }
}
