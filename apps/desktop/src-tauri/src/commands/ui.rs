//! Tauri commands served by the shared UI API, one per renderer method
//! (`desktop_core::renderer_methods!`), so capabilities grant each one; its
//! arguments are passed through unchanged as the method parameters the web UI
//! sends.

use std::sync::Arc;

use desktop_core::{client::Client, ui_api::Method};
use serde_json::Value;
use tauri::{
    ipc::{InvokeBody, Request},
    State, WebviewWindow,
};

macro_rules! ui_commands {
    (
        commands { $($command:ident => $command_variant:ident),+ $(,)? }
        host { $($host:ident => $host_variant:ident),+ $(,)? }
    ) => {
        ui_commands!(@each $($command => $command_variant,)+ $($host => $host_variant,)+);
    };
    (@each $($command:ident => $method:ident,)+) => {$(
        #[tauri::command]
        pub(crate) async fn $command(
            window: WebviewWindow,
            client: State<'_, Arc<Client>>,
            request: Request<'_>,
        ) -> Result<Value, String> {
            crate::ui_api::invoke(window, client, Method::$method, params(&request)?).await
        }
    )+};
}

desktop_core::renderer_methods!(ui_commands);

fn params(request: &Request<'_>) -> Result<Value, String> {
    match request.body() {
        InvokeBody::Json(params) => Ok(params.clone()),
        InvokeBody::Raw(_) => Err("Invalid management request".into()),
    }
}
