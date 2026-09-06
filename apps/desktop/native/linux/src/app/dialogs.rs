use super::*;

impl Ui {
    pub(super) fn show_profiles(self: &Rc<Self>) {
        if self.state.borrow().profiles.is_empty() {
            self.show_profile_editor(None);
            return;
        }
        let dialog = dialog(&self.window(), "Profiles", 620, 500);
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
                ui.request::<GatewayState, _, _>(
                    "activateProfile",
                    json!({ "profileId": profile.id }),
                    |ui, result| match result {
                        Ok(state) => ui.accept_state(state),
                        Err(error) => ui.error(&error),
                    },
                );
            }
        });
        dialog.connect_response(|dialog, _| dialog.close());
        dialog.present();
    }

    pub(super) fn show_profile_editor(self: &Rc<Self>, profile: Option<ConfidentialProfile>) {
        let dialog = dialog(
            &self.window(),
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
            let dialog = verify_dialog.clone();
            let button = button.clone();
            ui.request::<GatewayState, _, _>("verifyConfiguration", params, move |ui, result| {
                button.set_sensitive(true);
                match result {
                    Ok(state) => {
                        ui.accept_state(state);
                        dialog.close();
                    }
                    Err(error) => ui.error(&error),
                }
            });
        });
        dialog.connect_response(|dialog, _| dialog.close());
        dialog.present();
    }

    pub(super) fn confirm_delete_profile(self: &Rc<Self>, profile: ConfidentialProfile) {
        let weak = Rc::downgrade(self);
        confirm(
            &self.window(),
            "Delete profile?",
            "The profile credential will be removed from the system credential store.",
            "Delete",
            move || {
                if let Some(ui) = weak.upgrade() {
                    ui.request::<GatewayState, _, _>(
                        "deleteProfile",
                        json!({ "profileId": profile.id }),
                        |ui, result| match result {
                            Ok(state) => ui.accept_state(state),
                            Err(error) => ui.error(&error),
                        },
                    );
                }
            },
        );
    }
    pub(super) fn show_local_api(self: &Rc<Self>) {
        let config = self.state.borrow().local_api.clone();
        let dialog = dialog(&self.window(), "Local API Settings", 580, 440);
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
            let dialog = dialog.clone();
            ui.request::<GatewayState, _, _>(
                "saveLocalApiConfig",
                json!({ "config": config }),
                move |ui, result| match result {
                    Ok(state) => {
                        ui.accept_state(state);
                        dialog.close();
                    }
                    Err(error) => ui.error(&error),
                },
            );
        });
        dialog.present();
    }

    pub(super) fn show_proof(self: &Rc<Self>, item: RequestActivity) {
        let dialog = dialog(&self.window(), proof_verdict(&item), 700, 620);
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
    pub(super) fn show_privacy(self: &Rc<Self>) {
        let state = self.state.borrow();
        let dialog = dialog(&self.window(), "Privacy Verification", 740, 660);
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
    pub(super) fn confirm_clear_usage(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        confirm(&self.window(), "Clear all usage history?", "This permanently deletes the local usage database. It does not affect provider records.", "Clear History", move || if let Some(ui) = weak.upgrade() { ui.request::<u64, _, _>("clearUsage", json!({}), |ui, result| match result { Ok(_) => ui.reload_usage(true), Err(error) => ui.error(&error) }); });
    }
    pub(super) fn confirm_restore(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        confirm(&self.window(), "Restore all agent configurations?", "Private AI Gateway will revoke every managed agent token and restore its previous configuration where possible.", "Restore All", move || if let Some(ui) = weak.upgrade() { ui.request::<Vec<AgentStatus>, _, _>("disconnectAllAgents", json!({}), |ui, result| match result { Ok(agents) => { *ui.agents.borrow_mut() = agents; ui.render(); }, Err(error) => ui.error(&error) }); });
    }
    pub(super) fn rotate_client_key(self: &Rc<Self>) {
        self.request::<String, _, _>("rotateClientKey", json!({}), |ui, result| match result {
            Ok(key) => {
                *ui.client_key.borrow_mut() = key;
                ui.toast.add_toast(adw::Toast::new("Client key rotated"));
                ui.render();
            }
            Err(error) => ui.error(&error),
        });
    }
    pub(super) fn export_usage(self: &Rc<Self>) {
        let chooser = gtk::FileChooserNative::builder()
            .title("Export Usage CSV")
            .transient_for(&self.window())
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
                    ui.request::<usize, _, _>(
                        "exportUsageCsv",
                        json!({ "query": query, "path": path }),
                        |ui, result| match result {
                            Ok(count) => ui
                                .toast
                                .add_toast(adw::Toast::new(&format!("Exported {count} records"))),
                            Err(error) => ui.error(&error),
                        },
                    );
                }
            }
            chooser.destroy();
        });
        chooser.show();
    }
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
