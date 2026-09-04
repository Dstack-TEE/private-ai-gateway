use std::{
    cell::RefCell,
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};

use desktop_runtime::contracts::GatewayState;
use gtk::glib;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
type Callback = Box<dyn FnOnce(Result<Value, String>)>;

pub struct RuntimeClient {
    input: Arc<Mutex<ChildStdin>>,
    child: RefCell<Option<Child>>,
    pending: Rc<RefCell<HashMap<String, Callback>>>,
    next_id: AtomicU64,
    on_state: Rc<RefCell<Option<Box<dyn Fn(GatewayState)>>>>,
    on_error: Rc<RefCell<Option<Box<dyn Fn(String)>>>>,
}

impl RuntimeClient {
    pub fn start() -> Result<Rc<Self>, String> {
        let service = runtime_path()?;
        let mut child = Command::new(&service)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("Cannot start the desktop runtime: {error}"))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| "Cannot write to the desktop runtime".to_string())?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| "Cannot read from the desktop runtime".to_string())?;
        let (sender, receiver) = mpsc::channel::<Result<Value, String>>();
        thread::spawn(move || read_messages(output, sender));
        let client = Rc::new(Self {
            input: Arc::new(Mutex::new(input)),
            child: RefCell::new(Some(child)),
            pending: Rc::new(RefCell::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            on_state: Rc::new(RefCell::new(None)),
            on_error: Rc::new(RefCell::new(None)),
        });
        let weak = Rc::downgrade(&client);
        glib::timeout_add_local(std::time::Duration::from_millis(30), move || {
            let Some(client) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            for message in receiver.try_iter() {
                client.handle(message);
            }
            glib::ControlFlow::Continue
        });
        Ok(client)
    }

    pub fn on_state(&self, callback: impl Fn(GatewayState) + 'static) {
        *self.on_state.borrow_mut() = Some(Box::new(callback));
    }

    pub fn on_error(&self, callback: impl Fn(String) + 'static) {
        *self.on_error.borrow_mut() = Some(Box::new(callback));
    }

    pub fn request<T: DeserializeOwned + 'static>(
        &self,
        method: &str,
        params: impl Serialize,
        callback: impl FnOnce(Result<T, String>) + 'static,
    ) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        self.pending.borrow_mut().insert(
            id.clone(),
            Box::new(move |result| {
                callback(result.and_then(|value| {
                    serde_json::from_value(value)
                        .map_err(|_| "The desktop runtime returned an invalid response".to_string())
                }));
            }),
        );
        let message = json!({ "schemaVersion": 1, "id": id, "method": method, "params": params });
        let mut bytes = match serde_json::to_vec(&message) {
            Ok(bytes) if bytes.len() <= MAX_MESSAGE_BYTES => bytes,
            _ => {
                self.fail_request(&id, "Desktop runtime request is too large");
                return;
            }
        };
        bytes.push(b'\n');
        let result = self
            .input
            .lock()
            .map_err(|_| "The desktop runtime input is unavailable".to_string())
            .and_then(|mut input| {
                input
                    .write_all(&bytes)
                    .and_then(|()| input.flush())
                    .map_err(|error| format!("Cannot write to the desktop runtime: {error}"))
            });
        if let Err(error) = result {
            self.fail_request(&id, &error);
        }
    }

    pub fn shutdown(&self) {
        self.request::<Value>("shutdown", json!({}), |_| {});
    }

    fn fail_request(&self, id: &str, error: &str) {
        if let Some(callback) = self.pending.borrow_mut().remove(id) {
            callback(Err(error.to_string()));
        }
    }

    fn handle(&self, incoming: Result<Value, String>) {
        let message = match incoming {
            Ok(message) => message,
            Err(error) => {
                if let Some(callback) = self.on_error.borrow().as_ref() {
                    callback(error);
                }
                return;
            }
        };
        if message.get("event").and_then(Value::as_str) == Some("stateChanged") {
            if let Ok(state) = serde_json::from_value::<GatewayState>(
                message.get("payload").cloned().unwrap_or(Value::Null),
            ) {
                if let Some(callback) = self.on_state.borrow().as_ref() {
                    callback(state);
                }
            }
            return;
        }
        let Some(id) = message.get("id").and_then(Value::as_str) else {
            return;
        };
        let Some(callback) = self.pending.borrow_mut().remove(id) else {
            return;
        };
        if let Some(error) = message.get("error") {
            callback(Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("The operation failed")
                .to_string()));
        } else {
            callback(Ok(message.get("result").cloned().unwrap_or(Value::Null)));
        }
    }
}

impl Drop for RuntimeClient {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.borrow_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
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

fn read_messages(output: impl std::io::Read, sender: mpsc::Sender<Result<Value, String>>) {
    for line in BufReader::new(output).lines() {
        let message = line
            .map_err(|error| format!("Cannot read from the desktop runtime: {error}"))
            .and_then(|line| {
                if line.len() > MAX_MESSAGE_BYTES {
                    return Err("Desktop runtime response is too large".to_string());
                }
                serde_json::from_str(&line)
                    .map_err(|_| "Desktop runtime returned invalid JSON".to_string())
            });
        if sender.send(message).is_err() {
            break;
        }
    }
}
