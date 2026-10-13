use super::*;
use rufin_core::runtime::source::PlexLoginMethod;

#[derive(Default)]
struct Draft {
    authorization: Option<Arc<SourceAuthorization>>,
    profiles: Vec<SourceChoice>,
    profile: usize,
    servers: Option<Vec<SourceChoice>>,
    server: usize,
    name: String,
    address: String,
    trust_invalid_cert: bool,
}

impl Draft {
    fn connect(&self) {
        let Some(authorization) = self.authorization.clone() else {
            return;
        };
        let Some(server) = self
            .servers
            .as_ref()
            .and_then(|servers| servers.get(self.server))
        else {
            return;
        };
        let name = if self.name.trim().is_empty() {
            server.title.clone()
        } else {
            self.name.trim().into()
        };
        let input = SourceAuthorizationInput::Plex {
            server_index: self.server as u32,
            address_override: optional_address(&self.address),
            trust_invalid_cert: self.trust_invalid_cert,
        };
        gtk::glib::spawn_future_local(async move {
            let _ = authorization.finish(name, None, false, input).await;
        });
    }
}

struct PlexFlow {
    presentation: &'static SourcePresentation,
    draft: Rc<RefCell<Draft>>,
}

pub(super) fn setup_flow(
    _: &Rc<Preferences>,
    presentation: &'static SourcePresentation,
) -> Rc<dyn SourceSetupFlow> {
    Rc::new(PlexFlow {
        presentation,
        draft: Rc::new(RefCell::new(Draft::default())),
    })
}

fn remount(shell: &std::rc::Weak<Preferences>) {
    if let Some(shell) = shell.upgrade()
        && let Some(context) = shell.mounted_add_server_context()
    {
        mount_setup_flow(&shell, &context);
    }
}

impl SourceSetupFlow for PlexFlow {
    fn view(&self, shell: &Rc<Preferences>, context: &SetupViewContext) -> gtk::Widget {
        shell.select_discovery_provider(sources::DiscoveryProvider::Plex);
        shell.start_server_discovery_once();
        let (scroller, content, _, status) = setup_scaffold(shell, context, self.presentation);
        let resource = crate::ui_resource::PLEX_HOST_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            section: gtk::Box, account: adw::PreferencesGroup, username: adw::EntryRow,
            password: adw::PasswordEntryRow, verification: adw::EntryRow,
            password_login: gtk::Button, browser_login: gtk::Button, login_actions: gtk::Box,
            profiles: adw::PreferencesGroup, profile: adw::ComboRow, profile_options: gtk::StringList,
            pin: adw::PasswordEntryRow, choose_server: gtk::Button,
            connection: adw::PreferencesGroup, server: adw::ComboRow, server_options: gtk::StringList,
            name: adw::EntryRow, address: adw::EntryRow, cert_verify: adw::SwitchRow,
            change_profile: gtk::Button, connect: gtk::Button, save: gtk::Button
            , saved_login: adw::ComboRow, saved_options: gtk::StringList, reuse_login: gtk::Button
        });
        let draft = self.draft.borrow();
        let signed_in = draft.authorization.is_some();
        let choosing_server = draft.servers.is_some();
        account.set_visible(!signed_in);
        login_actions.set_visible(!signed_in);
        profiles.set_visible(signed_in && !choosing_server);
        choose_server.set_visible(signed_in && !choosing_server);
        choose_server.set_sensitive(!draft.profiles.is_empty());
        connection.set_visible(choosing_server);
        connect.set_visible(choosing_server);
        let single_unlocked_profile = draft.profiles.len() == 1 && !draft.profiles[0].pin_required;
        change_profile.set_visible(choosing_server && !single_unlocked_profile);
        save.set_visible(false);
        connect_entry_row_activation(&username, &password_login);
        connect_password_entry_row_activation(&password, &password_login);
        connect_entry_row_activation(&verification, &password_login);
        connect_password_entry_row_activation(&pin, &choose_server);
        connect_entry_row_activation(&name, &connect);
        connect_entry_row_activation(&address, &connect);
        profile_options.splice(
            0,
            0,
            &draft
                .profiles
                .iter()
                .map(|p| p.title.as_str())
                .collect::<Vec<_>>(),
        );
        profile.set_selected(draft.profile as u32);
        pin.set_visible(
            draft
                .profiles
                .get(draft.profile)
                .is_some_and(|p| p.pin_required),
        );
        if let Some(servers) = &draft.servers {
            server_options.splice(
                0,
                0,
                &servers.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
            );
        }
        server.set_selected(draft.server as u32);
        name.set_text(&draft.name);
        address.set_text(&draft.address);
        cert_verify.set_active(!draft.trust_invalid_cert);
        drop(draft);
        content.append(&section);
        content.append(&status);
        let tasks = Rc::new(RefCell::new(None::<gtk::glib::JoinHandle<()>>));
        let cancel = tasks.clone();
        section.connect_unmap(move |_| {
            if let Some(task) = cancel.borrow_mut().take() {
                task.abort();
            }
        });

        if !signed_in {
            let saved = Rc::new(RefCell::new(None::<Arc<SourceAuthorization>>));
            let source = shell.products.source.clone();
            let (saved_row, options, reuse, values) = (
                saved_login.downgrade(),
                saved_options.downgrade(),
                reuse_login.downgrade(),
                saved.clone(),
            );
            let listing = gtk::glib::spawn_future_local(async move {
                if let Ok(authorization) = SourceAuthorization::saved_plex(source).await {
                    let logins = authorization.choices();
                    let available = !logins.is_empty();
                    if let Some(options) = options.upgrade() {
                        options.splice(
                            0,
                            0,
                            &logins
                                .iter()
                                .map(|choice| choice.title.as_str())
                                .collect::<Vec<_>>(),
                        );
                    }
                    *values.borrow_mut() = Some(authorization);
                    if let Some(row) = saved_row.upgrade() {
                        row.set_visible(available);
                    }
                    if let Some(button) = reuse.upgrade() {
                        button.set_visible(available);
                    }
                }
            });
            section.connect_unmap(move |_| listing.abort());
            let (row, status) = (saved_login.downgrade(), status.downgrade());
            let login_buttons = [password_login.downgrade(), browser_login.downgrade()];
            let (state, weak_shell, tasks) =
                (self.draft.clone(), Rc::downgrade(shell), tasks.clone());
            reuse_login.connect_clicked(move |button| {
                let Some((authorization, index)) = saved
                    .borrow()
                    .clone()
                    .zip(row.upgrade().map(|row| row.selected()))
                else {
                    return;
                };
                for button in &login_buttons {
                    if let Some(button) = button.upgrade() {
                        button.set_sensitive(false);
                    }
                }
                if let Some(task) = tasks.borrow_mut().take() {
                    task.abort();
                }
                if let Some(status) = status.upgrade() {
                    status.set_text(&tr("Connecting to music server..."));
                    status.set_visible(true);
                }
                let (state, shell, status, button) = (
                    state.clone(),
                    weak_shell.clone(),
                    status.clone(),
                    button.downgrade(),
                );
                if let Some(button) = button.upgrade() {
                    button.set_sensitive(false);
                }
                let login_buttons = login_buttons.clone();
                *tasks.borrow_mut() = Some(gtk::glib::spawn_future_local(async move {
                    match authorization.plex_profiles(Some(index)).await {
                        Ok(profiles) => {
                            {
                                let mut draft = state.borrow_mut();
                                draft.authorization = Some(authorization);
                                draft.profiles = profiles;
                            }
                            remount(&shell);
                        }
                        Err(error) => {
                            let error = error.to_string();
                            if let Some(status) = status.upgrade() {
                                status.set_text(&error);
                                status.set_visible(true);
                            }
                            if let Some(button) = button.upgrade() {
                                button.set_sensitive(true);
                            }
                            for button in &login_buttons {
                                if let Some(button) = button.upgrade() {
                                    button.set_sensitive(true);
                                }
                            }
                        }
                    }
                }));
            });
        }

        for (button, browser) in [(&password_login, false), (&browser_login, true)] {
            let (username, password, verification) = (
                username.downgrade(),
                password.downgrade(),
                verification.downgrade(),
            );
            let (password_button, browser_button) =
                (password_login.downgrade(), browser_login.downgrade());
            let reuse_button = reuse_login.downgrade();
            let status = status.downgrade();
            let source = shell.products.source.clone();
            let shell = Rc::downgrade(shell);
            let state = self.draft.clone();
            let tasks = tasks.clone();
            button.connect_clicked(move |_| {
                if let Some(task) = tasks.borrow_mut().take() {
                    task.abort();
                }
                let method = if browser {
                    PlexLoginMethod::Browser
                } else {
                    let (Some(username), Some(password), Some(verification)) = (
                        username.upgrade(),
                        password.upgrade(),
                        verification.upgrade(),
                    ) else {
                        return;
                    };
                    PlexLoginMethod::Password {
                        username: username.text().into(),
                        password: password.text().into(),
                        verification_code: trimmed_optional_text(&verification),
                    }
                };
                for button in [&password_button, &browser_button, &reuse_button] {
                    if let Some(button) = button.upgrade() {
                        button.set_sensitive(false);
                    }
                }
                if let Some(status) = status.upgrade() {
                    status.set_text(&tr("Connecting to music server..."));
                    status.set_visible(true);
                }
                let events = SourceAuthorization::new(
                    source.clone(),
                    SourceAuthorizationMethod::Plex { method },
                );
                let reuse_button = reuse_button.clone();
                let (state, shell, status, password_button, browser_button) = (
                    state.clone(),
                    shell.clone(),
                    status.clone(),
                    password_button.clone(),
                    browser_button.clone(),
                );
                *tasks.borrow_mut() = Some(gtk::glib::spawn_future_local(async move {
                    loop {
                        let result = match events.next().await {
                            Ok(SourceAuthorizationEvent::OpenBrowser { url: address }) => {
                                if let Some(status) = status.upgrade() {
                                    status.set_text(&tr("Complete sign-in in your browser"));
                                }
                                gtk::gio::AppInfo::launch_default_for_uri_future(
                                    &address,
                                    None::<&gtk::gio::AppLaunchContext>,
                                )
                                .await
                                .map_err(|error| error.to_string())
                            }
                            Ok(SourceAuthorizationEvent::Ready { .. }) => {
                                match events.plex_profiles(None).await {
                                    Ok(profiles) => {
                                        {
                                            let mut draft = state.borrow_mut();
                                            draft.authorization = Some(events.clone());
                                            draft.profiles = profiles;
                                        }
                                        remount(&shell);
                                        return;
                                    }
                                    Err(error) => Err(error.to_string()),
                                }
                            }
                            Ok(SourceAuthorizationEvent::Code { .. }) => Ok(()),
                            Err(error) => Err(error.to_string()),
                        };
                        if let Err(error) = result {
                            if let Some(status) = status.upgrade() {
                                status.set_text(&error);
                            }
                            break;
                        }
                    }
                    for button in [&password_button, &browser_button, &reuse_button] {
                        if let Some(button) = button.upgrade() {
                            button.set_sensitive(true);
                        }
                    }
                }));
            });
        }
        let state = self.draft.clone();
        let pin_row = pin.downgrade();
        profile.connect_selected_notify(move |row| {
            let required = {
                let mut draft = state.borrow_mut();
                draft.profile = row.selected() as usize;
                draft
                    .profiles
                    .get(draft.profile)
                    .is_some_and(|p| p.pin_required)
            };
            if let Some(pin) = pin_row.upgrade() {
                pin.set_text("");
                pin.set_visible(required);
            }
        });
        let state = self.draft.clone();
        let weak_shell = Rc::downgrade(shell);
        let pin = pin.downgrade();
        let status_weak = status.downgrade();
        let profile_fields = profiles.downgrade();
        choose_server.connect_clicked(move |button| {
            let input = {
                let draft = state.borrow();
                draft
                    .authorization
                    .clone()
                    .map(|authorization| (authorization, draft.profile as u32))
            };
            let Some((authorization, profile)) = input else {
                return;
            };
            let pin = pin
                .upgrade()
                .filter(|p| p.is_visible())
                .map(|p| p.text().to_string())
                .filter(|p| !p.is_empty());
            let Some(shell) = weak_shell.upgrade() else {
                return;
            };
            let lan = shell.source.discovered_servers.borrow().clone();
            button.set_sensitive(false);
            if let Some(fields) = profile_fields.upgrade() {
                fields.set_sensitive(false);
            }
            if let Some(status) = status_weak.upgrade() {
                status.set_text(&tr("Connecting to music server..."));
                status.set_visible(true);
            }
            let (button, state, shell, status) = (
                button.downgrade(),
                state.clone(),
                weak_shell.clone(),
                status_weak.clone(),
            );
            let profile_fields = profile_fields.clone();
            *tasks.borrow_mut() = Some(gtk::glib::spawn_future_local(async move {
                match authorization.plex_servers(profile, pin, lan).await {
                    Ok(servers) => {
                        let single_server = servers.len() == 1;
                        {
                            let mut draft = state.borrow_mut();
                            draft.servers = Some(servers);
                            draft.server = 0;
                        }
                        remount(&shell);
                        if single_server {
                            state.borrow().connect();
                        }
                    }
                    Err(error) => {
                        let error = error.to_string();
                        if let Some(status) = status.upgrade() {
                            status.set_text(&error);
                            status.set_visible(true);
                        }
                        if let Some(button) = button.upgrade() {
                            button.set_sensitive(true);
                        }
                        if let Some(fields) = profile_fields.upgrade() {
                            fields.set_sensitive(true);
                        }
                    }
                }
            }));
        });
        let state = self.draft.clone();
        server.connect_selected_notify(move |row| {
            state.borrow_mut().server = row.selected() as usize
        });
        bind_connection(&name, &address, &cert_verify, &self.draft);
        let state = self.draft.clone();
        let ready: Rc<dyn Fn() -> bool> = Rc::new(move || {
            let draft = state.borrow();
            draft
                .servers
                .as_ref()
                .and_then(|s| s.get(draft.server))
                .is_some()
        });
        connect.set_sensitive(ready());
        *context.actions.borrow_mut() = Some(SetupActions {
            status: status.clone(),
            connect: connect.clone(),
            ready,
        });
        let state = self.draft.clone();
        let connect_status = status.clone();
        connect.connect_clicked(move |button| {
            begin_connect_attempt(
                &connect_status,
                button,
                &tr("Connecting to music server..."),
            );
            state.borrow().connect();
        });
        let state = self.draft.clone();
        let weak = Rc::downgrade(shell);
        change_profile.connect_clicked(move |_| {
            state.borrow_mut().servers = None;
            remount(&weak);
        });
        if signed_in && !choosing_server && single_unlocked_profile {
            choose_server.emit_clicked();
        }
        scroller.upcast()
    }
}

fn optional_address(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.trim().into())
}

fn bind_connection(
    name: &adw::EntryRow,
    address: &adw::EntryRow,
    cert: &adw::SwitchRow,
    draft: &Rc<RefCell<Draft>>,
) {
    let state = draft.clone();
    name.connect_text_notify(move |row| state.borrow_mut().name = row.text().into());
    let state = draft.clone();
    address.connect_text_notify(move |row| state.borrow_mut().address = row.text().into());
    let state = draft.clone();
    cert.connect_active_notify(move |row| state.borrow_mut().trust_invalid_cert = !row.is_active());
}

pub(super) fn settings_group(
    shell: &Rc<Preferences>,
    saved: &EditableSource,
    _: &'static SourcePresentation,
) -> Result<gtk::Widget, String> {
    let resource = crate::ui_resource::PLEX_HOST_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, { section: gtk::Box, account: adw::PreferencesGroup, profiles: adw::PreferencesGroup, login_actions: gtk::Box, choose_server: gtk::Button, change_profile: gtk::Button, connect: gtk::Button, save: gtk::Button, server: adw::ComboRow, name: adw::EntryRow, address: adw::EntryRow, cert_verify: adw::SwitchRow });
    for widget in [
        account.upcast::<gtk::Widget>(),
        profiles.upcast(),
        login_actions.upcast(),
        choose_server.upcast(),
        change_profile.upcast(),
        connect.upcast(),
        server.upcast(),
    ] {
        widget.set_visible(false);
    }
    let settings = saved.plex_settings.clone().expect("Plex editable settings");
    let draft = Rc::new(RefCell::new(Draft {
        name: settings.name,
        address: settings.address_override.unwrap_or_default(),
        trust_invalid_cert: settings.trust_invalid_cert,
        ..Draft::default()
    }));
    name.set_text(&draft.borrow().name);
    address.set_text(&draft.borrow().address);
    cert_verify.set_active(!draft.borrow().trust_invalid_cert);
    connect_entry_row_activation(&name, &save);
    connect_entry_row_activation(&address, &save);
    bind_connection(&name, &address, &cert_verify, &draft);
    let source = shell.products.source.clone();
    let source_id = saved.source.id.clone();
    save.connect_clicked(move |_| {
        let draft = draft.borrow();
        let settings = sources::PlexSettingsInput {
            name: draft.name.trim().into(),
            address_override: optional_address(&draft.address),
            trust_invalid_cert: draft.trust_invalid_cert,
        };
        let source = source.clone();
        let source_id = source_id.clone();
        gtk::glib::spawn_future_local(async move {
            let _ = source.save_plex(source_id, settings).await;
        });
    });
    Ok(section.upcast())
}
