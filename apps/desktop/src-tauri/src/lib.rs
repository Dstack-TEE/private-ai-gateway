mod contracts;
mod gateway;
mod menu;
mod tray;
mod usage;

use std::sync::{Arc, Mutex};

use contracts::{AgentPreview, AgentStatus, ConnectOptions, GatewayState, StartGatewayConfig};
use desktop_gateway::{
    agents::{app_data_dir, helper_binary_name, Agent, Projector},
    catalog::Catalog,
    lock,
    proxy::{self, ProxyEvent, ProxyState},
    secrets::{validate_api_key, KeyringStore, SecretStore, API_KEY_ENTRY},
    tokens::{TokenFiles, TokenSet, LOCAL_TOOLS_AGENT},
};
use gateway::{GatewayManager, LOCAL_ADDR, LOCAL_ENDPOINT};
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_clipboard_manager::ClipboardExt;
use usage::{UsagePage, UsageQuery, UsageStore};

const AUTOSTART_ARG: &str = "--autostart";

/// The OS credential store holding the RedPill API key and parked secrets.
struct Secrets(Arc<dyn SecretStore>);

struct ClientCredentials(Mutex<TokenFiles>);

impl ClientCredentials {
    fn token(&self) -> Result<String, String> {
        self.0
            .lock()
            .map_err(|_| "Client credential store unavailable".to_string())?
            .ensure(LOCAL_TOOLS_AGENT)
    }

    fn rotate(&self) -> Result<String, String> {
        self.0
            .lock()
            .map_err(|_| "Client credential store unavailable".to_string())?
            .rotate(LOCAL_TOOLS_AGENT)
    }
}

fn with_client_token(
    mut tokens: TokenSet,
    credentials: &ClientCredentials,
) -> Result<TokenSet, String> {
    tokens.insert(credentials.token()?, LOCAL_TOOLS_AGENT.to_string());
    Ok(tokens)
}

/// What the launch established before the window: the bound endpoint. `Err`
/// means nothing may start or connect this launch; the window still opens to
/// say so and to allow disconnecting agents.
struct Launch(Mutex<Result<Option<Bound>, String>>);

/// The primary-instance lock, held for the whole process lifetime whether or
/// not the endpoint or identity could be established, so a failed launch
/// still keeps a second instance from claiming the port meanwhile.
struct Instance(#[allow(dead_code)] Option<lock::InstanceLock>);

struct Bound {
    listener: std::net::TcpListener,
}

/// The projection engine for this installation: agent configs reference the
/// bundled console helper next to this executable.
fn projector(secrets: &Secrets) -> Result<Projector, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("Cannot locate the app executable: {error}"))?;
    let helper = exe
        .parent()
        .ok_or_else(|| "Cannot locate the app directory".to_string())?
        .join(helper_binary_name());
    Projector::new(helper, LOCAL_ENDPOINT, secrets.0.clone())
}

#[tauri::command]
fn get_gateway_state(manager: State<'_, GatewayManager>) -> Result<GatewayState, String> {
    manager.snapshot()
}

#[tauri::command]
fn start_gateway(
    app: AppHandle,
    manager: State<'_, GatewayManager>,
    config: StartGatewayConfig,
) -> Result<GatewayState, String> {
    manager.start(&app, config)
}

#[tauri::command]
fn stop_gateway(
    app: AppHandle,
    manager: State<'_, GatewayManager>,
) -> Result<GatewayState, String> {
    manager.stop(&app)
}

/// Open the brand's support page in the system browser.
#[tauri::command]
fn open_support(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(desktop_gateway::brand::SUPPORT_URL, None::<&str>)
        .map_err(|error| format!("Cannot open the support page: {error}"))
}

#[tauri::command]
fn copy_text(app: AppHandle, text: String) -> Result<(), String> {
    if text.is_empty() || text.len() > 4_096 {
        return Err("Invalid clipboard text".to_string());
    }
    app.clipboard()
        .write_text(text)
        .map_err(|error| format!("Cannot copy text: {error}"))
}

#[tauri::command]
fn query_usage(store: State<'_, Arc<UsageStore>>, query: UsageQuery) -> Result<UsagePage, String> {
    store.page(&query)
}

#[tauri::command]
fn export_usage_csv(
    store: State<'_, Arc<UsageStore>>,
    query: UsageQuery,
    path: String,
) -> Result<usize, String> {
    store.export_csv(&query, std::path::Path::new(&path))
}

#[tauri::command]
fn clear_usage(
    app: AppHandle,
    manager: State<'_, GatewayManager>,
    store: State<'_, Arc<UsageStore>>,
) -> Result<u64, String> {
    let changed = store.clear()?;
    manager.clear_session_usage(&app);
    Ok(changed)
}

/// Save (or replace) the RedPill API key in the OS credential store. The key
/// is validated, stored, loaded into the proxy, and never returned.
#[tauri::command]
fn set_api_key(
    app: AppHandle,
    manager: State<'_, GatewayManager>,
    proxy: State<'_, Arc<ProxyState>>,
    secrets: State<'_, Secrets>,
    key: String,
) -> Result<GatewayState, String> {
    let key = validate_api_key(&key)?;
    secrets.0.set(API_KEY_ENTRY, &key)?;
    proxy.set_api_key(Some(key));
    manager.set_api_key_saved(&app, true);
    manager.snapshot()
}

/// Revoke first, then forget: the key leaves proxy memory (and admitted
/// deliveries are cancelled) before the credential store is touched. If the
/// store delete fails the key stays revoked for this session; the UI keeps
/// showing it as saved so the user can retry Delete or save a new key.
#[tauri::command]
fn clear_api_key(
    app: AppHandle,
    manager: State<'_, GatewayManager>,
    proxy: State<'_, Arc<ProxyState>>,
    secrets: State<'_, Secrets>,
) -> Result<GatewayState, String> {
    proxy.set_api_key(None);
    if let Err(error) = secrets.0.delete(API_KEY_ENTRY) {
        return Err(format!(
            "The key is revoked for this session, but removing it from the credential store \
             failed: {error}. Retry Delete or save a new key"
        ));
    }
    manager.set_api_key_saved(&app, false);
    manager.snapshot()
}

#[tauri::command]
fn get_client_key(credentials: State<'_, Arc<ClientCredentials>>) -> Result<String, String> {
    credentials.token()
}

#[tauri::command]
fn rotate_client_key(
    proxy: State<'_, Arc<ProxyState>>,
    credentials: State<'_, Arc<ClientCredentials>>,
) -> Result<String, String> {
    proxy.set_tokens(proxy.tokens().without(LOCAL_TOOLS_AGENT));
    let token = credentials.rotate()?;
    let mut tokens = proxy.tokens();
    tokens.insert(token.clone(), LOCAL_TOOLS_AGENT.to_string());
    proxy.set_tokens(tokens);
    Ok(token)
}

#[tauri::command]
async fn refresh_catalog(
    app: AppHandle,
    manager: State<'_, GatewayManager>,
) -> Result<GatewayState, String> {
    manager.refresh_catalog(&app).await
}

/// One refresh: scan the agents and publish exactly the tokens those
/// statuses authorize. Publishing cancels admitted-but-unsent deliveries,
/// so a config that drifted or broke stops opening the proxy in the same
/// operation that reports it. Startup takes this path too.
#[tauri::command]
fn list_agents(
    proxy: State<'_, Arc<ProxyState>>,
    secrets: State<'_, Secrets>,
    credentials: State<'_, Arc<ClientCredentials>>,
) -> Result<Vec<AgentStatus>, String> {
    let session = proxy.session();
    let catalog = session.verified.then_some(session.catalog).flatten();
    let (statuses, tokens) = projector(&secrets)?.scan(catalog.as_ref())?;
    proxy.set_tokens(with_client_token(tokens, &credentials)?);
    Ok(statuses)
}

#[tauri::command]
fn preview_agent_connection(
    manager: State<'_, GatewayManager>,
    proxy: State<'_, Arc<ProxyState>>,
    secrets: State<'_, Secrets>,
    agent_id: String,
    connect: bool,
    options: ConnectOptions,
) -> Result<AgentPreview, String> {
    let agent = Agent::from_id(&agent_id)?;
    let catalog = connection_catalog(&manager, &proxy, agent, connect)?;
    projector(&secrets)?.preview(agent, connect, catalog.as_ref(), &options)
}

#[tauri::command]
fn apply_agent_connection(
    manager: State<'_, GatewayManager>,
    proxy: State<'_, Arc<ProxyState>>,
    secrets: State<'_, Secrets>,
    credentials: State<'_, Arc<ClientCredentials>>,
    agent_id: String,
    connect: bool,
    revision: String,
    options: ConnectOptions,
) -> Result<AgentStatus, String> {
    let agent = Agent::from_id(&agent_id)?;
    let catalog = connection_catalog(&manager, &proxy, agent, connect)?;
    let projector = projector(&secrets)?;
    if !connect {
        // Revoke in memory first; a disconnect that fails part-way leaves the
        // agent disabled rather than still authorized.
        proxy.set_tokens(proxy.tokens().without(agent.id()));
    }
    let status = projector.apply(agent, connect, &revision, catalog.as_ref(), &options)?;
    proxy.set_tokens(with_client_token(projector.scan(None)?.1, &credentials)?);
    Ok(status)
}

/// Emergency restore: revoke every agent in memory, then disconnect and
/// restore all recorded agents regardless of support, endpoint, or gateway
/// state (token files are deleted before anything else is touched). Returns
/// the statuses afterwards; failures are reported as an error once every
/// agent was attempted.
#[tauri::command]
fn disconnect_all_agents(
    proxy: State<'_, Arc<ProxyState>>,
    secrets: State<'_, Secrets>,
    credentials: State<'_, Arc<ClientCredentials>>,
) -> Result<Vec<AgentStatus>, String> {
    proxy.set_tokens(with_client_token(TokenSet::default(), &credentials)?);
    let projector = projector(&secrets)?;
    match projector.disconnect_all() {
        // Durable revocation or the tombstone step failed: keep every token
        // revoked in memory rather than reloading from the store.
        Err(error) => Err(format!(
            "Restore all could not revoke the agents ({error}); access stays revoked until \
             it is retried"
        )),
        Ok(failures) => {
            let (statuses, tokens) = projector.scan(None)?;
            proxy.set_tokens(with_client_token(tokens, &credentials)?);
            if failures.is_empty() {
                Ok(statuses)
            } else {
                Err(failures
                    .into_iter()
                    .map(|(agent, error)| format!("{agent}: {error}"))
                    .collect::<Vec<_>>()
                    .join("; "))
            }
        }
    }
}

/// The catalog a connection may project from: only the one published with
/// the current verified session, and only while the local endpoint is bound.
/// Disconnecting never needs either.
fn connection_catalog(
    manager: &GatewayManager,
    proxy: &ProxyState,
    agent: Agent,
    connect: bool,
) -> Result<Option<Catalog>, String> {
    if !connect {
        return Ok(None);
    }
    if let Some(error) = manager.snapshot()?.endpoint_error {
        return Err(format!("The local endpoint is unavailable: {error}"));
    }
    let session = proxy.session();
    match (session.verified, session.catalog) {
        (true, Some(catalog)) => Ok(Some(catalog)),
        _ => Err(format!(
            "Start the gateway and wait until it is verified; the model list for {} comes from it",
            agent.name()
        )),
    }
}

/// Become the primary instance and claim the endpoint before the window
/// exists. The instance lock is returned separately so it outlives any later
/// failure.
fn launch() -> (Option<lock::InstanceLock>, Result<Option<Bound>, String>) {
    let data_dir = match app_data_dir() {
        Ok(dir) => dir,
        Err(error) => return (None, Err(error)),
    };
    let instance = match lock::instance(&data_dir) {
        Ok(Some(instance)) => instance,
        Ok(None) => return (None, Ok(None)),
        Err(error) => return (None, Err(format!("Cannot take the instance lock: {error}"))),
    };
    let bound = proxy::bind_std(LOCAL_ADDR).map(|listener| Some(Bound { listener }));
    (Some(instance), bound)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let show_on_launch =
        !std::env::args_os().any(|argument| argument == std::ffi::OsStr::new(AUTOSTART_ARG));
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringStore);
    let (instance, launched) = launch();
    let (events, mut proxy_events) = tokio::sync::mpsc::channel::<ProxyEvent>(256);
    let proxy = ProxyState::new(events);
    let client_credentials = match app_data_dir() {
        Ok(dir) => Arc::new(ClientCredentials(Mutex::new(TokenFiles::new(&dir)))),
        Err(error) => {
            eprintln!("Cannot initialize client credentials: {error}");
            return;
        }
    };
    let (usage, usage_error) =
        match app_data_dir().and_then(|dir| UsageStore::open(dir.join("usage.sqlite3"))) {
            Ok(store) => (Arc::new(store), None),
            Err(error) => match UsageStore::memory() {
                Ok(store) => (
                    Arc::new(store),
                    Some(format!(
                        "Usage history is unavailable for this launch: {error}"
                    )),
                ),
                Err(fallback_error) => {
                    eprintln!("Cannot initialize usage storage: {error}; {fallback_error}");
                    return;
                }
            },
        };

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_ARG]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(GatewayManager::new(proxy.clone(), usage.clone()))
        .manage(proxy.clone())
        .manage(client_credentials.clone())
        .manage(usage)
        .manage(Secrets(secrets.clone()))
        .manage(Instance(instance))
        .manage(Launch(Mutex::new(launched)))
        .invoke_handler(tauri::generate_handler![
            get_gateway_state,
            start_gateway,
            stop_gateway,
            copy_text,
            query_usage,
            export_usage_csv,
            clear_usage,
            open_support,
            set_api_key,
            clear_api_key,
            get_client_key,
            rotate_client_key,
            refresh_catalog,
            list_agents,
            preview_agent_connection,
            apply_agent_connection,
            disconnect_all_agents
        ])
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let window = app
                .get_webview_window("main")
                .ok_or_else(|| "main window was not created".to_string())?;
            // The tracked config keeps the window geometry; the brand owns the
            // title, applied here while the window is still hidden.
            window.set_title(desktop_gateway::brand::PRODUCT_NAME)?;
            // Closing the window only hides it; the tray keeps the app alive.
            let window_for_events = window.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window_for_events.hide();
                }
            });
            tray::setup(app.handle())?;
            menu::setup(app.handle())?;

            let handle = app.handle().clone();
            let manager = app.state::<GatewayManager>();
            let launched = app
                .state::<Launch>()
                .0
                .lock()
                .map_err(|_| "launch state unavailable".to_string())?
                .as_mut()
                .map(Option::take)
                .map_err(|error| error.clone());
            match launched {
                Ok(Some(bound)) => {
                    manager.set_endpoint(&handle, Ok(()));
                    let serve_handle = handle.clone();
                    let serve_proxy = proxy.clone();
                    tauri::async_runtime::spawn(async move {
                        let result = proxy::serve(serve_proxy, bound.listener).await;
                        if let Err(error) = result {
                            let manager = serve_handle.state::<GatewayManager>();
                            manager.set_endpoint(&serve_handle, Err(error));
                        }
                    });
                }
                Ok(None) => manager.set_endpoint(
                    &handle,
                    Err(format!(
                        "Another instance of {} is the primary instance",
                        desktop_gateway::brand::PRODUCT_NAME
                    )),
                ),
                Err(error) => manager.set_endpoint(&handle, Err(error)),
            }

            // Load what the proxy needs to admit and forward: the saved key
            // (only its presence reaches the UI) and connected agents' tokens.
            match secrets.get(API_KEY_ENTRY) {
                Ok(key) => {
                    manager.set_api_key_saved(&handle, key.is_some());
                    proxy.set_api_key(key);
                }
                Err(error) => manager.report_error(&handle, error),
            }
            if let Some(error) = usage_error.clone() {
                manager.report_error(&handle, error);
            }
            // Startup maintenance (under the config lock) before authorizing
            // any token.
            let store = Secrets(secrets.clone());
            match with_client_token(TokenSet::default(), &client_credentials) {
                Ok(tokens) => proxy.set_tokens(tokens),
                Err(error) => manager.report_error(&handle, error),
            }
            match projector(&store).and_then(|projector| {
                projector.migrate_legacy()?;
                projector.scan(None)
            }) {
                Ok((_, tokens)) => match with_client_token(tokens, &client_credentials) {
                    Ok(tokens) => proxy.set_tokens(tokens),
                    Err(error) => manager.report_error(&handle, error),
                },
                Err(error) => manager.report_error(&handle, error),
            }

            let events_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                while let Some(event) = proxy_events.recv().await {
                    let manager = events_handle.state::<GatewayManager>();
                    manager.record_proxy_event(&events_handle, event);
                }
            });
            if show_on_launch {
                tray::show_window(app.handle());
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Tauri application");

    app.run(|app, event| match event {
        tauri::RunEvent::ExitRequested { .. } => {
            let manager = app.state::<GatewayManager>();
            let _ = manager.stop(app);
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => tray::show_window(app),
        _ => {}
    });
}
