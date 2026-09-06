use super::*;

pub(super) struct UiViews {
    overview_icon: gtk::Image,
    overview_title: gtk::Label,
    overview_detail: gtk::Label,
    privacy: gtk::Button,
    endpoint: gtk::Label,
    endpoint_copy: gtk::Button,
    client_key: gtk::Label,
    client_key_reveal: gtk::ToggleButton,
    overview_agents: StableAgentList,
    session_metrics: MetricsView,
    recent: StableUsageList,
    agents: StableAgentList,
    usage_agent: gtk::ComboBoxText,
    usage_model: gtk::ComboBoxText,
    usage_agents: Vec<String>,
    usage_models: Vec<String>,
    usage_metrics: MetricsView,
    usage_chart: UsageChartView,
    usage_list: StableUsageList,
    load_more: gtk::Button,
    profile: adw::ActionRow,
    local_api_settings: adw::ActionRow,
    policy: adw::ActionRow,
}

struct MetricsView {
    root: gtk::Box,
    values: [gtk::Label; 4],
}

struct UsageChartView {
    root: gtk::Box,
    area: gtk::DrawingArea,
    points: Rc<RefCell<Vec<u64>>>,
}

struct StableAgentList {
    root: gtk::Box,
    rows: HashMap<String, AgentRowView>,
    limit: Option<usize>,
}

struct AgentRowView {
    row: adw::ActionRow,
    toggle: gtk::Switch,
    agent: Rc<RefCell<AgentStatus>>,
    syncing: Rc<Cell<bool>>,
}

struct StableUsageList {
    root: gtk::Box,
    rows: HashMap<String, UsageRowView>,
    limit: Option<usize>,
    empty_text: &'static str,
    empty: Option<gtk::Label>,
}

struct UsageRowView {
    row: adw::ActionRow,
    icon: gtk::Image,
    tokens: gtk::Label,
    item: Rc<RefCell<RequestActivity>>,
}

impl Ui {
    pub(super) fn render(self: &Rc<Self>) {
        let state = self.state.borrow();
        let connected = self.runtime_connected.get();
        self.syncing.set(true);
        self.protection.set_active(connected && is_running(&state));
        self.protection.set_sensitive(connected);
        self.syncing.set(false);
        let status = if connected {
            status_label(&state)
        } else {
            "Runtime disconnected"
        };
        self.status.set_label(status);
        self.restart.set_visible(!connected);
        self.dev
            .set_visible(connected && is_running(&state) && !state.config.require_production_os);
        self.show_page();
        let protected = connected && is_protected(&state);
        let running = connected && is_running(&state);
        if let Some(handle) = self.tray.borrow().as_ref() {
            let label = status.to_string();
            let open = tray::open_at_login();
            handle.update(move |tray| {
                tray.running = running;
                tray.protected = protected;
                tray.runtime_connected = connected;
                tray.status = label;
                tray.open_at_login = open;
            });
        }

        let mut views_ref = self.views.borrow_mut();
        let Some(views) = views_ref.as_mut() else {
            return;
        };
        views.overview_icon.set_icon_name(Some(if protected {
            "security-high-symbolic"
        } else {
            "security-medium-symbolic"
        }));
        views.overview_title.set_label(status);
        let detail = if connected {
            state
                .progress
                .as_deref()
                .or(state.error.as_deref())
                .or_else(|| active_profile(&state).map(|profile| profile.name.as_str()))
                .unwrap_or("Choose a Confidential AI profile")
                .to_string()
        } else {
            self.runtime_error
                .borrow()
                .clone()
                .unwrap_or_else(|| "Restart the desktop runtime to continue".to_string())
        };
        views.overview_detail.set_label(&detail);
        views
            .privacy
            .set_sensitive(connected && state.identity.is_some());
        views
            .endpoint
            .set_label(state.proxy_url.as_deref().unwrap_or("Unavailable"));
        views
            .endpoint_copy
            .set_sensitive(connected && state.proxy_url.is_some());
        update_client_key(views, &self.client_key.borrow());
        views.overview_agents.update(self, &self.agents.borrow());
        views.session_metrics.update(&state.session_usage);
        views.recent.update(self, &state.activity);
        views.agents.update(self, &self.agents.borrow());

        let usage = self.usage.borrow();
        self.syncing.set(true);
        update_combo(
            &views.usage_agent,
            "All agents",
            &mut views.usage_agents,
            &usage.agents,
            self.usage_agent.borrow().as_deref(),
        );
        update_combo(
            &views.usage_model,
            "All models",
            &mut views.usage_models,
            &usage.models,
            self.usage_model.borrow().as_deref(),
        );
        self.syncing.set(false);
        views.usage_metrics.update(&usage.summary);
        views.usage_chart.update(&usage.series);
        views.usage_list.update(self, &usage.items);
        views.load_more.set_visible(usage.next_cursor.is_some());
        views.profile.set_subtitle(
            active_profile(&state)
                .map(|profile| profile.name.as_str())
                .unwrap_or("Not configured"),
        );
        views.profile.set_sensitive(connected && !running);
        views
            .local_api_settings
            .set_sensitive(connected && !running);
        views
            .policy
            .set_subtitle(if state.config.require_production_os {
                "Production OS required"
            } else {
                "Development OS allowed"
            });
    }

    pub(super) fn show_page(&self) {
        let title = ["Overview", "Agents", "Usage", "Settings"][self.page.get().min(3)];
        self.title.set_label(title);
        self.stack.set_visible_child_name(
            ["overview", "agents", "usage", "settings"][self.page.get().min(3)],
        );
    }

    pub(super) fn build_views(self: &Rc<Self>) -> UiViews {
        let root = page_box();
        let summary = hbox(18);
        let icon = gtk::Image::from_icon_name("security-medium-symbolic");
        icon.set_pixel_size(36);
        summary.append(&icon);
        let labels = vbox(4);
        let overview_title = gtk::Label::builder()
            .label("Not protected")
            .css_classes(["title-3"])
            .halign(Align::Start)
            .build();
        let overview_detail = gtk::Label::builder()
            .label("Choose a Confidential AI profile")
            .css_classes(["dim-label"])
            .halign(Align::Start)
            .wrap(true)
            .build();
        labels.append(&overview_title);
        labels.append(&overview_detail);
        summary.append(&labels);
        let spacer = gtk::Box::new(Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        summary.append(&spacer);
        let profiles = gtk::Button::with_label("Profiles…");
        let weak = Rc::downgrade(self);
        profiles.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_profiles();
            }
        });
        summary.append(&profiles);
        let privacy = gtk::Button::with_label("Privacy Verification…");
        let weak = Rc::downgrade(self);
        privacy.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_privacy();
            }
        });
        summary.append(&privacy);
        root.append(&card(&summary));
        let columns = hbox(20);
        columns.set_homogeneous(true);
        let local = vbox(0);
        local.append(&section_title("Local API"));
        let endpoint_copy = gtk::Button::new();
        endpoint_copy.set_has_frame(false);
        let endpoint = gtk::Label::builder()
            .label("Unavailable")
            .halign(Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["monospace"])
            .build();
        let endpoint_row = hbox(8);
        endpoint_row.append(&labeled("Endpoint", &endpoint));
        endpoint_row.append(&gtk::Image::from_icon_name("edit-copy-symbolic"));
        endpoint_copy.set_child(Some(&endpoint_row));
        let weak = Rc::downgrade(self);
        endpoint_copy.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if let Some(endpoint) = ui.state.borrow().proxy_url.as_deref() {
                    clipboard(endpoint);
                }
            }
        });
        local.append(&endpoint_copy);
        let key_row = hbox(8);
        let key_copy = gtk::Button::new();
        key_copy.set_hexpand(true);
        key_copy.set_has_frame(false);
        let client_key = gtk::Label::builder()
            .label("pag_••••••••••••")
            .halign(Align::Start)
            .css_classes(["monospace"])
            .build();
        key_copy.set_child(Some(&labeled("Client key", &client_key)));
        let weak = Rc::downgrade(self);
        key_copy.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                clipboard(&ui.client_key.borrow());
            }
        });
        key_row.append(&key_copy);
        let client_key_reveal = gtk::ToggleButton::builder()
            .icon_name("view-reveal-symbolic")
            .tooltip_text("Reveal client key")
            .build();
        let weak = Rc::downgrade(self);
        let key_label = client_key.clone();
        client_key_reveal.connect_toggled(move |button| {
            if let Some(ui) = weak.upgrade() {
                let key = if button.is_active() {
                    ui.client_key.borrow().clone()
                } else {
                    "pag_••••••••••••".to_string()
                };
                key_label.set_label(&key);
            }
            button.set_icon_name(if button.is_active() {
                "view-conceal-symbolic"
            } else {
                "view-reveal-symbolic"
            });
        });
        key_row.append(&client_key_reveal);
        local.append(&key_row);
        columns.append(&card(&local));
        let overview_agents = StableAgentList::new(self, Some(5));
        let agents_card = vbox(0);
        agents_card.append(&section_title("Agents"));
        agents_card.append(&overview_agents.root);
        columns.append(&card(&agents_card));
        root.append(&columns);
        let session_metrics = MetricsView::new(Some("This session"));
        root.append(&session_metrics.root);
        let recent = StableUsageList::new(self, Some(5), "No usage this session");
        let recent_card = vbox(0);
        recent_card.append(&section_title("Recent usage"));
        recent_card.append(&recent.root);
        root.append(&card(&recent_card));
        self.stack.add_named(&scrolled(&root), Some("overview"));

        let agents_root = page_box();
        let agents = StableAgentList::new(self, None);
        agents_root.append(&card(&agents.root));
        self.stack
            .add_named(&scrolled(&agents_root), Some("agents"));

        let usage_root = page_box();
        let filters = hbox(10);
        let usage_agent = self.filter_combo("Agent", "All agents", true);
        let usage_model = self.filter_combo("Model", "All models", false);
        filters.append(&usage_agent);
        filters.append(&usage_model);
        let range = gtk::ComboBoxText::new();
        for label in ["7 days", "30 days", "All time"] {
            range.append_text(label);
        }
        range.set_active(Some(self.usage_range.get() as u32));
        let weak = Rc::downgrade(self);
        range.connect_changed(move |combo| {
            if let Some(ui) = weak.upgrade() {
                if ui.syncing.get() {
                    return;
                }
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
        usage_root.append(&filters);
        let usage_metrics = MetricsView::new(None);
        usage_root.append(&usage_metrics.root);
        let usage_chart = UsageChartView::new();
        usage_root.append(&usage_chart.root);
        let usage_list = StableUsageList::new(self, None, "No usage matches these filters");
        let usage_card = vbox(0);
        usage_card.append(&section_title("Usage history"));
        usage_card.append(&usage_list.root);
        usage_root.append(&card(&usage_card));
        let load_more = gtk::Button::with_label("Load More");
        load_more.set_halign(Align::Center);
        let weak = Rc::downgrade(self);
        load_more.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.reload_usage(false);
            }
        });
        usage_root.append(&load_more);
        self.stack.add_named(&scrolled(&usage_root), Some("usage"));

        let settings_root = page_box();
        let profiles = adw::PreferencesGroup::builder()
            .title("Confidential AI")
            .build();
        let profile = adw::ActionRow::builder()
            .title("Profile")
            .subtitle("Not configured")
            .activatable(true)
            .build();
        let weak = Rc::downgrade(self);
        profile.connect_activated(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_profiles();
            }
        });
        profiles.add(&profile);
        settings_root.append(&profiles);
        let local = adw::PreferencesGroup::builder().title("Local API").build();
        let local_api_settings = adw::ActionRow::builder()
            .title("Local API Settings")
            .activatable(true)
            .build();
        local_api_settings.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = Rc::downgrade(self);
        local_api_settings.connect_activated(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_local_api();
            }
        });
        local.add(&local_api_settings);
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
        settings_root.append(&local);
        let advanced = adw::ExpanderRow::builder().title("Advanced").build();
        let policy = adw::ActionRow::builder()
            .title("OS policy")
            .subtitle("Production OS required")
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
        settings_root.append(&group);
        self.stack
            .add_named(&scrolled(&settings_root), Some("settings"));

        UiViews {
            overview_icon: icon,
            overview_title,
            overview_detail,
            privacy,
            endpoint,
            endpoint_copy,
            client_key,
            client_key_reveal,
            overview_agents,
            session_metrics,
            recent,
            agents,
            usage_agent,
            usage_model,
            usage_agents: Vec::new(),
            usage_models: Vec::new(),
            usage_metrics,
            usage_chart,
            usage_list,
            load_more,
            profile,
            local_api_settings,
            policy,
        }
    }

    fn filter_combo(self: &Rc<Self>, tooltip: &str, all: &str, agent: bool) -> gtk::ComboBoxText {
        let combo = gtk::ComboBoxText::new();
        combo.set_tooltip_text(Some(tooltip));
        combo.append_text(all);
        combo.set_active(Some(0));
        let weak = Rc::downgrade(self);
        combo.connect_changed(move |combo| {
            if let Some(ui) = weak.upgrade() {
                if ui.syncing.get() {
                    return;
                }
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
}

impl StableAgentList {
    fn new(_ui: &Rc<Ui>, limit: Option<usize>) -> Self {
        Self {
            root: vbox(0),
            rows: HashMap::new(),
            limit,
        }
    }

    fn update(&mut self, ui: &Rc<Ui>, agents: &[AgentStatus]) {
        let agents = agents
            .iter()
            .take(self.limit.unwrap_or(usize::MAX))
            .collect::<Vec<_>>();
        let ids = agents
            .iter()
            .map(|agent| agent.id.as_str())
            .collect::<HashSet<_>>();
        let removed = self
            .rows
            .keys()
            .filter(|id| !ids.contains(id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        for id in removed {
            if let Some(row) = self.rows.remove(&id) {
                self.root.remove(&row.row);
            }
        }
        let mut previous: Option<gtk::Widget> = None;
        for agent in agents {
            if !self.rows.contains_key(&agent.id) {
                let row = AgentRowView::new(ui, agent.clone());
                self.root.append(&row.row);
                self.rows.insert(agent.id.clone(), row);
            }
            let row = self.rows.get(&agent.id).expect("agent row was inserted");
            row.update(agent);
            let widget: gtk::Widget = row.row.clone().upcast();
            self.root.reorder_child_after(&widget, previous.as_ref());
            previous = Some(widget);
        }
    }
}

impl AgentRowView {
    fn new(ui: &Rc<Ui>, agent: AgentStatus) -> Self {
        let row = adw::ActionRow::builder().build();
        row.add_prefix(&asset_picture(&format!("agents/{}.svg", agent.id), 28));
        let toggle = gtk::Switch::builder().valign(Align::Center).build();
        let current = Rc::new(RefCell::new(agent));
        let syncing = Rc::new(Cell::new(false));
        let weak = Rc::downgrade(ui);
        let callback_agent = current.clone();
        let callback_syncing = syncing.clone();
        toggle.connect_active_notify(move |toggle| {
            if callback_syncing.get() {
                return;
            }
            let agent = callback_agent.borrow().clone();
            if toggle.is_active() != agent.connected {
                if let Some(ui) = weak.upgrade() {
                    ui.set_agent(agent, toggle.is_active());
                }
            }
        });
        row.add_suffix(&toggle);
        row.set_activatable_widget(Some(&toggle));
        Self {
            row,
            toggle,
            agent: current,
            syncing,
        }
    }

    fn update(&self, agent: &AgentStatus) {
        self.row.set_title(&agent.name);
        self.row.set_subtitle(
            agent
                .error
                .as_deref()
                .or(agent.attention.as_deref())
                .unwrap_or(if agent.installed {
                    &agent.config_path
                } else {
                    "CLI not found"
                }),
        );
        self.syncing.set(true);
        self.toggle.set_active(agent.connected);
        self.toggle.set_sensitive(agent.installed || agent.recorded);
        self.syncing.set(false);
        *self.agent.borrow_mut() = agent.clone();
    }
}

impl StableUsageList {
    fn new(_ui: &Rc<Ui>, limit: Option<usize>, empty_text: &'static str) -> Self {
        Self {
            root: vbox(0),
            rows: HashMap::new(),
            limit,
            empty_text,
            empty: None,
        }
    }

    fn update(&mut self, ui: &Rc<Ui>, items: &[RequestActivity]) {
        let items = items
            .iter()
            .take(self.limit.unwrap_or(usize::MAX))
            .collect::<Vec<_>>();
        let ids = items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<HashSet<_>>();
        let removed = self
            .rows
            .keys()
            .filter(|id| !ids.contains(id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        for id in removed {
            if let Some(row) = self.rows.remove(&id) {
                self.root.remove(&row.row);
            }
        }
        if items.is_empty() {
            if self.empty.is_none() {
                let label = empty(self.empty_text);
                self.root.append(&label);
                self.empty = Some(label);
            }
            return;
        }
        if let Some(label) = self.empty.take() {
            self.root.remove(&label);
        }
        let mut previous: Option<gtk::Widget> = None;
        for item in items {
            if !self.rows.contains_key(&item.id) {
                let row = UsageRowView::new(ui, item.clone());
                self.root.append(&row.row);
                self.rows.insert(item.id.clone(), row);
            }
            let row = self.rows.get(&item.id).expect("usage row was inserted");
            row.update(item);
            let widget: gtk::Widget = row.row.clone().upcast();
            self.root.reorder_child_after(&widget, previous.as_ref());
            previous = Some(widget);
        }
    }
}

impl UsageRowView {
    fn new(ui: &Rc<Ui>, item: RequestActivity) -> Self {
        let row = adw::ActionRow::builder().activatable(true).build();
        let icon = gtk::Image::new();
        row.add_prefix(&icon);
        let tokens = gtk::Label::builder()
            .css_classes(["dim-label", "monospace"])
            .build();
        row.add_suffix(&tokens);
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let current = Rc::new(RefCell::new(item));
        let weak = Rc::downgrade(ui);
        let selected = current.clone();
        row.connect_activated(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_proof(selected.borrow().clone());
            }
        });
        Self {
            row,
            icon,
            tokens,
            item: current,
        }
    }

    fn update(&self, item: &RequestActivity) {
        self.row
            .set_title(item.model.as_deref().unwrap_or(&item.path));
        self.row.set_subtitle(&format!(
            "{} · {}",
            item.agent.as_deref().unwrap_or("Unknown"),
            item.path
        ));
        self.icon
            .set_icon_name(Some(if item.verified == Some(true) {
                "security-high-symbolic"
            } else if item.left_device {
                "dialog-warning-symbolic"
            } else {
                "action-unavailable-symbolic"
            }));
        self.tokens.set_label(&format_number(
            item.input_tokens.unwrap_or(0) + item.output_tokens.unwrap_or(0),
        ));
        *self.item.borrow_mut() = item.clone();
    }
}

impl MetricsView {
    fn new(title: Option<&str>) -> Self {
        let root = vbox(10);
        if let Some(title) = title {
            root.append(&section_title(title));
        }
        let grid = gtk::Grid::builder()
            .column_spacing(28)
            .column_homogeneous(true)
            .build();
        let values = std::array::from_fn(|index| {
            let value = gtk::Label::builder()
                .label("0")
                .css_classes(["title-3", "monospace"])
                .halign(Align::Start)
                .build();
            grid.attach(
                &labeled(["Requests", "Tokens", "Cost", "Protected"][index], &value),
                index as i32,
                0,
                1,
                1,
            );
            value
        });
        root.append(&grid);
        Self {
            root: card(&root),
            values,
        }
    }

    fn update(&self, summary: &UsageSummary) {
        self.values[0].set_label(&format_number(summary.requests));
        self.values[1].set_label(&format_number(summary.input_tokens + summary.output_tokens));
        self.values[2].set_label(&format!("${:.4}", summary.cost_usd));
        self.values[3].set_label(&if summary.requests == 0 {
            "—".to_string()
        } else {
            format!("{}%", summary.protected * 100 / summary.requests)
        });
    }
}

impl UsageChartView {
    fn new() -> Self {
        let area = gtk::DrawingArea::builder()
            .content_height(170)
            .hexpand(true)
            .build();
        let points = Rc::new(RefCell::new(Vec::<u64>::new()));
        let draw_points = points.clone();
        area.set_draw_func(move |_, cr, width, height| {
            let points = draw_points.borrow();
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
        Self {
            root: card(&area),
            area,
            points,
        }
    }

    fn update(&self, points: &[desktop_runtime::usage::UsagePoint]) {
        *self.points.borrow_mut() = points.iter().map(|point| point.tokens).collect();
        self.area.queue_draw();
    }
}

fn update_combo(
    combo: &gtk::ComboBoxText,
    all: &str,
    current_values: &mut Vec<String>,
    values: &[String],
    selected: Option<&str>,
) {
    if current_values != values {
        combo.remove_all();
        combo.append_text(all);
        for value in values {
            combo.append_text(value);
        }
        *current_values = values.to_vec();
    }
    combo.set_active(Some(
        selected
            .and_then(|selected| values.iter().position(|value| value == selected))
            .map_or(0, |index| index + 1) as u32,
    ));
}

fn update_client_key(views: &UiViews, key: &str) {
    views
        .client_key
        .set_label(if views.client_key_reveal.is_active() {
            key
        } else {
            "pag_••••••••••••"
        });
}

pub(super) fn page_box() -> gtk::Box {
    let root = vbox(24);
    root.set_margin_start(28);
    root.set_margin_end(28);
    root.set_margin_top(28);
    root.set_margin_bottom(28);
    root
}
pub(super) fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(Orientation::Vertical, spacing)
}
pub(super) fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(Orientation::Horizontal, spacing)
}
pub(super) fn card(child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let card = vbox(0);
    card.add_css_class("card");
    card.append(child);
    card
}
pub(super) fn section_title(title: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(title)
        .css_classes(["heading"])
        .halign(Align::Start)
        .margin_bottom(8)
        .build()
}
pub(super) fn wrapped(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .halign(Align::Start)
        .xalign(0.0)
        .build()
}
pub(super) fn empty(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["dim-label"])
        .margin_top(24)
        .margin_bottom(24)
        .build()
}
pub(super) fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}
pub(super) fn labeled(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
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
pub(super) fn detail_row(label: &str, value: &str) -> gtk::Grid {
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
pub(super) fn clipboard(value: &str) {
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_text(value);
    }
}
pub(super) fn asset_picture(relative: &str, size: i32) -> gtk::Picture {
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
pub(super) fn format_number(value: u64) -> String {
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
