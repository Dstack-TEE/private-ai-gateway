use std::{
    cell::{Cell, RefCell},
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
use serde_json::json;

use crate::{
    runtime_client::RuntimeClient,
    tray::{self, TrayCommand},
};

pub fn run() {
    adw::init().expect("libadwaita could not be initialized");
    let app = adw::Application::builder()
        .application_id("org.dstack.private-ai-gateway")
        .flags(gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_startup(|_| install_css());
    app.connect_activate(build_app);
    app.connect_command_line(|app, command| {
        app.activate();
        if command
            .arguments()
            .iter()
            .any(|value| value == "--autostart")
        {
            if let Some(window) = app.active_window() {
                window.hide();
            }
        }
        0.into()
    });
    app.run();
}

struct Ui {
    app: adw::Application,
    window: adw::ApplicationWindow,
    toast: adw::ToastOverlay,
    content: gtk::Box,
    title: gtk::Label,
    status: gtk::Label,
    dev: gtk::Label,
    header_controls: gtk::Box,
    protection: gtk::Switch,
    sidebar: gtk::ListBox,
    syncing: Cell<bool>,
    page: Cell<usize>,
    client: Rc<RuntimeClient>,
    state: RefCell<GatewayState>,
    agents: RefCell<Vec<AgentStatus>>,
    usage: RefCell<UsagePage>,
    client_key: RefCell<String>,
    usage_agent: RefCell<Option<String>>,
    usage_model: RefCell<Option<String>>,
    usage_range: Cell<usize>,
    tray: RefCell<Option<ksni::blocking::Handle<tray::GatewayTray>>>,
}

fn build_app(app: &adw::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let client = match RuntimeClient::start() {
        Ok(client) => client,
        Err(error) => {
            show_startup_error(app, &error);
            return;
        }
    };
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
    let protected_label = gtk::Label::new(Some("Protected"));
    let protection = gtk::Switch::new();
    let switch_box = gtk::Box::new(Orientation::Horizontal, 8);
    switch_box.append(&dev);
    switch_box.append(&status);
    switch_box.append(&protected_label);
    switch_box.append(&protection);
    header.pack_end(&switch_box);

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
    let content = vbox(0);
    let main = vbox(0);
    main.append(&header);
    main.append(&content);
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
    let ui = Rc::new(Ui {
        app: app.clone(),
        window: window.clone(),
        toast,
        content,
        title,
        status,
        dev,
        header_controls: switch_box,
        protection: protection.clone(),
        sidebar: sidebar.clone(),
        syncing: Cell::new(false),
        page: Cell::new(0),
        client: client.clone(),
        state: RefCell::new(GatewayState::default()),
        agents: RefCell::new(Vec::new()),
        usage: RefCell::new(empty_usage()),
        client_key: RefCell::new(String::new()),
        usage_agent: RefCell::new(None),
        usage_model: RefCell::new(None),
        usage_range: Cell::new(1),
        tray: RefCell::new(tray::spawn(tray_tx)),
    });
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
    client.on_state(move |state| {
        if let Some(ui) = weak.upgrade() {
            ui.accept_state(state);
        }
    });
    let weak = Rc::downgrade(&ui);
    client.on_error(move |error| {
        if let Some(ui) = weak.upgrade() {
            ui.error(&error);
        }
    });
    let weak = Rc::downgrade(&ui);
    sidebar.connect_row_selected(move |_, row| {
        if let (Some(ui), Some(row)) = (weak.upgrade(), row) {
            ui.page.set(row.index() as usize);
            ui.render();
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
        if weak.upgrade().is_some() {
            window.hide();
        }
        glib::Propagation::Stop
    });
    ui.reload();
    ui.render();
    if !std::env::args().any(|argument| argument == "--autostart") {
        window.present();
    }
}

impl Ui {
    fn reload(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.client
            .request::<GatewayState>("getState", json!({}), move |result| {
                if let Some(ui) = weak.upgrade() {
                    match result {
                        Ok(state) => ui.accept_state(state),
                        Err(error) => ui.error(&error),
                    }
                }
            });
        self.reload_agents();
        self.reload_usage(true);
        let weak = Rc::downgrade(self);
        self.client
            .request::<String>("getClientKey", json!({}), move |result| {
                if let (Some(ui), Ok(key)) = (weak.upgrade(), result) {
                    *ui.client_key.borrow_mut() = key;
                    ui.render();
                }
            });
    }

    fn accept_state(self: &Rc<Self>, state: GatewayState) {
        let usage_changed = state.usage_revision != self.state.borrow().usage_revision;
        *self.state.borrow_mut() = state;
        self.render();
        if usage_changed {
            self.reload_usage(true);
        }
    }

    fn render(self: &Rc<Self>) {
        let state = self.state.borrow();
        self.syncing.set(true);
        self.protection.set_active(is_running(&state));
        self.protection.set_sensitive(true);
        self.syncing.set(false);
        self.status.set_label(status_label(&state));
        self.dev
            .set_visible(is_running(&state) && !state.config.require_production_os);
        self.header_controls.set_visible(self.page.get() != 0);
        let title = ["Overview", "Agents", "Usage", "Settings"][self.page.get().min(3)];
        self.title.set_label(title);
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        let page = match self.page.get() {
            1 => self.agents_page(),
            2 => self.usage_page(),
            3 => self.settings_page(),
            _ => self.overview_page(),
        };
        self.content.append(&page);
        if let Some(handle) = self.tray.borrow().as_ref() {
            let running = is_running(&state);
            let protected = state.status == "verified" && !state.configuration_verification;
            let label = status_label(&state).to_string();
            let open = tray::open_at_login();
            handle.update(move |tray| {
                tray.running = running;
                tray.protected = protected;
                tray.status = label;
                tray.open_at_login = open;
            });
        }
    }

    fn overview_page(self: &Rc<Self>) -> gtk::Widget {
        let root = page_box();
        let state = self.state.borrow();
        root.append(&self.status_surface());
        let columns = gtk::Grid::builder()
            .column_spacing(20)
            .row_spacing(42)
            .column_homogeneous(true)
            .build();
        columns.attach(
            &overview_module("Local API", None, &self.local_api_card(), 136),
            0,
            0,
            1,
            1,
        );
        columns.attach(
            &overview_module(
                "Session usage",
                Some("This session"),
                &session_metrics(&state.session_usage),
                136,
            ),
            1,
            0,
            1,
            1,
        );
        let agents = self.agents_card();
        let agents_module = overview_module_with_action("Agents", "View all", &agents, 320);
        let weak = Rc::downgrade(self);
        agents_module.1.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.navigate(1);
            }
        });
        columns.attach(&agents_module.0, 0, 1, 1, 1);
        let recent = vbox(0);
        if state.activity.is_empty() {
            recent.append(&empty(if is_running(&state) {
                "No requests in this session yet."
            } else {
                "Start protection to begin a new session."
            }));
        } else {
            for item in state.activity.iter().take(5) {
                recent.append(&self.usage_row(item.clone()));
            }
        }
        let usage_module = overview_module_with_action("Recent usage", "View all", &recent, 320);
        let weak = Rc::downgrade(self);
        usage_module.1.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.navigate(2);
            }
        });
        columns.attach(&usage_module.0, 1, 1, 1, 1);
        root.append(&columns);
        scrolled(&root).upcast()
    }

    fn status_surface(self: &Rc<Self>) -> gtk::Widget {
        let state = self.state.borrow();
        let ready = state.status == "verified"
            && !state.configuration_verification
            && state.api_key_saved;
        let overlay = gtk::Overlay::new();
        let tracks = gtk::Grid::builder()
            .column_homogeneous(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        if ready {
            tracks.attach(&track_layer(&PLAINTEXT_TRACKS, false, "track-plain"), 0, 0, 1, 1);
            tracks.attach(&track_layer(&TLS_TRACKS, true, "track-cipher"), 2, 0, 1, 1);
        }
        overlay.set_child(Some(&tracks));

        let foreground = gtk::Grid::builder()
            .column_homogeneous(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        let local = vbox(7);
        local.set_margin_start(16);
        local.set_margin_end(16);
        local.set_margin_top(16);
        local.set_margin_bottom(16);
        let local_title = hbox(8);
        local_title.append(&gtk::Image::from_icon_name("computer-symbolic"));
        local_title.append(
            &gtk::Label::builder()
                .label("This PC")
                .css_classes(["heading"])
                .build(),
        );
        local.append(&local_title);
        let agents = self.agents.borrow();
        local.append(
            &gtk::Label::builder()
                .label(format!(
                    "{} enabled · {} active",
                    agents.iter().filter(|agent| agent.recorded).count(),
                    agents.iter().filter(|agent| agent.connected).count()
                ))
                .css_classes(["dim-label", "caption"])
                .halign(Align::Start)
                .build(),
        );
        let icons = hbox(6);
        for agent in agents.iter().filter(|agent| agent.recorded).take(5) {
            let icon = gtk::Box::new(Orientation::Horizontal, 0);
            icon.add_css_class("agent-icon");
            icon.append(&asset_picture(&format!("agents/{}.svg", agent.id), 15));
            icons.append(&icon);
        }
        local.append(&icons);
        local.append(
            &gtk::Label::builder()
                .label("Enabled agents send their requests to the gateway on this PC.")
                .css_classes(["dim-label", "caption"])
                .wrap(true)
                .xalign(0.0)
                .max_width_chars(28)
                .halign(Align::Start)
                .build(),
        );
        drop(agents);
        foreground.attach(&local, 0, 0, 1, 1);

        let gateway = vbox(4);
        gateway.set_halign(Align::Center);
        gateway.set_valign(Align::Center);
        gateway.append(&asset_picture("brand/mark.svg", 44));
        gateway.append(
            &gtk::Label::builder()
                .label("Private AI Gateway")
                .css_classes(["heading"])
                .build(),
        );
        let verdict = gtk::Label::new(Some(status_label(&state)));
        verdict.add_css_class(if is_running(&state) && !state.config.require_production_os {
            "warning"
        } else if state.status == "verified" {
            "success"
        } else {
            "dim-label"
        });
        gateway.append(&verdict);
        let protection = gtk::Switch::builder()
            .active(is_running(&state))
            .sensitive(state.endpoint_error.is_none() || is_running(&state))
            .halign(Align::Center)
            .build();
        let original = is_running(&state);
        let weak = Rc::downgrade(self);
        protection.connect_active_notify(move |toggle| {
            if toggle.is_active() != original {
                if let Some(ui) = weak.upgrade() {
                    ui.set_protection(toggle.is_active());
                }
            }
        });
        gateway.append(&protection);
        foreground.attach(&gateway, 1, 0, 1, 1);

        let remote = vbox(6);
        remote.set_margin_start(16);
        remote.set_margin_end(16);
        remote.set_margin_top(16);
        remote.set_margin_bottom(16);
        remote.set_halign(Align::Fill);
        let profile = active_profile(&state);
        let service = hbox(8);
        service.set_halign(Align::End);
        service.append(
            &gtk::Label::builder()
                .label(profile.map_or("Custom service", |entry| entry.name.as_str()))
                .css_classes(["heading"])
                .build(),
        );
        let provider_asset = match profile.map(|entry| &entry.provider) {
            Some(ServiceProvider::Phala) => "providers/phala.svg",
            Some(ServiceProvider::Redpill) => "providers/redpill.png",
            _ => "brand/mark.svg",
        };
        service.append(&asset_picture(provider_asset, 24));
        remote.append(&service);
        remote.append(
            &gtk::Label::builder()
                .label(service_host(
                    state.remote_url.as_deref().unwrap_or(&state.config.remote_url),
                ))
                .css_classes(["dim-label", "caption", "monospace"])
                .halign(Align::End)
                .build(),
        );
        for text in if state.status == "verified" {
            vec![
                "Verified hardware ✓".to_string(),
                format!("{} answers this session ✓", state.session_usage.protected),
            ]
        } else {
            vec!["Not verified •".into(), "No answers this session •".into()]
        } {
            remote.append(
                &gtk::Label::builder()
                    .label(text)
                    .css_classes([if state.status == "verified" { "success" } else { "dim-label" }, "caption"])
                    .halign(Align::End)
                    .build(),
            );
        }
        let actions = hbox(6);
        actions.set_halign(Align::End);
        let profiles = gtk::Button::builder()
            .icon_name("preferences-system-symbolic")
            .tooltip_text("Profiles")
            .build();
        let weak = Rc::downgrade(self);
        profiles.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_profiles();
            }
        });
        actions.append(&profiles);
        let privacy = gtk::Button::builder()
            .icon_name("security-high-symbolic")
            .tooltip_text("Privacy verification")
            .sensitive(state.identity.is_some())
            .build();
        let weak = Rc::downgrade(self);
        privacy.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_privacy();
            }
        });
        actions.append(&privacy);
        remote.append(&actions);
        foreground.attach(&remote, 2, 0, 1, 1);
        overlay.add_overlay(&foreground);
        let surface = card(&overlay);
        surface.add_css_class("status-surface");
        surface.set_height_request(184);
        surface.upcast()
    }

    fn navigate(self: &Rc<Self>, index: i32) {
        if let Some(row) = self.sidebar.row_at_index(index) {
            self.sidebar.select_row(Some(&row));
        }
    }

    fn local_api_card(self: &Rc<Self>) -> gtk::Widget {
        let state = self.state.borrow();
        let root = vbox(0);
        let endpoint = gtk::Overlay::new();
        endpoint.set_child(Some(&copy_row(
            "Endpoint",
            state.proxy_url.as_deref().unwrap_or("Unavailable"),
            state.proxy_url.clone(),
        )));
        let settings = gtk::Button::builder()
            .icon_name("preferences-system-symbolic")
            .tooltip_text("Local API settings")
            .halign(Align::End)
            .valign(Align::Center)
            .margin_end(12)
            .build();
        let weak = Rc::downgrade(self);
        settings.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_local_api();
            }
        });
        endpoint.add_overlay(&settings);
        root.append(&endpoint);
        let key = self.client_key.borrow().clone();
        let row = hbox(8);
        let copy = gtk::Button::new();
        copy.set_hexpand(true);
        copy.set_has_frame(false);
        let value = gtk::Label::builder()
            .label("pag_••••••••••••")
            .halign(Align::Start)
            .css_classes(["monospace"])
            .build();
        copy.set_child(Some(&labeled("Client key", &value)));
        let key_copy = key.clone();
        copy.connect_clicked(move |_| clipboard(&key_copy));
        row.append(&copy);
        let eye = gtk::ToggleButton::builder()
            .icon_name("view-reveal-symbolic")
            .tooltip_text("Reveal client key")
            .build();
        let masked = value.clone();
        eye.connect_toggled(move |button| {
            masked.set_label(if button.is_active() {
                &key
            } else {
                "pag_••••••••••••"
            });
            button.set_icon_name(if button.is_active() {
                "view-conceal-symbolic"
            } else {
                "view-reveal-symbolic"
            });
        });
        row.append(&eye);
        root.append(&row);
        root.set_height_request(136);
        root.upcast()
    }

    fn agents_card(self: &Rc<Self>) -> gtk::Widget {
        let root = vbox(0);
        for agent in self.agents.borrow().iter().take(5) {
            root.append(&self.agent_row(agent.clone()));
        }
        root.upcast()
    }
    fn agents_page(self: &Rc<Self>) -> gtk::Widget {
        let root = page_box();
        let group = vbox(0);
        for agent in self.agents.borrow().iter() {
            group.append(&self.agent_row(agent.clone()));
        }
        root.append(&card(&group));
        scrolled(&root).upcast()
    }

    fn agent_row(self: &Rc<Self>, agent: AgentStatus) -> gtk::Widget {
        let row = adw::ActionRow::builder()
            .title(&agent.name)
            .subtitle(
                agent
                    .error
                    .as_deref()
                    .or(agent.attention.as_deref())
                    .unwrap_or(if agent.installed {
                        &agent.config_path
                    } else {
                        "CLI not found"
                    }),
            )
            .build();
        row.add_prefix(&asset_picture(&format!("agents/{}.svg", agent.id), 28));
        let toggle = gtk::Switch::builder()
            .active(agent.connected)
            .valign(Align::Center)
            .sensitive(agent.installed || agent.recorded)
            .build();
        let original = agent.connected;
        let weak = Rc::downgrade(self);
        toggle.connect_active_notify(move |toggle| {
            if toggle.is_active() != original {
                if let Some(ui) = weak.upgrade() {
                    ui.set_agent(agent.clone(), toggle.is_active());
                }
            }
        });
        row.add_suffix(&toggle);
        row.set_activatable_widget(Some(&toggle));
        row.upcast()
    }

    fn usage_page(self: &Rc<Self>) -> gtk::Widget {
        let root = page_box();
        let usage = self.usage.borrow();
        let filters = hbox(10);
        filters.append(&self.filter_combo(
            "Agent",
            "All agents",
            &usage.agents,
            self.usage_agent.borrow().clone(),
            true,
        ));
        filters.append(&self.filter_combo(
            "Model",
            "All models",
            &usage.models,
            self.usage_model.borrow().clone(),
            false,
        ));
        let range = gtk::ComboBoxText::new();
        for label in ["7 days", "30 days", "All time"] {
            range.append_text(label);
        }
        range.set_active(Some(self.usage_range.get() as u32));
        let weak = Rc::downgrade(self);
        range.connect_changed(move |combo| {
            if let Some(ui) = weak.upgrade() {
                ui.usage_range.set(combo.active().unwrap_or(1) as usize);
                ui.reload_usage(true);
            }
        });
        filters.append(&range);
        let spacer = gtk::Box::new(Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        filters.append(&spacer);
        let export = gtk::Button::with_label("Export CSV…");
        let weak = Rc::downgrade(self);
        export.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.export_usage();
            }
        });
        filters.append(&export);
        let clear = gtk::Button::with_label("Clear History…");
        clear.add_css_class("destructive-action");
        let weak = Rc::downgrade(self);
        clear.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.confirm_clear_usage();
            }
        });
        filters.append(&clear);
        root.append(&filters);
        root.append(&metrics(&usage.summary, None));
        root.append(&usage_chart(&usage.series));
        let list = vbox(0);
        list.append(&section_title("Usage history"));
        if usage.items.is_empty() {
            list.append(&empty("No usage matches these filters"));
        } else {
            for item in &usage.items {
                list.append(&self.usage_row(item.clone()));
            }
        }
        root.append(&card(&list));
        if usage.next_cursor.is_some() {
            let more = gtk::Button::with_label("Load More");
            more.set_halign(Align::Center);
            let weak = Rc::downgrade(self);
            more.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.reload_usage(false);
                }
            });
            root.append(&more);
        }
        scrolled(&root).upcast()
    }

    fn settings_page(self: &Rc<Self>) -> gtk::Widget {
        let root = page_box();
        let state = self.state.borrow();
        let profiles = adw::PreferencesGroup::builder()
            .title("Confidential AI")
            .build();
        let row = adw::ActionRow::builder()
            .title("Profile")
            .subtitle(
                active_profile(&state)
                    .map(|profile| profile.name.as_str())
                    .unwrap_or("Not configured"),
            )
            .activatable(true)
            .build();
        let weak = Rc::downgrade(self);
        row.connect_activated(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_profiles();
            }
        });
        profiles.add(&row);
        root.append(&profiles);
        let local = adw::PreferencesGroup::builder().title("Local API").build();
        let settings = adw::ActionRow::builder()
            .title("Local API Settings")
            .activatable(true)
            .build();
        settings.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = Rc::downgrade(self);
        settings.connect_activated(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_local_api();
            }
        });
        local.add(&settings);
        let rotate = adw::ActionRow::builder()
            .title("Rotate Client Key")
            .activatable(true)
            .build();
        let weak = Rc::downgrade(self);
        rotate.connect_activated(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.rotate_client_key();
            }
        });
        local.add(&rotate);
        root.append(&local);
        let advanced = adw::ExpanderRow::builder().title("Advanced").build();
        let policy = adw::ActionRow::builder()
            .title("OS policy")
            .subtitle(if state.config.require_production_os {
                "Production OS required"
            } else {
                "Development OS allowed"
            })
            .build();
        advanced.add_row(&policy);
        let restore = adw::ActionRow::builder()
            .title("Restore All Agent Configurations")
            .activatable(true)
            .build();
        restore.add_css_class("error");
        let weak = Rc::downgrade(self);
        restore.connect_activated(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.confirm_restore();
            }
        });
        advanced.add_row(&restore);
        let group = adw::PreferencesGroup::new();
        group.add(&advanced);
        root.append(&group);
        scrolled(&root).upcast()
    }

    fn usage_row(self: &Rc<Self>, item: RequestActivity) -> gtk::Widget {
        let row = adw::ActionRow::builder()
            .title(item.model.as_deref().unwrap_or(&item.path))
            .subtitle(format!(
                "{} · {}",
                item.agent.as_deref().unwrap_or("Unknown"),
                item.path
            ))
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(
            if item.verified == Some(true) {
                "security-high-symbolic"
            } else if item.left_device {
                "dialog-warning-symbolic"
            } else {
                "action-unavailable-symbolic"
            },
        ));
        let tokens = item.input_tokens.unwrap_or(0) + item.output_tokens.unwrap_or(0);
        row.add_suffix(
            &gtk::Label::builder()
                .label(format_number(tokens))
                .css_classes(["dim-label", "monospace"])
                .build(),
        );
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = Rc::downgrade(self);
        row.connect_activated(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_proof(item.clone());
            }
        });
        row.upcast()
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
            serde_json::to_value(json!({ "config": state.config })).unwrap()
        } else {
            json!({})
        };
        drop(state);
        let weak = Rc::downgrade(self);
        self.client.request::<GatewayState>(
            if enabled { "start" } else { "stop" },
            params,
            move |result| {
                if let Some(ui) = weak.upgrade() {
                    match result {
                        Ok(state) => ui.accept_state(state),
                        Err(error) => {
                            ui.error(&error);
                            ui.render();
                        }
                    }
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
        let weak = Rc::downgrade(self);
        let id = agent.id.clone();
        self.client.request::<AgentPreview>("previewAgent", json!({ "agentId": id, "connect": connect, "options": options }), move |result| { let Some(ui) = weak.upgrade() else { return; }; match result { Ok(preview) => { let weak = Rc::downgrade(&ui); ui.client.request::<AgentStatus>("applyAgent", json!({ "agentId": preview.agent.id, "connect": connect, "revision": preview.revision, "options": options }), move |result| if let Some(ui) = weak.upgrade() { match result { Ok(_) => ui.reload_agents(), Err(error) => ui.error(&error) } }); }, Err(error) => ui.error(&error) } });
    }
    fn reload_agents(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.client
            .request::<Vec<AgentStatus>>("listAgents", json!({}), move |result| {
                if let Some(ui) = weak.upgrade() {
                    match result {
                        Ok(agents) => {
                            *ui.agents.borrow_mut() = agents;
                            ui.render();
                        }
                        Err(error) => ui.error(&error),
                    }
                }
            });
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
        let weak = Rc::downgrade(self);
        self.client
            .request::<UsagePage>("queryUsage", json!({ "query": query }), move |result| {
                if let Some(ui) = weak.upgrade() {
                    match result {
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
                    }
                }
            });
    }
    fn filter_combo(
        self: &Rc<Self>,
        tooltip: &str,
        all: &str,
        values: &[String],
        selected: Option<String>,
        agent: bool,
    ) -> gtk::ComboBoxText {
        let combo = gtk::ComboBoxText::new();
        combo.set_tooltip_text(Some(tooltip));
        combo.append_text(all);
        for value in values {
            combo.append_text(value);
        }
        combo.set_active(Some(
            selected
                .as_ref()
                .and_then(|selected| values.iter().position(|value| value == selected))
                .map_or(0, |index| index + 1) as u32,
        ));
        let weak = Rc::downgrade(self);
        combo.connect_changed(move |combo| {
            if let Some(ui) = weak.upgrade() {
                let value = combo
                    .active_text()
                    .and_then(|value| (combo.active() != Some(0)).then(|| value.to_string()));
                if agent {
                    *ui.usage_agent.borrow_mut() = value;
                } else {
                    *ui.usage_model.borrow_mut() = value;
                }
                ui.reload_usage(true);
            }
        });
        combo
    }

    fn show_profiles(self: &Rc<Self>) {
        if self.state.borrow().profiles.is_empty() {
            self.show_profile_editor(None);
            return;
        }
        let dialog = dialog(&self.window, "Profiles", 620, 500);
        let body = vbox(10);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        let profiles = Rc::new(self.state.borrow().profiles.clone());
        for profile in profiles.iter() {
            let row = adw::ActionRow::builder()
                .title(&profile.name)
                .subtitle(&profile.remote_url)
                .build();
            list.append(&row);
        }
        body.append(&list);
        let buttons = hbox(8);
        let new = gtk::Button::with_label("New");
        let edit = gtk::Button::with_label("Edit");
        let delete = gtk::Button::with_label("Delete");
        delete.add_css_class("destructive-action");
        let use_profile = gtk::Button::with_label("Use Profile");
        buttons.append(&new);
        buttons.append(&edit);
        buttons.append(&delete);
        let spacer = gtk::Box::new(Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        buttons.append(&spacer);
        buttons.append(&use_profile);
        body.append(&buttons);
        dialog.content_area().append(&body);
        dialog.add_button("Done", gtk::ResponseType::Close);
        let weak = Rc::downgrade(self);
        new.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_profile_editor(None);
            }
        });
        let weak = Rc::downgrade(self);
        let list_edit = list.clone();
        let profiles_edit = profiles.clone();
        edit.connect_clicked(move |_| {
            if let (Some(ui), Some(profile)) =
                (weak.upgrade(), selected_profile(&list_edit, &profiles_edit))
            {
                ui.show_profile_editor(Some(profile));
            }
        });
        let weak = Rc::downgrade(self);
        let list_delete = list.clone();
        let profiles_delete = profiles.clone();
        delete.connect_clicked(move |_| {
            if let (Some(ui), Some(profile)) = (
                weak.upgrade(),
                selected_profile(&list_delete, &profiles_delete),
            ) {
                ui.confirm_delete_profile(profile);
            }
        });
        let weak = Rc::downgrade(self);
        let profiles_use = profiles.clone();
        use_profile.connect_clicked(move |_| {
            if let (Some(ui), Some(profile)) =
                (weak.upgrade(), selected_profile(&list, &profiles_use))
            {
                let weak = Rc::downgrade(&ui);
                ui.client.request::<GatewayState>(
                    "activateProfile",
                    json!({ "profileId": profile.id }),
                    move |result| {
                        if let Some(ui) = weak.upgrade() {
                            match result {
                                Ok(state) => ui.accept_state(state),
                                Err(error) => ui.error(&error),
                            }
                        }
                    },
                );
            }
        });
        dialog.connect_response(|dialog, _| dialog.close());
        dialog.present();
    }

    fn show_profile_editor(self: &Rc<Self>, profile: Option<ConfidentialProfile>) {
        let dialog = dialog(
            &self.window,
            if profile.is_some() {
                "Edit Profile"
            } else {
                "New Profile"
            },
            620,
            480,
        );
        let body = vbox(12);
        let name = gtk::Entry::new();
        name.set_placeholder_text(Some("Name"));
        name.set_text(
            profile
                .as_ref()
                .map(|profile| profile.name.as_str())
                .unwrap_or("RedPill"),
        );
        let provider = gtk::ComboBoxText::new();
        for label in ["Phala", "RedPill", "Custom"] {
            provider.append_text(label);
        }
        provider.set_active(Some(
            match profile.as_ref().map(|profile| &profile.provider) {
                Some(ServiceProvider::Phala) => 0,
                Some(ServiceProvider::Custom) => 2,
                _ => 1,
            },
        ));
        let endpoint = gtk::Entry::new();
        endpoint.set_placeholder_text(Some("Endpoint"));
        endpoint.set_text(
            profile
                .as_ref()
                .map(|profile| profile.remote_url.as_str())
                .unwrap_or("https://tee.redpill.ai"),
        );
        endpoint.set_sensitive(provider.active() == Some(2));
        let key = gtk::PasswordEntry::new();
        key.set_placeholder_text(Some(
            if profile
                .as_ref()
                .and_then(|profile| profile.verified_at)
                .is_some()
            {
                "API key (leave blank to keep)"
            } else {
                "API key"
            },
        ));
        key.set_show_peek_icon(true);
        let allow_dev = gtk::Switch::builder()
            .active(!self.state.borrow().config.require_production_os)
            .build();
        let dev_row = adw::ActionRow::builder()
            .title("Allow development OS")
            .subtitle(
                "Weakens the production attestation policy and is shown in yellow while running.",
            )
            .build();
        dev_row.add_suffix(&allow_dev);
        dev_row.set_activatable_widget(Some(&allow_dev));
        body.append(&name);
        body.append(&provider);
        body.append(&endpoint);
        body.append(&key);
        let verify = gtk::Button::with_label("Verify and Save");
        verify.add_css_class("suggested-action");
        verify.set_halign(Align::End);
        body.append(&verify);
        if profile
            .as_ref()
            .and_then(|profile| profile.verified_at)
            .is_some()
        {
            body.append(
                &gtk::Label::builder()
                    .label("✓ Verified configuration")
                    .css_classes(["success"])
                    .halign(Align::Start)
                    .build(),
            );
        }
        body.append(&dev_row);
        dialog.content_area().append(&body);
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        let endpoint_change = endpoint.clone();
        let name_change = name.clone();
        let is_new = profile.is_none();
        provider.connect_changed(move |provider| {
            match provider.active() {
                Some(0) => {
                    endpoint_change.set_text("https://inference.phala.com");
                    if is_new {
                        name_change.set_text("Phala");
                    }
                }
                Some(1) => {
                    endpoint_change.set_text("https://tee.redpill.ai");
                    if is_new {
                        name_change.set_text("RedPill");
                    }
                }
                _ => {
                    if is_new {
                        endpoint_change.set_text("");
                        name_change.set_text("Custom");
                    }
                }
            }
            endpoint_change.set_sensitive(provider.active() == Some(2));
        });
        let weak = Rc::downgrade(self);
        let profile_id = profile.as_ref().map(|profile| profile.id.clone());
        let verify_dialog = dialog.clone();
        verify.connect_clicked(move |button| {
            let Some(ui) = weak.upgrade() else { return };
            button.set_sensitive(false);
            let input = ConfidentialProfileInput {
                id: profile_id
                    .clone()
                    .unwrap_or_else(|| format!("profile-{}", unix_now())),
                name: name.text().to_string(),
                provider: match provider.active() {
                    Some(0) => ServiceProvider::Phala,
                    Some(2) => ServiceProvider::Custom,
                    _ => ServiceProvider::Redpill,
                },
                remote_url: endpoint.text().to_string(),
            };
            let params = json!({
                "profile": input,
                "requireProductionOs": !allow_dev.is_active(),
                "key": (!key.text().is_empty()).then(|| key.text().to_string())
            });
            let weak = Rc::downgrade(&ui);
            let dialog = verify_dialog.clone();
            let button = button.clone();
            ui.client
                .request::<GatewayState>("verifyConfiguration", params, move |result| {
                    if let Some(ui) = weak.upgrade() {
                        button.set_sensitive(true);
                        match result {
                            Ok(state) => {
                                ui.accept_state(state);
                                dialog.close();
                            }
                            Err(error) => ui.error(&error),
                        }
                    }
                });
        });
        dialog.connect_response(|dialog, _| dialog.close());
        dialog.present();
    }

    fn confirm_delete_profile(self: &Rc<Self>, profile: ConfidentialProfile) {
        let weak = Rc::downgrade(self);
        confirm(
            &self.window,
            "Delete profile?",
            "The profile credential will be removed from the system credential store.",
            "Delete",
            move || {
                if let Some(ui) = weak.upgrade() {
                    let weak = Rc::downgrade(&ui);
                    ui.client.request::<GatewayState>(
                        "deleteProfile",
                        json!({ "profileId": profile.id }),
                        move |result| {
                            if let Some(ui) = weak.upgrade() {
                                match result {
                                    Ok(state) => ui.accept_state(state),
                                    Err(error) => ui.error(&error),
                                }
                            }
                        },
                    );
                }
            },
        );
    }
    fn show_local_api(self: &Rc<Self>) {
        let config = self.state.borrow().local_api.clone();
        let dialog = dialog(&self.window, "Local API Settings", 580, 440);
        let body = vbox(12);
        let address = gtk::Entry::new();
        address.set_text(&config.listen_address);
        let network = gtk::Switch::builder()
            .active(config.allow_network_access)
            .build();
        let network_row = adw::ActionRow::builder()
            .title("Allow network access")
            .subtitle("Exposes the Local API beyond this computer.")
            .build();
        network_row.add_suffix(&network);
        let port = gtk::SpinButton::with_range(1024.0, 65535.0, 1.0);
        port.set_value(config.port as f64);
        let host = gtk::Entry::new();
        host.set_text(config.client_host.as_deref().unwrap_or(""));
        body.append(&labeled("Listen address", &address));
        body.append(&network_row);
        body.append(&labeled("Port", &port));
        body.append(&labeled("Client host", &host));
        dialog.content_area().append(&body);
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button("Save", gtk::ResponseType::Accept);
        let weak = Rc::downgrade(self);
        dialog.connect_response(move |dialog, response| {
            if response != gtk::ResponseType::Accept {
                dialog.close();
                return;
            }
            let Some(ui) = weak.upgrade() else {
                dialog.close();
                return;
            };
            let config = LocalApiConfig {
                listen_address: address.text().to_string(),
                allow_network_access: network.is_active(),
                port: port.value_as_int() as u16,
                client_host: (!host.text().is_empty()).then(|| host.text().to_string()),
            };
            let weak = Rc::downgrade(&ui);
            let dialog = dialog.clone();
            ui.client.request::<GatewayState>(
                "saveLocalApiConfig",
                json!({ "config": config }),
                move |result| {
                    if let Some(ui) = weak.upgrade() {
                        match result {
                            Ok(state) => {
                                ui.accept_state(state);
                                dialog.close();
                            }
                            Err(error) => ui.error(&error),
                        }
                    }
                },
            );
        });
        dialog.present();
    }

    fn show_proof(self: &Rc<Self>, item: RequestActivity) {
        let dialog = dialog(&self.window, proof_verdict(&item), 700, 620);
        let body = vbox(9);
        let state = self.state.borrow();
        for (label, value) in [
            ("Request", item.id.clone()),
            (
                "Agent",
                item.agent.clone().unwrap_or_else(|| "Unknown".into()),
            ),
            (
                "Model",
                item.model.clone().unwrap_or_else(|| "Not reported".into()),
            ),
            ("Path", format!("{} {}", item.method, item.path)),
            ("Status", item.status.to_string()),
            (
                "Receipt",
                item.receipt_id
                    .clone()
                    .unwrap_or_else(|| "No receipt".into()),
            ),
            (
                "Policy",
                if item.locally_constrained == Some(true) {
                    "Applied before forwarding".into()
                } else {
                    "Not reported".into()
                },
            ),
            (
                "Rewrite",
                if item.rewritten == Some(true) {
                    "Service rewrote the request".into()
                } else {
                    "No rewrite reported".into()
                },
            ),
            (
                "Delivery",
                if item.left_device {
                    "Request may have left this computer".into()
                } else {
                    "Blocked locally before delivery".into()
                },
            ),
            (
                "Input tokens",
                item.input_tokens
                    .map(format_number)
                    .unwrap_or_else(|| "Not reported".into()),
            ),
            (
                "Output tokens",
                item.output_tokens
                    .map(format_number)
                    .unwrap_or_else(|| "Not reported".into()),
            ),
            (
                "Cost",
                item.cost_usd
                    .map(|cost| format!("${cost:.4}"))
                    .unwrap_or_else(|| "Not reported".into()),
            ),
            (
                "Gateway keyset",
                state
                    .identity
                    .as_ref()
                    .map(|identity| identity.keyset_digest.clone())
                    .unwrap_or_else(|| "Not available".into()),
            ),
            (
                "Detail",
                if item.detail.is_empty() {
                    "No additional detail".into()
                } else {
                    item.detail.clone()
                },
            ),
        ] {
            body.append(&detail_row(label, &value));
        }
        dialog.content_area().append(&scrolled(&body));
        dialog.add_button("Done", gtk::ResponseType::Close);
        dialog.connect_response(|dialog, _| dialog.close());
        dialog.present();
    }
    fn show_privacy(self: &Rc<Self>) {
        let state = self.state.borrow();
        let dialog = dialog(&self.window, "Privacy Verification", 740, 660);
        let body = vbox(14);
        body.append(&wrapped("The gateway verifies the workload identity and model catalog before forwarding requests. Each response receipt binds the request, verified upstream session, and returned response."));
        if let Some(identity) = &state.identity {
            body.append(&section_title("Workload identity"));
            for (label, value) in [
                ("TEE", identity.tee_type.as_str()),
                ("Trust level", identity.trust_level.as_str()),
                ("Keyset digest", identity.keyset_digest.as_str()),
                ("Serving mode", identity.serving.as_str()),
                (
                    "TLS SPKI",
                    identity.tls_spki.as_deref().unwrap_or("Not published"),
                ),
            ] {
                body.append(&detail_row(label, value));
            }
            body.append(&section_title("Source provenance"));
            for (label, value) in [
                (
                    "Repository",
                    identity
                        .source
                        .repo_url
                        .as_deref()
                        .unwrap_or("Not published"),
                ),
                (
                    "Commit",
                    identity
                        .source
                        .repo_commit
                        .as_deref()
                        .unwrap_or("Not published"),
                ),
                (
                    "Image digest",
                    identity
                        .source
                        .image_digest
                        .as_deref()
                        .unwrap_or("Not published"),
                ),
            ] {
                body.append(&detail_row(label, value));
            }
        }
        body.append(&section_title("Verification checks"));
        for check in &state.checks {
            let row = hbox(10);
            row.append(&gtk::Image::from_icon_name(if check.status == "pass" {
                "emblem-ok-symbolic"
            } else if check.status == "fail" {
                "dialog-error-symbolic"
            } else {
                "dialog-information-symbolic"
            }));
            let text = vbox(2);
            text.append(
                &gtk::Label::builder()
                    .label(&check.title)
                    .halign(Align::Start)
                    .build(),
            );
            text.append(
                &gtk::Label::builder()
                    .label(&check.detail)
                    .css_classes(["dim-label", "caption"])
                    .halign(Align::Start)
                    .wrap(true)
                    .build(),
            );
            row.append(&text);
            body.append(&row);
        }
        dialog.content_area().append(&scrolled(&body));
        dialog.add_button("Done", gtk::ResponseType::Close);
        dialog.connect_response(|dialog, _| dialog.close());
        dialog.present();
    }
    fn confirm_clear_usage(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        confirm(&self.window, "Clear all usage history?", "This permanently deletes the local usage database. It does not affect provider records.", "Clear History", move || if let Some(ui) = weak.upgrade() { let weak = Rc::downgrade(&ui); ui.client.request::<u64>("clearUsage", json!({}), move |result| if let Some(ui) = weak.upgrade() { match result { Ok(_) => ui.reload_usage(true), Err(error) => ui.error(&error) } }); });
    }
    fn confirm_restore(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        confirm(&self.window, "Restore all agent configurations?", "Private AI Gateway will revoke every managed agent token and restore its previous configuration where possible.", "Restore All", move || if let Some(ui) = weak.upgrade() { let weak = Rc::downgrade(&ui); ui.client.request::<Vec<AgentStatus>>("disconnectAllAgents", json!({}), move |result| if let Some(ui) = weak.upgrade() { match result { Ok(agents) => { *ui.agents.borrow_mut() = agents; ui.render(); }, Err(error) => ui.error(&error) } }); });
    }
    fn rotate_client_key(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.client
            .request::<String>("rotateClientKey", json!({}), move |result| {
                if let Some(ui) = weak.upgrade() {
                    match result {
                        Ok(key) => {
                            *ui.client_key.borrow_mut() = key;
                            ui.toast.add_toast(adw::Toast::new("Client key rotated"));
                            ui.render();
                        }
                        Err(error) => ui.error(&error),
                    }
                }
            });
    }
    fn export_usage(self: &Rc<Self>) {
        let chooser = gtk::FileChooserNative::builder()
            .title("Export Usage CSV")
            .transient_for(&self.window)
            .action(gtk::FileChooserAction::Save)
            .accept_label("Export")
            .cancel_label("Cancel")
            .build();
        chooser.set_current_name("private-ai-gateway-usage.csv");
        let weak = Rc::downgrade(self);
        chooser.connect_response(move |chooser, response| {
            if response == gtk::ResponseType::Accept {
                if let (Some(ui), Some(path)) =
                    (weak.upgrade(), chooser.file().and_then(|file| file.path()))
                {
                    let now = unix_now();
                    let since = match ui.usage_range.get() {
                        0 => Some(now.saturating_sub(7 * 86_400)),
                        1 => Some(now.saturating_sub(30 * 86_400)),
                        _ => None,
                    };
                    let query = UsageQuery {
                        agent: ui.usage_agent.borrow().clone(),
                        model: ui.usage_model.borrow().clone(),
                        session_id: None,
                        since,
                        until: None,
                        cursor: None,
                        limit: None,
                    };
                    let weak = Rc::downgrade(&ui);
                    ui.client.request::<usize>(
                        "exportUsageCsv",
                        json!({ "query": query, "path": path }),
                        move |result| {
                            if let Some(ui) = weak.upgrade() {
                                match result {
                                    Ok(count) => ui.toast.add_toast(adw::Toast::new(&format!(
                                        "Exported {count} records"
                                    ))),
                                    Err(error) => ui.error(&error),
                                }
                            }
                        },
                    );
                }
            }
            chooser.destroy();
        });
        chooser.show();
    }

    fn handle_tray(self: &Rc<Self>, command: TrayCommand) {
        match command {
            TrayCommand::Toggle => {
                let enabled = !is_running(&self.state.borrow());
                self.set_protection(enabled);
            }
            TrayCommand::Open => self.window.present(),
            TrayCommand::Settings => {
                self.page.set(3);
                self.window.present();
                self.render();
            }
            TrayCommand::OpenAtLogin => match tray::set_open_at_login(!tray::open_at_login()) {
                Ok(()) => self.render(),
                Err(error) => self.error(&error),
            },
            TrayCommand::Quit => {
                self.client.shutdown();
                self.app.quit();
            }
        }
    }
    fn error(&self, message: &str) {
        self.toast.add_toast(adw::Toast::new(message));
    }
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        ".navigation-sidebar { background: alpha(@window_fg_color, .035); } \
         .card { background: alpha(@window_fg_color, .035); border: 1px solid alpha(@window_fg_color, .10); border-radius: 8px; padding: 14px; } \
         .status-surface, .overview-surface { padding: 0; } \
         .status-surface { background: alpha(@window_fg_color, .025); } \
         .track-plain, .track-cipher { font-family: monospace; font-size: 11px; opacity: .11; } \
         .track-cipher { color: #2c6e49; opacity: .16; } \
         .agent-icon { background: white; border: 1px solid alpha(black, .12); border-radius: 6px; padding: 4px; } \
         .session-metric { padding: 9px 12px; border: 1px solid alpha(@window_fg_color, .07); } \
         .overview-action { color: #2c6e49; padding: 2px 5px; } \
         .dev-badge { color: #9a6700; background: alpha(#e9a400, .16); border-radius: 5px; padding: 4px 8px; font-weight: 600; } \
         .success { color: #2c6e49; } .warning { color: #9a6700; } \
         .monospace { font-family: monospace; } .caption { font-size: 0.88em; }",
    );
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
fn show_startup_error(app: &adw::Application, message: &str) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Private AI Gateway")
        .default_width(520)
        .default_height(220)
        .build();
    let box_ = page_box();
    box_.append(&section_title("Private AI Gateway could not start"));
    box_.append(&wrapped(message));
    window.set_content(Some(&box_));
    window.present();
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
fn status_label(state: &GatewayState) -> &'static str {
    match state.status.as_str() {
        "verifying" if state.configuration_verification => "Verifying configuration",
        "verifying" => "Starting",
        "verified" if !state.config.require_production_os => "Protected · Dev mode",
        "verified" => "Protected",
        "blocked" => "Blocked",
        "error" => "Needs attention",
        _ => "Not protected",
    }
}
fn page_box() -> gtk::Box {
    let root = vbox(24);
    root.set_margin_start(28);
    root.set_margin_end(28);
    root.set_margin_top(28);
    root.set_margin_bottom(28);
    root
}
fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(Orientation::Vertical, spacing)
}
fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(Orientation::Horizontal, spacing)
}
fn card(child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let card = vbox(0);
    card.add_css_class("card");
    card.append(child);
    card
}
fn overview_module(
    title: &str,
    meta: Option<&str>,
    child: &impl IsA<gtk::Widget>,
    height: i32,
) -> gtk::Box {
    let root = vbox(8);
    let heading = hbox(8);
    heading.append(&section_title(title));
    let spacer = gtk::Box::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    heading.append(&spacer);
    if let Some(meta) = meta {
        heading.append(
            &gtk::Label::builder()
                .label(meta)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    root.append(&heading);
    let surface = card(child);
    surface.add_css_class("overview-surface");
    surface.set_height_request(height);
    root.append(&surface);
    root
}
fn overview_module_with_action(
    title: &str,
    action: &str,
    child: &impl IsA<gtk::Widget>,
    height: i32,
) -> (gtk::Box, gtk::Button) {
    let root = vbox(8);
    let heading = hbox(8);
    heading.append(&section_title(title));
    let spacer = gtk::Box::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    heading.append(&spacer);
    let button = gtk::Button::with_label(action);
    button.set_has_frame(false);
    button.add_css_class("overview-action");
    heading.append(&button);
    root.append(&heading);
    let surface = card(child);
    surface.add_css_class("overview-surface");
    surface.set_height_request(height);
    root.append(&surface);
    (root, button)
}
fn session_metrics(summary: &UsageSummary) -> gtk::Grid {
    let forwarded = summary.requests.saturating_sub(summary.blocked_locally);
    let protected = if forwarded == 0 {
        "—".to_string()
    } else {
        format!("{}%", summary.protected.saturating_mul(100) / forwarded)
    };
    let grid = gtk::Grid::builder()
        .column_homogeneous(true)
        .row_homogeneous(true)
        .build();
    for (index, (label, value)) in [
        ("Requests", format_number(summary.requests)),
        (
            "Tokens",
            compact_number(summary.input_tokens + summary.output_tokens),
        ),
        ("Cost", format!("${:.4}", summary.cost_usd)),
        ("Protected", protected),
    ]
    .into_iter()
    .enumerate()
    {
        let metric = vbox(1);
        metric.add_css_class("session-metric");
        metric.append(
            &gtk::Label::builder()
                .label(label)
                .css_classes(["dim-label", "caption"])
                .halign(Align::Start)
                .build(),
        );
        metric.append(
            &gtk::Label::builder()
                .label(value)
                .css_classes(["title-3", "monospace"])
                .halign(Align::Start)
                .build(),
        );
        grid.attach(&metric, (index % 2) as i32, (index / 2) as i32, 1, 1);
    }
    grid
}
fn track_layer(lines: &[&str], align_right: bool, class: &str) -> gtk::Box {
    let root = vbox(0);
    root.set_valign(Align::Center);
    root.set_overflow(gtk::Overflow::Hidden);
    for line in lines {
        root.append(
            &gtk::Label::builder()
                .label(*line)
                .css_classes([class])
                .halign(if align_right { Align::End } else { Align::Start })
                .xalign(if align_right { 1.0 } else { 0.0 })
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .single_line_mode(true)
                .build(),
        );
    }
    root
}
fn service_host(value: &str) -> &str {
    value
        .split_once("://")
        .map_or(value, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .filter(|host| !host.is_empty())
        .unwrap_or("Not configured")
}
fn section_title(title: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(title)
        .css_classes(["heading"])
        .halign(Align::Start)
        .margin_bottom(8)
        .build()
}
fn wrapped(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .halign(Align::Start)
        .xalign(0.0)
        .build()
}
fn empty(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["dim-label"])
        .margin_top(24)
        .margin_bottom(24)
        .build()
}
fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}
fn labeled(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let box_ = vbox(3);
    box_.append(
        &gtk::Label::builder()
            .label(label)
            .css_classes(["dim-label", "caption"])
            .halign(Align::Start)
            .build(),
    );
    box_.append(widget);
    box_
}
fn detail_row(label: &str, value: &str) -> gtk::Grid {
    let grid = gtk::Grid::builder()
        .column_spacing(18)
        .row_spacing(4)
        .build();
    let name = gtk::Label::builder()
        .label(label)
        .css_classes(["dim-label"])
        .halign(Align::End)
        .build();
    let value = gtk::Label::builder()
        .label(value)
        .selectable(true)
        .wrap(true)
        .halign(Align::Start)
        .xalign(0.0)
        .hexpand(true)
        .build();
    grid.attach(&name, 0, 0, 1, 1);
    grid.attach(&value, 1, 0, 1, 1);
    grid
}
fn copy_row(label: &str, value: &str, copy_value: Option<String>) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_has_frame(false);
    let row = hbox(8);
    let value_label = gtk::Label::builder()
        .label(value)
        .halign(Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["monospace"])
        .build();
    row.append(&labeled(label, &value_label));
    row.append(&gtk::Image::from_icon_name("edit-copy-symbolic"));
    button.set_child(Some(&row));
    button.set_sensitive(copy_value.is_some());
    button.connect_clicked(move |_| {
        if let Some(value) = &copy_value {
            clipboard(value);
        }
    });
    button
}
fn clipboard(value: &str) {
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_text(value);
    }
}
fn asset_picture(relative: &str, size: i32) -> gtk::Picture {
    let path = std::env::var_os("PRIVATE_AI_GATEWAY_ASSETS")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|parent| parent.join("Assets")))
        })
        .unwrap_or_default()
        .join(relative);
    let picture = gtk::Picture::for_filename(path);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_size_request(size, size);
    picture
}
fn metrics(summary: &UsageSummary, title: Option<&str>) -> gtk::Box {
    let root = vbox(10);
    if let Some(title) = title {
        root.append(&section_title(title));
    }
    let grid = gtk::Grid::builder()
        .column_spacing(28)
        .column_homogeneous(true)
        .build();
    let protected = if summary.requests == 0 {
        "—".into()
    } else {
        format!("{}%", summary.protected * 100 / summary.requests)
    };
    for (index, (label, value)) in [
        ("Requests", format_number(summary.requests)),
        (
            "Tokens",
            format_number(summary.input_tokens + summary.output_tokens),
        ),
        ("Cost", format!("${:.4}", summary.cost_usd)),
        ("Protected", protected),
    ]
    .into_iter()
    .enumerate()
    {
        grid.attach(
            &labeled(
                label,
                &gtk::Label::builder()
                    .label(value)
                    .css_classes(["title-3", "monospace"])
                    .halign(Align::Start)
                    .build(),
            ),
            index as i32,
            0,
            1,
            1,
        );
    }
    root.append(&grid);
    card(&root)
}
fn usage_chart(points: &[desktop_runtime::usage::UsagePoint]) -> gtk::Box {
    let chart = gtk::DrawingArea::builder()
        .content_height(170)
        .hexpand(true)
        .build();
    let points = points.iter().map(|point| point.tokens).collect::<Vec<_>>();
    chart.set_draw_func(move |_, cr, width, height| {
        let max = points.iter().copied().max().unwrap_or(1).max(1) as f64;
        let gap = 3.0;
        let bar = (width as f64 / points.len().max(1) as f64 - gap).max(2.0);
        cr.set_source_rgb(0.17, 0.43, 0.29);
        for (index, value) in points.iter().enumerate() {
            let h = (*value as f64 / max * (height as f64 - 16.0)).max(2.0);
            cr.rectangle(index as f64 * (bar + gap), height as f64 - h, bar, h);
            let _ = cr.fill();
        }
    });
    card(&chart)
}
fn dialog(parent: &adw::ApplicationWindow, title: &str, width: i32, height: i32) -> gtk::Dialog {
    gtk::Dialog::builder()
        .transient_for(parent)
        .modal(true)
        .title(title)
        .default_width(width)
        .default_height(height)
        .use_header_bar(1)
        .build()
}
fn confirm(
    parent: &adw::ApplicationWindow,
    title: &str,
    message: &str,
    action: &str,
    accept: impl Fn() + 'static,
) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .message_type(gtk::MessageType::Warning)
        .buttons(gtk::ButtonsType::None)
        .text(title)
        .secondary_text(message)
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    let button = dialog.add_button(action, gtk::ResponseType::Accept);
    button.add_css_class("destructive-action");
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            accept();
        }
        dialog.close();
    });
    dialog.present();
}
fn selected_profile(
    list: &gtk::ListBox,
    profiles: &[ConfidentialProfile],
) -> Option<ConfidentialProfile> {
    let index = usize::try_from(list.selected_row()?.index()).ok()?;
    profiles.get(index).cloned()
}
fn proof_verdict(item: &RequestActivity) -> &'static str {
    if item.verified == Some(true) {
        "Proof verified"
    } else if !item.left_device {
        "Blocked locally"
    } else if item.verified == Some(false) {
        "Proof failed"
    } else {
        "Proof unavailable"
    }
}
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
fn format_number(value: u64) -> String {
    let text = value.to_string();
    let mut result = String::new();
    for (index, character) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            result.push(',');
        }
        result.push(character);
    }
    result.chars().rev().collect()
}
fn compact_number(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        format_number(value)
    }
}

const PLAINTEXT_TRACKS: [&str; 11] = [
    "POST /v1/messages  model demo/verified-chat-01  stream true",
    "event content_block_delta  compose hash matches expected value",
    "messages user  inspect public dstack attestation report",
    "event message_delta  stop_reason end_turn  output_tokens 96",
    "POST /v1/responses  model demo/verified-reasoning-01",
    "event response.output_text.delta  release notes summarized",
    "input user  summarize public release notes  tool read_public_file",
    "event response.completed  input_tokens 384  output_tokens 96",
    "POST /v1/chat/completions  compare tdx_quote and compose_hash",
    "data chat.completion.chunk  both digests match",
    "tools function compare_hash  tool_choice auto",
];

const TLS_TRACKS: [&str; 11] = [
    "17 03 03 00 f4  9f3a c1e0 7b42 d5a8 0e6f 2c91",
    "17 03 03 03 1a  4d17 e8b3 5a0c f9d2 61b7 a3e4",
    "application_data record_len 244  17 03 03 00 f4  6d03 c1e8",
    "17 03 03 01 6c  e1a7 5c09 f38d 2b64 d0e7 4a1f",
    "17 03 03 00 5e  b2a0 7e95 0d1b 9c6e 18e4 a0f7",
    "application_data record_len 794  17 03 03 03 1a  b5c8 02a6",
    "17 03 03 02 48  9e21 4fb7 a6d5 0c83 e2f9 71b4",
    "17 03 03 00 91  81c7 e6a2 5b9d 1f74 c0e3 a8d6",
    "17 03 03 01 d0  a3e8 5c02 e9b1 4d7f 8a36 1e0c",
    "application_data record_len 152  17 03 03 00 98  c7d2 3e5a",
    "17 03 03 00 3c  7a0d 2c95 f6e3 41b8 d9c0 3f5e",
];
