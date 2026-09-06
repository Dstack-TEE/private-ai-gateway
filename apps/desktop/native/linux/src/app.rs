use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::mpsc,
};

use adw::prelude::*;
use desktop_gateway::agents::{AgentPreview, AgentStatus, ConnectOptions};
use desktop_runtime::{
    contracts::{
        ConfidentialProfile, ConfidentialProfileInput, GatewayState, LocalApiConfig,
        RequestActivity, ServiceProvider, UsageSummary,
    },
    usage::{UsagePage, UsageQuery},
};
use gtk::{gdk, glib, Align, Orientation};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;

use crate::{
    runtime_client::RuntimeClient,
    tray::{self, TrayCommand},
};

mod dialogs;
mod views;

use views::*;

#[derive(Clone, Copy, Default)]
struct LaunchOptions {
    autostart: bool,
    smoke_test: bool,
}

thread_local! {
    static LAUNCH_OPTIONS: Cell<LaunchOptions> = Cell::new(LaunchOptions::default());
    static SMOKE_RESULT: Cell<i32> = const { Cell::new(1) };
}

pub fn run() -> i32 {
    let arguments = std::env::args().collect::<Vec<_>>();
    LAUNCH_OPTIONS.set(LaunchOptions {
        autostart: arguments.iter().any(|argument| argument == "--autostart"),
        smoke_test: arguments.iter().any(|argument| argument == "--smoke-test"),
    });
    adw::init().expect("libadwaita could not be initialized");
    let app = adw::Application::builder()
        .application_id("org.dstack.private-ai-gateway")
        .flags(gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_startup(|_| install_css());
    app.connect_activate(build_app);
    app.connect_command_line(|app, _| {
        app.activate();
        0.into()
    });
    app.run();
    if LAUNCH_OPTIONS.get().smoke_test {
        SMOKE_RESULT.get()
    } else {
        0
    }
}

struct Ui {
    window: glib::WeakRef<adw::ApplicationWindow>,
    toast: adw::ToastOverlay,
    stack: gtk::Stack,
    sidebar: gtk::ListBox,
    title: gtk::Label,
    status: gtk::Label,
    dev: gtk::Label,
    protection: gtk::Switch,
    restart: gtk::Button,
    syncing: Cell<bool>,
    page: Cell<usize>,
    client: RefCell<Option<Rc<RuntimeClient>>>,
    generation: Cell<u64>,
    runtime_connected: Cell<bool>,
    runtime_error: RefCell<Option<String>>,
    handshake_remaining: Cell<u8>,
    smoke_pending: Cell<bool>,
    state: RefCell<GatewayState>,
    agents: RefCell<Vec<AgentStatus>>,
    usage: RefCell<UsagePage>,
    client_key: RefCell<String>,
    usage_agent: RefCell<Option<String>>,
    usage_model: RefCell<Option<String>>,
    usage_range: Cell<usize>,
    tray: RefCell<Option<ksni::blocking::Handle<tray::GatewayTray>>>,
    tray_available: Cell<bool>,
    views: RefCell<Option<UiViews>>,
    shutting_down: Cell<bool>,
}

fn build_app(app: &adw::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let runtime = RuntimeClient::start();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Private AI Gateway")
        .default_width(1052)
        .default_height(820)
        .width_request(760)
        .height_request(620)
        .build();
    let header = adw::HeaderBar::new();
    let title = gtk::Label::builder()
        .label("Overview")
        .css_classes(["title"])
        .halign(Align::Start)
        .hexpand(true)
        .build();
    header.set_title_widget(Some(&title));
    let dev = gtk::Label::builder()
        .label("Dev mode")
        .css_classes(["dev-badge"])
        .visible(false)
        .build();
    let status = gtk::Label::builder()
        .label("Not protected")
        .css_classes(["dim-label"])
        .build();
    let protected_label = gtk::Label::new(Some("Protection"));
    let protection = gtk::Switch::new();
    let switch_box = gtk::Box::new(Orientation::Horizontal, 8);
    switch_box.append(&protected_label);
    switch_box.append(&protection);
    header.pack_end(&switch_box);
    header.pack_end(&status);
    header.pack_end(&dev);
    let restart = gtk::Button::with_label("Restart Runtime");
    restart.set_visible(false);
    header.pack_end(&restart);
    let menu = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Application menu")
        .build();
    let menu_popover = gtk::Popover::new();
    let menu_box = vbox(4);
    menu_box.set_margin_start(6);
    menu_box.set_margin_end(6);
    menu_box.set_margin_top(6);
    menu_box.set_margin_bottom(6);
    let quit = gtk::Button::with_label("Quit Private AI Gateway");
    quit.set_has_frame(false);
    menu_box.append(&quit);
    menu_popover.set_child(Some(&menu_box));
    menu.set_popover(Some(&menu_popover));
    header.pack_end(&menu);

    let sidebar = gtk::ListBox::new();
    sidebar.add_css_class("navigation-sidebar");
    sidebar.set_selection_mode(gtk::SelectionMode::Single);
    for (label, icon) in [
        ("Overview", "security-high-symbolic"),
        ("Agents", "utilities-terminal-symbolic"),
        ("Usage", "office-chart-bar-symbolic"),
        ("Settings", "preferences-system-symbolic"),
    ] {
        let row = gtk::ListBoxRow::new();
        let item = gtk::Box::new(Orientation::Horizontal, 12);
        item.set_margin_start(14);
        item.set_margin_end(14);
        item.set_margin_top(10);
        item.set_margin_bottom(10);
        item.append(&gtk::Image::from_icon_name(icon));
        item.append(
            &gtk::Label::builder()
                .label(label)
                .halign(Align::Start)
                .build(),
        );
        row.set_child(Some(&item));
        sidebar.append(&row);
    }
    sidebar.select_row(sidebar.row_at_index(0).as_ref());
    let brand = gtk::Box::new(Orientation::Horizontal, 10);
    brand.set_margin_start(14);
    brand.set_margin_end(14);
    brand.set_margin_top(14);
    brand.set_margin_bottom(14);
    brand.append(&asset_picture("brand/mark.svg", 34));
    let brand_text = vbox(1);
    brand_text.append(
        &gtk::Label::builder()
            .label("Private AI Gateway")
            .css_classes(["heading"])
            .halign(Align::Start)
            .build(),
    );
    brand_text.append(
        &gtk::Label::builder()
            .label("Confidential inference")
            .css_classes(["dim-label", "caption"])
            .halign(Align::Start)
            .build(),
    );
    brand.append(&brand_text);
    let side = gtk::Box::new(Orientation::Vertical, 0);
    side.set_size_request(220, -1);
    side.append(&brand);
    side.append(&sidebar);
    let stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
    let main = vbox(0);
    main.append(&header);
    main.append(&stack);
    let paned = gtk::Paned::new(Orientation::Horizontal);
    paned.set_start_child(Some(&side));
    paned.set_end_child(Some(&main));
    paned.set_resize_start_child(false);
    paned.set_shrink_start_child(false);
    paned.set_position(220);
    let toast = adw::ToastOverlay::new();
    toast.set_child(Some(&paned));
    window.set_content(Some(&toast));

    let (tray_tx, tray_rx) = mpsc::channel();
    let tray = tray::spawn(tray_tx);
    let window_ref = glib::WeakRef::new();
    window_ref.set(Some(&window));
    let ui = Rc::new(Ui {
        window: window_ref,
        toast,
        stack,
        sidebar: sidebar.clone(),
        title,
        status,
        dev,
        protection: protection.clone(),
        restart: restart.clone(),
        syncing: Cell::new(false),
        page: Cell::new(0),
        client: RefCell::new(None),
        generation: Cell::new(0),
        runtime_connected: Cell::new(false),
        runtime_error: RefCell::new(None),
        handshake_remaining: Cell::new(0),
        smoke_pending: Cell::new(false),
        state: RefCell::new(GatewayState::default()),
        agents: RefCell::new(Vec::new()),
        usage: RefCell::new(empty_usage()),
        client_key: RefCell::new(String::new()),
        usage_agent: RefCell::new(None),
        usage_model: RefCell::new(None),
        usage_range: Cell::new(1),
        tray: RefCell::new(tray.ok()),
        tray_available: Cell::new(false),
        views: RefCell::new(None),
        shutting_down: Cell::new(false),
    });
    *ui.views.borrow_mut() = Some(ui.build_views());
    let weak = Rc::downgrade(&ui);
    glib::timeout_add_local(std::time::Duration::from_millis(40), move || {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        for command in tray_rx.try_iter() {
            ui.handle_tray(command);
        }
        glib::ControlFlow::Continue
    });
    let weak = Rc::downgrade(&ui);
    sidebar.connect_row_selected(move |_, row| {
        if let (Some(ui), Some(row)) = (weak.upgrade(), row) {
            ui.page.set(row.index() as usize);
            ui.show_page();
        }
    });
    let weak = Rc::downgrade(&ui);
    protection.connect_active_notify(move |switch| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.syncing.get() {
            return;
        }
        ui.set_protection(switch.is_active());
    });
    let weak = Rc::downgrade(&ui);
    window.connect_close_request(move |window| {
        if let Some(ui) = weak.upgrade() {
            if ui.tray_available.get() {
                window.hide();
            } else {
                ui.quit();
            }
        }
        glib::Propagation::Stop
    });
    let weak = Rc::downgrade(&ui);
    restart.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.restart_runtime();
        }
    });
    let weak = Rc::downgrade(&ui);
    quit.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.quit();
        }
    });
    let owner = ui.clone();
    app.connect_shutdown(move |_| owner.shutdown());
    match runtime {
        Ok(client) => ui.install_client(client),
        Err(error) => ui.disconnected(error),
    }
    ui.render();
    let options = LAUNCH_OPTIONS.get();
    if !options.autostart || !ui.tray_available.get() {
        window.present();
    }
    if options.smoke_test {
        let weak = Rc::downgrade(&ui);
        glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                ui.run_smoke_test();
            }
        });
    }
}

impl Ui {
    fn window(&self) -> adw::ApplicationWindow {
        self.window
            .upgrade()
            .expect("application window should outlive the UI")
    }

    fn install_client(self: &Rc<Self>, client: Rc<RuntimeClient>) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.runtime_connected.set(false);
        self.runtime_error.borrow_mut().take();
        self.handshake_remaining.set(4);
        *self.state.borrow_mut() = GatewayState::default();
        self.agents.borrow_mut().clear();
        *self.usage.borrow_mut() = empty_usage();
        self.client_key.borrow_mut().clear();
        let weak = Rc::downgrade(self);
        client.on_state(move |state| {
            if let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.generation.get() == generation)
            {
                ui.accept_state(state);
            }
        });
        let weak = Rc::downgrade(self);
        client.on_disconnect(move |error| {
            if let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.generation.get() == generation)
            {
                ui.disconnected(error);
            }
        });
        *self.client.borrow_mut() = Some(client);
        self.render();
        self.handshake();
    }

    fn disconnected(self: &Rc<Self>, error: String) {
        self.runtime_connected.set(false);
        self.handshake_remaining.set(0);
        *self.runtime_error.borrow_mut() = Some(error.clone());
        self.render();
        if LAUNCH_OPTIONS.get().smoke_test || self.smoke_pending.get() {
            eprintln!("SMOKE_TEST_FAILED {error}");
            SMOKE_RESULT.set(1);
            self.quit();
        } else {
            self.error(&error);
        }
    }

    fn restart_runtime(self: &Rc<Self>) {
        self.restart.set_sensitive(false);
        self.generation.set(self.generation.get().wrapping_add(1));
        self.runtime_connected.set(false);
        *self.state.borrow_mut() = GatewayState::default();
        self.agents.borrow_mut().clear();
        *self.usage.borrow_mut() = empty_usage();
        self.client_key.borrow_mut().clear();
        self.render();
        if let Some(client) = self.client.borrow_mut().take() {
            client.shutdown_and_wait();
        }
        match RuntimeClient::start() {
            Ok(client) => self.install_client(client),
            Err(error) => self.disconnected(error),
        }
        self.restart.set_sensitive(true);
        self.render();
    }

    fn request<T, P, F>(self: &Rc<Self>, method: &str, params: P, callback: F)
    where
        T: DeserializeOwned + 'static,
        P: Serialize,
        F: FnOnce(&Rc<Self>, Result<T, String>) + 'static,
    {
        let generation = self.generation.get();
        let Some(client) = self.client.borrow().clone() else {
            callback(self, Err("The desktop runtime is disconnected".to_string()));
            return;
        };
        let weak = Rc::downgrade(self);
        client.request(method, params, move |result| {
            if let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.generation.get() == generation)
            {
                callback(&ui, result);
            }
        });
    }

    fn run_smoke_test(self: &Rc<Self>) {
        self.smoke_pending.set(true);
        if self.runtime_connected.get() {
            self.finish_smoke_test();
            return;
        }
        let error = self.runtime_error.borrow().clone();
        if let Some(error) = error {
            self.disconnected(error);
        }
    }

    fn quit(&self) {
        if let Some(application) = self.window().application() {
            application.quit();
        }
    }

    fn shutdown(&self) {
        if self.shutting_down.replace(true) {
            return;
        }
        self.generation.set(self.generation.get().wrapping_add(1));
        if let Some(handle) = self.tray.borrow_mut().take() {
            handle.shutdown().wait();
        }
        if let Some(client) = self.client.borrow_mut().take() {
            client.shutdown_and_wait();
        }
    }

    fn handshake(self: &Rc<Self>) {
        self.request::<GatewayState, _, _>("getState", json!({}), |ui, result| match result {
            Ok(state) => {
                *ui.state.borrow_mut() = state;
                ui.handshake_step();
            }
            Err(error) => ui.disconnected(error),
        });
        self.request::<Vec<AgentStatus>, _, _>(
            "listAgents",
            json!({}),
            |ui, result| match result {
                Ok(agents) => {
                    *ui.agents.borrow_mut() = agents;
                    ui.handshake_step();
                }
                Err(error) => ui.disconnected(error),
            },
        );
        let query = UsageQuery {
            agent: None,
            model: None,
            session_id: None,
            since: Some(unix_now().saturating_sub(30 * 86_400)),
            until: None,
            cursor: None,
            limit: Some(20),
        };
        self.request::<UsagePage, _, _>("queryUsage", json!({ "query": query }), |ui, result| {
            match result {
                Ok(usage) => {
                    *ui.usage.borrow_mut() = usage;
                    ui.handshake_step();
                }
                Err(error) => ui.disconnected(error),
            }
        });
        self.request::<String, _, _>("getClientKey", json!({}), |ui, result| match result {
            Ok(key) => {
                *ui.client_key.borrow_mut() = key;
                ui.handshake_step();
            }
            Err(error) => ui.disconnected(error),
        });
    }

    fn handshake_step(self: &Rc<Self>) {
        let remaining = self.handshake_remaining.get();
        if remaining == 0 {
            return;
        }
        self.handshake_remaining.set(remaining - 1);
        if remaining == 1 {
            let process_running = self
                .client
                .borrow()
                .as_ref()
                .is_some_and(|client| client.is_process_running());
            if !process_running {
                self.disconnected("The desktop runtime exited during startup".to_string());
                return;
            }
            self.runtime_connected.set(true);
            self.render();
            if self.smoke_pending.get() {
                self.finish_smoke_test();
            }
        }
    }

    fn finish_smoke_test(&self) {
        println!("SMOKE_TEST_OK status={}", self.state.borrow().status);
        SMOKE_RESULT.set(0);
        self.quit();
    }

    fn accept_state(self: &Rc<Self>, state: GatewayState) {
        let usage_changed = state.usage_revision != self.state.borrow().usage_revision;
        *self.state.borrow_mut() = state;
        self.render();
        if usage_changed {
            self.reload_usage(true);
        }
    }

    fn set_protection(self: &Rc<Self>, enabled: bool) {
        let state = self.state.borrow();
        if enabled && (!state.api_key_saved || state.profiles.is_empty()) {
            drop(state);
            self.syncing.set(true);
            self.protection.set_active(false);
            self.syncing.set(false);
            self.show_profiles();
            return;
        }
        let params = if enabled {
            json!({ "config": state.config })
        } else {
            json!({})
        };
        drop(state);
        self.request::<GatewayState, _, _>(
            if enabled { "start" } else { "stop" },
            params,
            |ui, result| match result {
                Ok(state) => ui.accept_state(state),
                Err(error) => {
                    ui.error(&error);
                    ui.render();
                }
            },
        );
    }

    fn set_agent(self: &Rc<Self>, agent: AgentStatus, connect: bool) {
        let default_model = if agent.id == "codex" {
            self.state
                .borrow()
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.models.first())
                .map(|model| model.id.clone())
        } else {
            None
        };
        let options = ConnectOptions { default_model };
        let id = agent.id.clone();
        self.request::<AgentPreview, _, _>(
            "previewAgent",
            json!({ "agentId": id, "connect": connect, "options": options }),
            move |ui, result| match result {
                Ok(preview) => ui.request::<AgentStatus, _, _>(
                    "applyAgent",
                    json!({
                        "agentId": preview.agent.id,
                        "connect": connect,
                        "revision": preview.revision,
                        "options": options
                    }),
                    |ui, result| match result {
                        Ok(_) => ui.reload_agents(),
                        Err(error) => ui.error(&error),
                    },
                ),
                Err(error) => ui.error(&error),
            },
        );
    }
    fn reload_agents(self: &Rc<Self>) {
        self.request::<Vec<AgentStatus>, _, _>(
            "listAgents",
            json!({}),
            |ui, result| match result {
                Ok(agents) => {
                    *ui.agents.borrow_mut() = agents;
                    ui.render();
                }
                Err(error) => ui.error(&error),
            },
        );
    }
    fn reload_usage(self: &Rc<Self>, reset: bool) {
        let usage = self.usage.borrow();
        let cursor = (!reset).then(|| usage.next_cursor.clone()).flatten();
        drop(usage);
        let now = unix_now();
        let since = match self.usage_range.get() {
            0 => Some(now.saturating_sub(7 * 86_400)),
            1 => Some(now.saturating_sub(30 * 86_400)),
            _ => None,
        };
        let query = UsageQuery {
            agent: self.usage_agent.borrow().clone(),
            model: self.usage_model.borrow().clone(),
            session_id: None,
            since,
            until: None,
            cursor,
            limit: Some(20),
        };
        self.request::<UsagePage, _, _>(
            "queryUsage",
            json!({ "query": query }),
            move |ui, result| match result {
                Ok(mut page) => {
                    if !reset {
                        let mut items = ui.usage.borrow().items.clone();
                        items.append(&mut page.items);
                        page.items = items;
                    }
                    *ui.usage.borrow_mut() = page;
                    ui.render();
                }
                Err(error) => ui.error(&error),
            },
        );
    }

    fn handle_tray(self: &Rc<Self>, command: TrayCommand) {
        match command {
            TrayCommand::Toggle => {
                let enabled = !is_running(&self.state.borrow());
                self.set_protection(enabled);
            }
            TrayCommand::Open => self.window().present(),
            TrayCommand::Settings => {
                self.page.set(3);
                self.sidebar
                    .select_row(self.sidebar.row_at_index(3).as_ref());
                self.window().present();
            }
            TrayCommand::RestartRuntime => self.restart_runtime(),
            TrayCommand::OpenAtLogin => match tray::set_open_at_login(!tray::open_at_login()) {
                Ok(()) => self.render(),
                Err(error) => self.error(&error),
            },
            TrayCommand::Quit => self.quit(),
            TrayCommand::Availability(available) => {
                self.tray_available.set(available);
                if !available {
                    self.window().present();
                }
            }
        }
    }
    fn error(&self, message: &str) {
        self.toast.add_toast(adw::Toast::new(message));
    }
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(".navigation-sidebar { background: alpha(@window_fg_color, .035); } .card { background: alpha(@window_fg_color, .035); border: 1px solid alpha(@window_fg_color, .10); border-radius: 8px; padding: 14px; } .dev-badge { color: #9a6700; background: alpha(#e9a400, .16); border-radius: 5px; padding: 4px 8px; font-weight: 600; } .success { color: #2c6e49; } .monospace { font-family: monospace; } .caption { font-size: 0.88em; }");
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
fn empty_usage() -> UsagePage {
    UsagePage {
        items: Vec::new(),
        next_cursor: None,
        summary: UsageSummary::default(),
        series: Vec::new(),
        agents: Vec::new(),
        models: Vec::new(),
    }
}
fn active_profile(state: &GatewayState) -> Option<&ConfidentialProfile> {
    state
        .profiles
        .iter()
        .find(|profile| profile.id == state.active_profile_id)
}
fn is_running(state: &GatewayState) -> bool {
    matches!(state.status.as_str(), "verifying" | "verified" | "blocked")
}
fn is_protected(state: &GatewayState) -> bool {
    state.status == "verified" && !state.configuration_verification
}
fn status_label(state: &GatewayState) -> &'static str {
    if is_protected(state) {
        return "Protected";
    }
    match state.status.as_str() {
        "verifying" if state.configuration_verification => "Verifying configuration",
        "verifying" => "Starting",
        "verified" => "Verifying configuration",
        "blocked" => "Blocked",
        "error" => "Needs attention",
        _ => "Not protected",
    }
}
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
