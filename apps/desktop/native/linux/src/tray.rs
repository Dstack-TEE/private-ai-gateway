use std::{path::PathBuf, sync::mpsc::Sender};

use ksni::blocking::{Handle, TrayMethods};

#[derive(Clone, Copy)]
pub enum TrayCommand {
    Toggle,
    Open,
    Settings,
    OpenAtLogin,
    Quit,
}

pub struct GatewayTray {
    pub running: bool,
    pub protected: bool,
    pub status: String,
    pub open_at_login: bool,
    pub icon_path: String,
    pub commands: Sender<TrayCommand>,
}

impl ksni::Tray for GatewayTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "org.dstack.private-ai-gateway".into()
    }
    fn title(&self) -> String {
        format!("Private AI Gateway - {}", self.status)
    }
    fn icon_theme_path(&self) -> String {
        self.icon_path.clone()
    }
    fn icon_name(&self) -> String {
        if self.protected {
            "private-ai-gateway-protected"
        } else {
            "private-ai-gateway"
        }
        .into()
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, StandardItem};
        vec![
            CheckmarkItem {
                label: "Protected".into(),
                checked: self.running,
                activate: Box::new(|tray: &mut GatewayTray| {
                    let _ = tray.commands.send(TrayCommand::Toggle);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.status.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Open Private AI Gateway".into(),
                activate: Box::new(|tray: &mut GatewayTray| {
                    let _ = tray.commands.send(TrayCommand::Open);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Settings…".into(),
                activate: Box::new(|tray: &mut GatewayTray| {
                    let _ = tray.commands.send(TrayCommand::Settings);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            CheckmarkItem {
                label: "Open at Login".into(),
                checked: self.open_at_login,
                activate: Box::new(|tray: &mut GatewayTray| {
                    let _ = tray.commands.send(TrayCommand::OpenAtLogin);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit Private AI Gateway".into(),
                activate: Box::new(|tray: &mut GatewayTray| {
                    let _ = tray.commands.send(TrayCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub fn spawn(commands: Sender<TrayCommand>) -> Option<Handle<GatewayTray>> {
    GatewayTray {
        running: false,
        protected: false,
        status: "Not protected".into(),
        open_at_login: open_at_login(),
        icon_path: asset_dir().to_string_lossy().into_owned(),
        commands,
    }
    .assume_sni_available(true)
    .spawn()
    .ok()
}

pub fn set_open_at_login(enabled: bool) -> Result<(), String> {
    let path = autostart_path()?;
    if enabled {
        let parent = path
            .parent()
            .ok_or_else(|| "Cannot locate the autostart directory".to_string())?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Cannot create the autostart directory: {error}"))?;
        let executable = std::env::current_exe()
            .map_err(|error| format!("Cannot locate the application: {error}"))?;
        let exec = executable
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let desktop = format!("[Desktop Entry]\nType=Application\nName=Private AI Gateway\nExec=\"{exec}\" --autostart\nIcon=private-ai-gateway\nTerminal=false\nX-GNOME-Autostart-enabled=true\n");
        std::fs::write(path, desktop)
            .map_err(|error| format!("Cannot enable Open at Login: {error}"))
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("Cannot disable Open at Login: {error}")),
        }
    }
}

pub fn open_at_login() -> bool {
    autostart_path().is_ok_and(|path| path.is_file())
}

fn autostart_path() -> Result<PathBuf, String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| "Cannot locate the user configuration directory".to_string())?;
    Ok(config.join("autostart/org.dstack.private-ai-gateway.desktop"))
}

fn asset_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("PRIVATE_AI_GATEWAY_ASSETS") {
        return path.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from))
        .unwrap_or_default()
        .join("Assets/brand")
}
