use std::{
    ffi::{c_void, OsStr},
    io,
    io::{Read, Write},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use desktop_gateway::{agents::APP_IDENTIFIER, lock::InstanceLock};
use windows_sys::Win32::{
    Foundation::{
        GetLastError, LocalFree, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND,
        ERROR_INSUFFICIENT_BUFFER, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_NO_DATA,
        ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, FALSE, HANDLE,
        INVALID_HANDLE_VALUE, TRUE, WAIT_TIMEOUT,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        CopySid, EqualSid, GetLengthSid, GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR,
        PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    },
    System::{
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId,
            GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        },
        Threading::{
            CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
        IO::{CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED},
    },
};

use super::endpoint_hash;

const PIPE_BUFFER_SIZE: u32 = 64 * 1024;
const CONNECT_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(20);
const NO_TIMEOUT: u32 = u32::MAX;

pub struct Listener {
    endpoint: PathBuf,
    state: Mutex<AcceptState>,
    nonblocking: AtomicBool,
}

struct AcceptState {
    pending: Option<PendingPipe>,
}

impl std::fmt::Debug for Listener {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Listener")
            .field("endpoint", &self.endpoint)
            .field("nonblocking", &self.nonblocking.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Listener {
    /// Bind the first named-pipe instance with a current-user-only DACL.
    ///
    /// The lock capability ensures endpoint ownership was established before
    /// the first-instance claim is attempted.
    pub fn bind(owner: &InstanceLock) -> io::Result<Self> {
        Self::bind_at(owner, super::endpoint_path()?)
    }

    pub fn accept(&self) -> io::Result<Stream> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("IPC listener state is poisoned"))?;
        if state.pending.is_none() {
            state.pending = Some(PendingPipe::new(&self.endpoint, false)?);
        }

        let nonblocking = self.nonblocking.load(Ordering::Relaxed);
        state
            .pending
            .as_mut()
            .unwrap()
            .finish_connect(nonblocking)?;
        let pipe = state.pending.take().unwrap();
        // Keep an instance continuously present while authenticating and
        // handing off the accepted handle, so the pipe name can never be
        // reclaimed between accepts.
        state.pending = Some(PendingPipe::new(&self.endpoint, false)?);
        authenticate_client(pipe.handle()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Cannot authenticate the IPC client",
            )
        })?;
        Ok(Stream::new(pipe.into_handle()))
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking.store(nonblocking, Ordering::Relaxed);
        Ok(())
    }

    pub fn endpoint_path(&self) -> &Path {
        &self.endpoint
    }

    fn bind_at(_owner: &InstanceLock, endpoint: PathBuf) -> io::Result<Self> {
        let pending = PendingPipe::new(&endpoint, true)?;
        Ok(Self {
            endpoint,
            state: Mutex::new(AcceptState {
                pending: Some(pending),
            }),
            nonblocking: AtomicBool::new(false),
        })
    }
}

#[derive(Debug)]
pub struct Stream {
    handle: OwnedHandle,
    read_timeout_ms: AtomicU32,
    write_timeout_ms: AtomicU32,
}

impl Stream {
    pub fn connect() -> io::Result<Self> {
        Self::connect_at(super::endpoint_path()?)
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout_ms
            .store(timeout_millis(timeout)?, Ordering::Relaxed);
        Ok(())
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.write_timeout_ms
            .store(timeout_millis(timeout)?, Ordering::Relaxed);
        Ok(())
    }

    fn new(handle: OwnedHandle) -> Self {
        Self {
            handle,
            read_timeout_ms: AtomicU32::new(NO_TIMEOUT),
            write_timeout_ms: AtomicU32::new(NO_TIMEOUT),
        }
    }

    fn connect_at(endpoint: PathBuf) -> io::Result<Self> {
        let wide = wide_path(&endpoint);
        let deadline = Instant::now() + CONNECT_BUSY_TIMEOUT;
        loop {
            let raw = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                    FILE_SHARE_NONE,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    ptr::null_mut(),
                )
            };
            if raw != INVALID_HANDLE_VALUE {
                // SAFETY: CreateFileW returned a unique owned kernel handle.
                let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
                authenticate_server(raw)?;
                return Ok(Self::new(handle));
            }

            let error = unsafe { GetLastError() };
            if error == ERROR_FILE_NOT_FOUND {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            if error != ERROR_PIPE_BUSY || Instant::now() >= deadline {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            thread::sleep(CONNECT_RETRY_DELAY);
        }
    }

    fn raw_handle(&self) -> HANDLE {
        self.handle.as_raw_handle().cast()
    }
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let length = buffer.len().min(u32::MAX as usize) as u32;
        let result = overlapped_io(
            self.raw_handle(),
            self.read_timeout_ms.load(Ordering::Relaxed),
            |overlapped, transferred| unsafe {
                ReadFile(
                    self.raw_handle(),
                    buffer.as_mut_ptr(),
                    length,
                    transferred,
                    overlapped,
                )
            },
        );
        match result {
            Err(error)
                if matches!(
                    error.raw_os_error().map(|code| code as u32),
                    Some(ERROR_BROKEN_PIPE) | Some(ERROR_NO_DATA)
                ) =>
            {
                Ok(0)
            }
            other => other,
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let length = buffer.len().min(u32::MAX as usize) as u32;
        overlapped_io(
            self.raw_handle(),
            self.write_timeout_ms.load(Ordering::Relaxed),
            |overlapped, transferred| unsafe {
                WriteFile(
                    self.raw_handle(),
                    buffer.as_ptr(),
                    length,
                    transferred,
                    overlapped,
                )
            },
        )
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn endpoint_path(data_dir: &Path) -> io::Result<PathBuf> {
    let mut bytes = Vec::new();
    for unit in data_dir.as_os_str().encode_wide() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let hash = endpoint_hash(&bytes);
    Ok(PathBuf::from(format!(
        r"\\.\pipe\{APP_IDENTIFIER}-{hash:016x}"
    )))
}

struct PendingPipe {
    handle: Option<OwnedHandle>,
    _event: OwnedHandle,
    overlapped: Box<OVERLAPPED>,
    pending: bool,
}

// OVERLAPPED carries raw pointers but this value is only accessed while held
// by Listener's mutex. Its event and pipe handles outlive every pending call.
unsafe impl Send for PendingPipe {}

impl PendingPipe {
    fn new(endpoint: &Path, first_instance: bool) -> io::Result<Self> {
        let security = SecurityDescriptor::current_user()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.as_ptr(),
            bInheritHandle: FALSE,
        };
        let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if first_instance {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let wide = wide_path(endpoint);
        let raw = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER_SIZE,
                PIPE_BUFFER_SIZE,
                0,
                &attributes,
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateNamedPipeW returned a unique owned kernel handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        let event_raw = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
        if event_raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateEventW returned a unique owned kernel handle.
        let event = unsafe { OwnedHandle::from_raw_handle(event_raw.cast()) };
        // Windows retains this address until the asynchronous connect completes.
        let mut overlapped = Box::new(OVERLAPPED {
            hEvent: event_raw,
            ..Default::default()
        });

        let connected = unsafe { ConnectNamedPipe(raw, &mut *overlapped) } != FALSE;
        let pending = if connected {
            false
        } else {
            match unsafe { GetLastError() } {
                ERROR_IO_PENDING => true,
                ERROR_PIPE_CONNECTED => false,
                error => return Err(io::Error::from_raw_os_error(error as i32)),
            }
        };
        Ok(Self {
            handle: Some(handle),
            _event: event,
            overlapped,
            pending,
        })
    }

    fn finish_connect(&mut self, nonblocking: bool) -> io::Result<()> {
        if !self.pending {
            return Ok(());
        }
        let mut transferred = 0;
        let timeout = if nonblocking { 0 } else { NO_TIMEOUT };
        let complete = unsafe {
            GetOverlappedResultEx(
                self.handle(),
                &*self.overlapped,
                &mut transferred,
                timeout,
                FALSE,
            )
        };
        if complete != FALSE {
            self.pending = false;
            return Ok(());
        }
        let error = unsafe { GetLastError() };
        if nonblocking && matches!(error, ERROR_IO_INCOMPLETE | WAIT_TIMEOUT) {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        self.pending = false;
        Err(io::Error::from_raw_os_error(error as i32))
    }

    fn handle(&self) -> HANDLE {
        self.handle.as_ref().unwrap().as_raw_handle().cast()
    }

    fn into_handle(mut self) -> OwnedHandle {
        self.pending = false;
        self.handle.take().unwrap()
    }

    fn cancel(&mut self) {
        if !self.pending {
            return;
        }
        let handle = self.handle();
        unsafe {
            CancelIoEx(handle, &*self.overlapped);
            let mut transferred = 0;
            GetOverlappedResult(handle, &*self.overlapped, &mut transferred, TRUE);
        }
        self.pending = false;
    }
}

impl Drop for PendingPipe {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn overlapped_io(
    handle: HANDLE,
    timeout_ms: u32,
    start: impl FnOnce(*mut OVERLAPPED, *mut u32) -> i32,
) -> io::Result<usize> {
    let event_raw = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
    if event_raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateEventW returned a unique owned kernel handle.
    let event = unsafe { OwnedHandle::from_raw_handle(event_raw.cast()) };
    let mut overlapped = OVERLAPPED {
        hEvent: event_raw,
        ..Default::default()
    };
    let mut transferred = 0;

    if start(&mut overlapped, &mut transferred) != FALSE {
        return Ok(transferred as usize);
    }
    let error = unsafe { GetLastError() };
    if error != ERROR_IO_PENDING {
        return Err(io::Error::from_raw_os_error(error as i32));
    }

    let complete =
        unsafe { GetOverlappedResultEx(handle, &overlapped, &mut transferred, timeout_ms, FALSE) };
    if complete != FALSE {
        return Ok(transferred as usize);
    }
    let error = unsafe { GetLastError() };
    if error != WAIT_TIMEOUT {
        return Err(io::Error::from_raw_os_error(error as i32));
    }

    unsafe {
        CancelIoEx(handle, &overlapped);
        let completed_after_timeout =
            GetOverlappedResult(handle, &overlapped, &mut transferred, TRUE);
        if completed_after_timeout != FALSE {
            return Ok(transferred as usize);
        }
        let final_error = GetLastError();
        if final_error != ERROR_OPERATION_ABORTED {
            return Err(io::Error::from_raw_os_error(final_error as i32));
        }
    }
    let _ = event;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "local IPC operation timed out",
    ))
}

fn timeout_millis(timeout: Option<Duration>) -> io::Result<u32> {
    let Some(timeout) = timeout else {
        return Ok(NO_TIMEOUT);
    };
    if timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "zero IPC timeout is not supported",
        ));
    }
    let milliseconds = timeout.as_millis().max(1).min(u128::from(NO_TIMEOUT - 1));
    Ok(milliseconds as u32)
}

fn authenticate_client(pipe: HANDLE) -> io::Result<()> {
    let mut process_id = 0;
    if unsafe { GetNamedPipeClientProcessId(pipe, &mut process_id) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    authenticate_process(process_id)
}

fn authenticate_server(pipe: HANDLE) -> io::Result<()> {
    let mut process_id = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut process_id) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    authenticate_process(process_id)
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
    let current = current_user_sid()?;
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

fn current_user_sid() -> io::Result<Sid> {
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
        Ok(format!("D:P(A;;GA;;;{sid})"))
    }
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn current_user() -> io::Result<Self> {
        let sddl = current_user_sid()?.sddl()?;
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

fn wide_path(path: &Path) -> Vec<u16> {
    wide_string(path.as_os_str())
}

fn wide_string(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn test_endpoint() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(format!(
            r"\\.\pipe\pag-transport-test-{}-{sequence}",
            std::process::id()
        ))
    }

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
        let endpoint = test_endpoint();
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
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            io::ErrorKind::TimedOut
        );
        server.write_all(&[41]).unwrap();
        let mut response = [0];
        server.read_exact(&mut response).unwrap();
        assert_eq!(response, [42]);
        client.join().unwrap();
    }
}
