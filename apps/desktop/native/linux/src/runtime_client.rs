use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io::{Read, Write},
    os::fd::AsRawFd,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use desktop_runtime::contracts::GatewayState;
use gtk::glib;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const OUTBOUND_CAPACITY: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(75);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
type Callback = Box<dyn FnOnce(Result<Value, String>)>;

struct PendingRequest {
    deadline: Instant,
    callback: Callback,
}

enum Outbound {
    Message(Vec<u8>),
    Stop,
}

enum Incoming {
    Message(Value),
    Disconnected(String),
}

pub struct RuntimeClient {
    outbound: RefCell<Option<SyncSender<Outbound>>>,
    incoming: Receiver<Incoming>,
    child: RefCell<Option<Child>>,
    reader: RefCell<Option<JoinHandle<()>>>,
    writer: RefCell<Option<JoinHandle<()>>>,
    pending: RefCell<HashMap<String, PendingRequest>>,
    next_id: AtomicU64,
    connected: Cell<bool>,
    shutting_down: Cell<bool>,
    request_timeout: Duration,
    shutdown_timeout: Duration,
    stop_threads: Arc<AtomicBool>,
    on_state: RefCell<Option<Box<dyn Fn(GatewayState)>>>,
    on_disconnect: RefCell<Option<Box<dyn Fn(String)>>>,
}

impl RuntimeClient {
    pub fn start() -> Result<Rc<Self>, String> {
        Self::start_path(&runtime_path()?, REQUEST_TIMEOUT, SHUTDOWN_TIMEOUT)
    }

    fn start_path(
        path: &Path,
        request_timeout: Duration,
        shutdown_timeout: Duration,
    ) -> Result<Rc<Self>, String> {
        Self::start_command(&mut Command::new(path), request_timeout, shutdown_timeout)
    }

    fn start_command(
        command: &mut Command,
        request_timeout: Duration,
        shutdown_timeout: Duration,
    ) -> Result<Rc<Self>, String> {
        let mut child = command
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("Cannot start the desktop runtime: {error}"))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| "Cannot write to the desktop runtime".to_string())?;
        if let Err(error) = set_nonblocking(&input) {
            force_stop_group(&mut child);
            return Err(error);
        }
        let Some(output) = child.stdout.take() else {
            force_stop_group(&mut child);
            return Err("Cannot read from the desktop runtime".to_string());
        };
        if let Err(error) = set_nonblocking(&output) {
            force_stop_group(&mut child);
            return Err(error);
        }

        let (outbound, outbound_rx) = mpsc::sync_channel(OUTBOUND_CAPACITY);
        let (incoming_tx, incoming) = mpsc::channel();
        let stop_threads = Arc::new(AtomicBool::new(false));
        let writer_events = incoming_tx.clone();
        let writer_stop = stop_threads.clone();
        let reader_stop = stop_threads.clone();
        let writer =
            thread::spawn(move || write_messages(input, outbound_rx, writer_events, writer_stop));
        let reader = thread::spawn(move || read_messages(output, incoming_tx, reader_stop));
        let client = Rc::new(Self {
            outbound: RefCell::new(Some(outbound)),
            incoming,
            child: RefCell::new(Some(child)),
            reader: RefCell::new(Some(reader)),
            writer: RefCell::new(Some(writer)),
            pending: RefCell::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            connected: Cell::new(true),
            shutting_down: Cell::new(false),
            request_timeout,
            shutdown_timeout,
            stop_threads,
            on_state: RefCell::new(None),
            on_disconnect: RefCell::new(None),
        });
        let weak = Rc::downgrade(&client);
        glib::timeout_add_local(Duration::from_millis(30), move || {
            let Some(client) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            client.poll();
            glib::ControlFlow::Continue
        });
        Ok(client)
    }

    pub fn on_state(&self, callback: impl Fn(GatewayState) + 'static) {
        *self.on_state.borrow_mut() = Some(Box::new(callback));
    }

    pub fn on_disconnect(&self, callback: impl Fn(String) + 'static) {
        *self.on_disconnect.borrow_mut() = Some(Box::new(callback));
    }

    pub fn request<T: DeserializeOwned + 'static>(
        &self,
        method: &str,
        params: impl Serialize,
        callback: impl FnOnce(Result<T, String>) + 'static,
    ) {
        if !self.connected.get() {
            callback(Err("The desktop runtime is disconnected".to_string()));
            return;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let message = json!({ "schemaVersion": 1, "id": id, "method": method, "params": params });
        let mut bytes = match serde_json::to_vec(&message) {
            Ok(bytes) if bytes.len() <= MAX_MESSAGE_BYTES => bytes,
            _ => {
                callback(Err("Desktop runtime request is too large".to_string()));
                return;
            }
        };
        bytes.push(b'\n');
        self.pending.borrow_mut().insert(
            id.clone(),
            PendingRequest {
                deadline: Instant::now() + self.request_timeout,
                callback: Box::new(move |result| {
                    callback(result.and_then(|value| {
                        serde_json::from_value(value).map_err(|_| {
                            "The desktop runtime returned an invalid response".to_string()
                        })
                    }));
                }),
            },
        );
        let result = self
            .outbound
            .borrow()
            .as_ref()
            .ok_or_else(|| "The desktop runtime is disconnected".to_string())
            .and_then(|sender| {
                sender
                    .try_send(Outbound::Message(bytes))
                    .map_err(|error| match error {
                        TrySendError::Full(_) => "The desktop runtime is busy".to_string(),
                        TrySendError::Disconnected(_) => {
                            "The desktop runtime is disconnected".to_string()
                        }
                    })
            });
        if let Err(error) = result {
            self.fail_request(&id, error);
        }
    }

    pub fn is_process_running(&self) -> bool {
        self.child
            .borrow()
            .as_ref()
            .is_some_and(|child| !child_has_exited(child))
    }

    pub fn shutdown_and_wait(&self) {
        if self.shutting_down.replace(true) {
            return;
        }
        if self.connected.get() {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
            if let Ok(mut bytes) = serde_json::to_vec(
                &json!({ "schemaVersion": 1, "id": id, "method": "shutdown", "params": {} }),
            ) {
                bytes.push(b'\n');
                if let Some(sender) = self.outbound.borrow().as_ref() {
                    let _ = sender.try_send(Outbound::Message(bytes));
                }
            }
        }

        let deadline = Instant::now() + self.shutdown_timeout;
        let mut child = self.child.borrow_mut().take();
        while Instant::now() < deadline {
            let exited = child.as_ref().is_none_or(child_has_exited);
            if exited {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        if let Some(child) = child.as_mut() {
            kill_process_group(child, libc::SIGKILL);
            let _ = child.wait();
        }
        if let Some(sender) = self.outbound.borrow_mut().take() {
            let _ = sender.try_send(Outbound::Stop);
            drop(sender);
        }
        self.stop_threads.store(true, Ordering::Release);
        join_thread(&self.writer);
        join_thread(&self.reader);
        self.connected.set(false);
        self.fail_all("The desktop runtime stopped");
    }

    fn poll(&self) {
        for incoming in self.incoming.try_iter() {
            match incoming {
                Incoming::Message(message) if self.connected.get() => self.handle(message),
                Incoming::Message(_) => {}
                Incoming::Disconnected(error) => self.disconnect(error),
            }
        }
        let exited = self.child.borrow().as_ref().is_some_and(child_has_exited);
        if self.connected.get() && exited {
            self.disconnect("The desktop runtime exited unexpectedly".to_string());
        }
        let now = Instant::now();
        let expired = self
            .pending
            .borrow()
            .iter()
            .filter_map(|(id, request)| (request.deadline <= now).then(|| id.clone()))
            .collect::<Vec<_>>();
        if !expired.is_empty() {
            self.disconnect("The desktop runtime request timed out".to_string());
        }
    }

    fn fail_request(&self, id: &str, error: String) {
        let request = self.pending.borrow_mut().remove(id);
        if let Some(request) = request {
            (request.callback)(Err(error));
        }
    }

    fn fail_all(&self, error: &str) {
        let callbacks = self
            .pending
            .borrow_mut()
            .drain()
            .map(|(_, request)| request.callback)
            .collect::<Vec<_>>();
        for callback in callbacks {
            callback(Err(error.to_string()));
        }
    }

    fn disconnect(&self, error: String) {
        if !self.connected.replace(false) {
            return;
        }
        self.stop_threads.store(true, Ordering::Release);
        self.outbound.borrow_mut().take();
        if let Some(mut child) = self.child.borrow_mut().take() {
            force_stop_group(&mut child);
        }
        join_thread(&self.writer);
        join_thread(&self.reader);
        self.fail_all(&error);
        if !self.shutting_down.get() {
            if let Some(callback) = self.on_disconnect.borrow().as_ref() {
                callback(error);
            }
        }
    }

    fn handle(&self, message: Value) {
        let Some(object) = message.as_object() else {
            self.disconnect("The desktop runtime returned an invalid message".to_string());
            return;
        };
        if object.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
            self.disconnect("The desktop runtime uses an unsupported protocol version".to_string());
            return;
        }
        if object.contains_key("event") {
            if !has_exact_keys(object, &["schemaVersion", "event", "payload"])
                || object.get("event").and_then(Value::as_str) != Some("stateChanged")
            {
                self.disconnect("The desktop runtime returned an invalid event".to_string());
                return;
            }
            match serde_json::from_value::<GatewayState>(
                object.get("payload").cloned().unwrap_or(Value::Null),
            ) {
                Ok(state) => {
                    if let Some(callback) = self.on_state.borrow().as_ref() {
                        callback(state);
                    }
                }
                Err(_) => self
                    .disconnect("The desktop runtime returned an invalid state event".to_string()),
            }
            return;
        }
        let result = object.get("result");
        let error = object.get("error");
        let valid_result = result.is_some()
            && error.is_none()
            && has_exact_keys(object, &["schemaVersion", "id", "result"]);
        let valid_error = error.is_some_and(|error| {
            error.get("code").and_then(Value::as_str).is_some()
                && error.get("message").and_then(Value::as_str).is_some()
        }) && result.is_none()
            && has_exact_keys(object, &["schemaVersion", "id", "error"]);
        if valid_result == valid_error {
            self.disconnect("The desktop runtime returned an invalid response".to_string());
            return;
        }
        let Some(id) = object.get("id").and_then(Value::as_str) else {
            self.disconnect("The desktop runtime returned an invalid message".to_string());
            return;
        };
        let request = self.pending.borrow_mut().remove(id);
        let Some(request) = request else {
            return;
        };
        if let Some(error) = error {
            (request.callback)(Err(error
                .get("message")
                .and_then(Value::as_str)
                .expect("validated runtime error message")
                .to_string()));
        } else {
            (request.callback)(Ok(result.cloned().unwrap_or(Value::Null)));
        }
    }
}

fn has_exact_keys(object: &serde_json::Map<String, Value>, keys: &[&str]) -> bool {
    object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
}

impl Drop for RuntimeClient {
    fn drop(&mut self) {
        self.shutdown_and_wait();
    }
}

fn set_nonblocking(handle: &impl AsRawFd) -> Result<(), String> {
    let fd = handle.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(format!(
            "Cannot configure desktop runtime input: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn write_messages(
    mut input: ChildStdin,
    receiver: Receiver<Outbound>,
    events: mpsc::Sender<Incoming>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        let outbound = match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(outbound) => outbound,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match outbound {
            Outbound::Message(bytes) => {
                if let Err(error) = write_frame(&mut input, &bytes, &stop) {
                    if !stop.load(Ordering::Acquire) {
                        let _ = events.send(Incoming::Disconnected(error));
                    }
                    break;
                }
            }
            Outbound::Stop => break,
        }
    }
}

fn write_frame(input: &mut ChildStdin, bytes: &[u8], stop: &AtomicBool) -> Result<(), String> {
    let deadline = Instant::now() + WRITE_TIMEOUT;
    let mut written = 0;
    while written < bytes.len() {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        match input.write(&bytes[written..]) {
            Ok(0) => return Err("The desktop runtime closed its input".to_string()),
            Ok(count) => written += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("Writing to the desktop runtime timed out".to_string());
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(format!("Cannot write to the desktop runtime: {error}")),
        }
    }
    Ok(())
}

fn read_messages(mut output: impl Read, sender: mpsc::Sender<Incoming>, stop: Arc<AtomicBool>) {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        match output.read(&mut chunk) {
            Ok(0) => {
                if !stop.load(Ordering::Acquire) {
                    let _ = sender.send(Incoming::Disconnected(
                        "The desktop runtime closed its output".to_string(),
                    ));
                }
                return;
            }
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => {
                let _ = sender.send(Incoming::Disconnected(format!(
                    "Cannot read from the desktop runtime: {error}"
                )));
                return;
            }
        }
        if buffer.len() > MAX_MESSAGE_BYTES && !buffer.contains(&b'\n') {
            let _ = sender.send(Incoming::Disconnected(
                "Desktop runtime response is too large".to_string(),
            ));
            return;
        }
        while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            let line = buffer.drain(..=newline).collect::<Vec<_>>();
            let line = &line[..line.len() - 1];
            if line.is_empty() {
                continue;
            }
            if line.len() > MAX_MESSAGE_BYTES {
                let _ = sender.send(Incoming::Disconnected(
                    "Desktop runtime response is too large".to_string(),
                ));
                return;
            }
            match serde_json::from_slice(line) {
                Ok(message) => {
                    if sender.send(Incoming::Message(message)).is_err() {
                        return;
                    }
                }
                Err(_) => {
                    let _ = sender.send(Incoming::Disconnected(
                        "Desktop runtime returned invalid JSON".to_string(),
                    ));
                    return;
                }
            }
        }
    }
}

fn join_thread(thread: &RefCell<Option<JoinHandle<()>>>) {
    if let Some(thread) = thread.borrow_mut().take() {
        let _ = thread.join();
    }
}

fn kill_process_group(child: &Child, signal: libc::c_int) {
    if let Ok(process_group) = i32::try_from(child.id()) {
        unsafe {
            libc::kill(-process_group, signal);
        }
    }
}

fn child_has_exited(child: &Child) -> bool {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        return std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD);
    }
    unsafe { info.assume_init().si_pid() != 0 }
}

fn force_stop_group(child: &mut Child) {
    kill_process_group(child, libc::SIGKILL);
    let _ = child.wait();
}

fn runtime_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("PRIVATE_AI_GATEWAY_RUNTIME") {
        return Ok(path.into());
    }
    let executable = std::env::current_exe()
        .map_err(|error| format!("Cannot locate the application: {error}"))?;
    Ok(executable
        .parent()
        .ok_or_else(|| "Cannot locate the application directory".to_string())?
        .join("private-ai-gateway-desktop-service"))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    struct TestRuntime {
        script: PathBuf,
        marker: PathBuf,
    }

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    impl TestRuntime {
        fn new(body: impl FnOnce(&Path) -> String) -> Self {
            let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "private-ai-gateway-runtime-{}-{nonce}",
                std::process::id()
            ));
            let script = base.with_extension("py");
            let marker = base.with_extension("pid");
            fs::write(&script, format!("{}\n", body(&marker))).expect("write runtime fixture");
            Self { script, marker }
        }

        fn client(&self, request_timeout: Duration) -> Rc<RuntimeClient> {
            RuntimeClient::start_command(
                Command::new("python3").arg(&self.script),
                request_timeout,
                Duration::from_millis(150),
            )
            .expect("start runtime fixture")
        }
    }

    impl Drop for TestRuntime {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.script);
            let _ = fs::remove_file(&self.marker);
        }
    }

    fn poll_until(client: &RuntimeClient, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition() && Instant::now() < deadline {
            client.poll();
            thread::sleep(Duration::from_millis(10));
        }
        assert!(condition(), "condition did not complete");
    }

    #[test]
    fn request_round_trip_and_graceful_shutdown() {
        let runtime = TestRuntime::new(|marker| {
            format!(
                concat!(
                    "import json, subprocess, sys\n",
                    "child = subprocess.Popen(['sleep', '60'])\n",
                    "open({marker:?}, 'w').write(str(child.pid))\n",
                    "for line in sys.stdin:\n",
                    "    request = json.loads(line)\n",
                    "    result = None if request['method'] == 'shutdown' else {{'ok': True}}\n",
                    "    print(json.dumps({{'schemaVersion': 1, 'id': request['id'], 'result': result}}), flush=True)\n",
                    "    if request['method'] == 'shutdown':\n",
                    "        break"
                ),
                marker = marker.display().to_string()
            )
        });
        let client = runtime.client(Duration::from_secs(1));
        poll_until(&client, || runtime.marker.is_file());
        let descendant = fs::read_to_string(&runtime.marker)
            .expect("descendant pid")
            .trim()
            .parse::<u32>()
            .expect("numeric descendant pid");
        let result = Rc::new(RefCell::new(None));
        let received = result.clone();
        client.request::<Value>("getState", json!({}), move |value| {
            *received.borrow_mut() = Some(value)
        });
        poll_until(&client, || result.borrow().is_some());
        assert_eq!(
            result.borrow().as_ref().unwrap().as_ref().unwrap()["ok"],
            true
        );
        client.shutdown_and_wait();
        assert!(!client.is_process_running());
        assert!(!process_is_live(descendant));
    }

    #[test]
    fn eof_disconnects_and_fails_pending_requests() {
        let runtime = TestRuntime::new(|_| "raise SystemExit(7)".to_string());
        let client = runtime.client(Duration::from_secs(1));
        let disconnected = Rc::new(RefCell::new(None));
        let observed = disconnected.clone();
        client.on_disconnect(move |error| *observed.borrow_mut() = Some(error));
        let result = Rc::new(RefCell::new(None));
        let received = result.clone();
        client.request::<Value>("getState", json!({}), move |value| {
            *received.borrow_mut() = Some(value)
        });
        poll_until(&client, || disconnected.borrow().is_some());
        assert!(result.borrow().as_ref().is_some_and(Result::is_err));
        client.shutdown_and_wait();
    }

    #[test]
    fn unsupported_protocol_version_disconnects() {
        let runtime = TestRuntime::new(|_| {
            concat!(
                "import json, sys\n",
                "request = json.loads(sys.stdin.readline())\n",
                "print(json.dumps({'schemaVersion': 2, 'id': request['id'], 'result': None}), flush=True)"
            )
            .to_string()
        });
        let client = runtime.client(Duration::from_secs(1));
        let disconnected = Rc::new(RefCell::new(None));
        let observed = disconnected.clone();
        client.on_disconnect(move |error| *observed.borrow_mut() = Some(error));
        let result = Rc::new(RefCell::new(None));
        let received = result.clone();
        client.request::<Value>("getState", json!({}), move |value| {
            *received.borrow_mut() = Some(value)
        });
        poll_until(&client, || disconnected.borrow().is_some());
        assert!(disconnected
            .borrow()
            .as_deref()
            .is_some_and(|error| error.contains("protocol version")));
        assert!(result.borrow().as_ref().is_some_and(Result::is_err));
        client.shutdown_and_wait();
    }

    #[test]
    fn timeout_disconnects_and_stops_owned_process_group() {
        let runtime = TestRuntime::new(|marker| {
            format!(
                "import subprocess, sys\nchild = subprocess.Popen(['sleep', '60'])\nopen({:?}, 'w').write(str(child.pid))\nfor _line in sys.stdin:\n    pass",
                marker.display().to_string()
            )
        });
        let client = runtime.client(Duration::from_millis(50));
        poll_until(&client, || runtime.marker.is_file());
        let result = Rc::new(RefCell::new(None));
        let received = result.clone();
        client.request::<Value>("getState", json!({}), move |value| {
            *received.borrow_mut() = Some(value)
        });
        poll_until(&client, || result.borrow().is_some());
        assert!(result.borrow().as_ref().is_some_and(Result::is_err));
        let descendant = fs::read_to_string(&runtime.marker)
            .expect("descendant pid")
            .trim()
            .parse::<u32>()
            .expect("numeric descendant pid");
        poll_until(&client, || !process_is_live(descendant));
        client.shutdown_and_wait();
    }

    fn process_is_live(pid: u32) -> bool {
        fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(") ").map(|(_, fields)| fields.to_string()))
            .and_then(|fields| fields.chars().next())
            .is_some_and(|state| state != 'Z')
    }
}
