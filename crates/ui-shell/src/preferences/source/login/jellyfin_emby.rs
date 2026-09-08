use super::*;
use rufin_core::runtime::source::{
    EmbyConnectLoginEvent, EmbyConnectLoginMethod, JellyfinQuickConnectEvent,
};

struct Draft {
    manual: bool,
    username: String,
    password: String,
    servers: Vec<sources::EmbyConnectServer>,
    server: usize,
    name: String,
    use_instant_mix: bool,
}

struct Flow {
    presentation: &'static SourcePresentation,
    local: Rc<RefCell<CredentialHostDraft>>,
    draft: Rc<RefCell<Draft>>,
}

impl Flow {
    fn new(presentation: &'static SourcePresentation, saved: Option<&EditableSource>) -> Self {
        Self {
            presentation,
            local: Rc::new(RefCell::new(credential_draft(
                saved.map(|s| s.credentials.clone()),
            ))),
            draft: Rc::new(RefCell::new(Draft {
                manual: presentation.kind != "emby" || saved.is_some_and(|s| !s.emby_connect),
                username: String::new(),
                password: String::new(),
                servers: Vec::new(),
                server: 0,
                name: saved
                    .map(|s| s.credentials.source_name.clone())
                    .unwrap_or_default(),
                use_instant_mix: saved
                    .and_then(|s| s.use_instant_mix)
                    .unwrap_or(presentation.kind == "emby"),
            })),
        }
    }
}

pub(super) fn setup_flow(
    _: &Rc<Shell>,
    presentation: &'static SourcePresentation,
) -> Rc<dyn SourceSetupFlow> {
    Rc::new(Flow::new(presentation, None))
}

pub(super) fn settings_group(
    shell: &Rc<Shell>,
    saved: &EditableSource,
    presentation: &'static SourcePresentation,
) -> Result<gtk::Widget, String> {
    Ok(Flow::new(presentation, Some(saved)).form(shell, None, Some(saved)))
}

impl SourceSetupFlow for Flow {
    fn view(&self, shell: &Rc<Shell>, context: &SetupViewContext) -> gtk::Widget {
        let (scroller, content, _, _) = setup_scaffold(shell, context, self.presentation);
        content.append(&self.form(shell, Some(context), None));
        scroller.upcast()
    }
}

impl Flow {
    fn form(
        &self,
        shell: &Rc<Shell>,
        context: Option<&SetupViewContext>,
        saved: Option<&EditableSource>,
    ) -> gtk::Widget {
        let resource = crate::ui_resource::JELLYFIN_EMBY_HOST_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        ui_shared::objects!(builder, resource, {
            section: gtk::Box, options: adw::PreferencesGroup, manual: adw::SwitchRow,
            manual_host: gtk::Box, account: adw::PreferencesGroup, username: adw::EntryRow,
            manual_extras: gtk::Box,
            password: adw::PasswordEntryRow, sign_in_actions: adw::WrapBox,
            sign_in: gtk::Button, code_sign_in: gtk::Button, connection: adw::PreferencesGroup,
            server: adw::ComboRow, server_options: gtk::StringList, name: adw::EntryRow,
            instant_mix: adw::SwitchRow, connect: gtk::Button, save: gtk::Button,
            quick_connect: gtk::Button, status: gtk::Label,
        });
        let emby = self.presentation.kind == "emby";
        let saved_connect = saved.is_some_and(|s| s.emby_connect);
        let saved_credentials = saved.map(|s| s.credentials.clone());
        let source_id = saved.map(|s| s.source.id.clone());
        let host = credential_host(&self.local, true, None);
        manual_host.append(&host.widget);
        host.rows.remove(&host.cert_verify);
        options.add(&host.cert_verify);
        manual.set_visible(emby);
        quick_connect.set_visible(!emby);
        connect.set_visible(saved.is_none());
        save.set_visible(saved.is_some());
        let action = if saved.is_some() { &save } else { &connect };
        {
            let draft = self.draft.borrow();
            manual.set_active(draft.manual);
            username.set_text(&draft.username);
            password.set_text(&draft.password);
            name.set_text(&draft.name);
            instant_mix.set_active(draft.use_instant_mix);
        }
        if let Some(context) = context {
            let discovery = discovered_servers_view(&host, self.presentation.kind);
            manual_extras.append(&discovery.group);
            *context.discovery.borrow_mut() = Some(discovery);
        }
        let task = Rc::new(RefCell::new(None::<gtk::glib::JoinHandle<()>>));
        let active_dialog = Rc::new(RefCell::new(None::<gtk::glib::WeakRef<adw::Dialog>>));
        let cancel: Rc<dyn Fn()> = {
            let task = task.clone();
            let dialog = active_dialog.clone();
            Rc::new(move || {
                if let Some(task) = task.borrow_mut().take() {
                    task.abort();
                }
                let dialog = dialog.borrow_mut().take().and_then(|d| d.upgrade());
                if let Some(dialog) = dialog {
                    dialog.close();
                }
            })
        };
        let on_unmap = cancel.clone();
        section.connect_unmap(move |_| on_unmap());
        let refresh: Rc<dyn Fn()> = {
            let (
                manual_host,
                manual_extras,
                cert_verify,
                account,
                sign_in_actions,
                connection,
                server,
                server_options,
                action,
                sign_in,
                quick_connect,
            ) = (
                manual_host.downgrade(),
                manual_extras.downgrade(),
                host.cert_verify.downgrade(),
                account.downgrade(),
                sign_in_actions.downgrade(),
                connection.downgrade(),
                server.downgrade(),
                server_options.downgrade(),
                action.downgrade(),
                sign_in.downgrade(),
                quick_connect.downgrade(),
            );
            let draft = self.draft.clone();
            let local = self.local.clone();
            Rc::new(move || {
                let state = draft.borrow();
                let manual = state.manual;
                let names = state
                    .servers
                    .iter()
                    .map(|s| s.name.clone())
                    .collect::<Vec<_>>();
                let selected = state.server;
                let account_ready = !state.username.trim().is_empty() && !state.password.is_empty();
                let connected = !names.is_empty() || saved_connect;
                let ready = if manual {
                    server_address_ready(&local.borrow().url)
                        && !local.borrow().username.trim().is_empty()
                } else {
                    connected
                };
                drop(state);
                if let Some(w) = manual_host.upgrade() {
                    w.set_visible(manual);
                }
                if let Some(w) = manual_extras.upgrade() {
                    w.set_visible(manual);
                }
                if let Some(w) = cert_verify.upgrade() {
                    w.set_visible(manual);
                }
                if let Some(w) = account.upgrade() {
                    w.set_visible(!manual);
                }
                if let Some(w) = sign_in_actions.upgrade() {
                    w.set_visible(!manual);
                }
                if let Some(w) = connection.upgrade() {
                    w.set_visible(!manual && connected);
                }
                if let Some(w) = server.upgrade() {
                    w.set_visible(!names.is_empty());
                    if let Some(options) = server_options.upgrade() {
                        let current = (0..options.n_items())
                            .filter_map(|i| options.string(i))
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>();
                        if current != names {
                            options.splice(
                                0,
                                options.n_items(),
                                &names.iter().map(String::as_str).collect::<Vec<_>>(),
                            );
                        }
                    }
                    if w.selected() != selected as u32 {
                        w.set_selected(selected as u32);
                    }
                }
                if let Some(w) = action.upgrade() {
                    w.set_visible(manual || connected);
                    w.set_sensitive(ready);
                }
                if let Some(w) = sign_in.upgrade() {
                    w.set_sensitive(account_ready);
                }
                if let Some(w) = quick_connect.upgrade() {
                    w.set_sensitive(server_address_ready(&local.borrow().url));
                }
            })
        };
        let discover: Rc<dyn Fn()> = {
            let shell = Rc::downgrade(shell);
            let setup = context.is_some();
            Rc::new(move || {
                if setup && let Some(shell) = shell.upgrade() {
                    shell.select_discovery_provider(if emby {
                        sources::DiscoveryProvider::Emby
                    } else {
                        sources::DiscoveryProvider::Jellyfin
                    });
                    shell.start_server_discovery_once();
                    shell.update_add_server_discovery();
                }
            })
        };
        let state = self.draft.clone();
        let update = refresh.clone();
        let cancel_mode = cancel.clone();
        let mode_status = status.downgrade();
        let discover_mode = discover.clone();
        manual.connect_active_notify(move |row| {
            cancel_mode();
            state.borrow_mut().manual = row.is_active();
            if let Some(status) = mode_status.upgrade() {
                status.set_visible(false);
            }
            update();
            if row.is_active() {
                discover_mode();
            }
        });
        let state = self.draft.clone();
        let update = refresh.clone();
        username.connect_text_notify(move |row| {
            state.borrow_mut().username = row.text().into();
            update();
        });
        let state = self.draft.clone();
        let update = refresh.clone();
        password.connect_text_notify(move |row| {
            state.borrow_mut().password = row.text().into();
            update();
        });
        for row in [&host.name, &host.url, &host.username] {
            let update = refresh.clone();
            row.connect_text_notify(move |_| update());
            connect_entry_row_activation(row, action);
        }
        connect_password_entry_row_activation(&host.password, action);
        connect_entry_row_activation(&username, &sign_in);
        connect_password_entry_row_activation(&password, &sign_in);
        connect_entry_row_activation(&name, action);
        let state = self.draft.clone();
        name.connect_text_notify(move |row| state.borrow_mut().name = row.text().into());
        let state = self.draft.clone();
        instant_mix
            .connect_active_notify(move |row| state.borrow_mut().use_instant_mix = row.is_active());
        let state = self.draft.clone();
        let name_row = name.downgrade();
        server.connect_selected_notify(move |row| {
            let name = {
                let mut state = state.borrow_mut();
                state.server = row.selected() as usize;
                state.servers.get(state.server).map(|s| s.name.clone())
            };
            if let Some(name) = name
                && let Some(row) = name_row.upgrade()
            {
                row.set_text(&name);
            }
        });
        let accept_servers: Rc<dyn Fn(Vec<sources::EmbyConnectServer>)> = {
            let state = self.draft.clone();
            let refresh = refresh.clone();
            let name = name.downgrade();
            let server = server.downgrade();
            let action = action.downgrade();
            let status = status.downgrade();
            Rc::new(move |servers| {
                let empty = servers.is_empty();
                let single_server = servers.len() == 1;
                let default_name = servers.first().map(|s| s.name.clone()).unwrap_or_default();
                {
                    let mut state = state.borrow_mut();
                    state.servers = servers;
                    state.server = 0;
                    state.name = default_name.clone();
                }
                if let Some(name) = name.upgrade() {
                    name.set_text(&default_name);
                }
                if let Some(status) = status.upgrade() {
                    status.set_text(&if empty {
                        tr("No linked servers found")
                    } else {
                        String::new()
                    });
                    status.set_visible(empty);
                }
                refresh();
                if single_server && let Some(action) = action.upgrade() {
                    action.emit_clicked();
                } else if !empty && let Some(server) = server.upgrade() {
                    server.grab_focus();
                }
            })
        };
        let source = shell.products.source.clone();
        let state = self.draft.clone();
        let login_status = status.downgrade();
        let complete = accept_servers.clone();
        let update = refresh.clone();
        let cancel_password = cancel.clone();
        sign_in.connect_clicked(move |button| {
            cancel_password();
            let state = state.borrow();
            let events = source.emby_connect_login(EmbyConnectLoginMethod::Password {
                username: state.username.clone(),
                password: state.password.clone(),
            });
            drop(state);
            button.set_sensitive(false);
            if let Some(status) = login_status.upgrade() {
                status.set_text(&tr("Connecting to music server..."));
                status.set_visible(true);
            }
            let (status, complete, update) =
                (login_status.clone(), complete.clone(), update.clone());
            *task.borrow_mut() = Some(gtk::glib::spawn_future_local(async move {
                match events.recv().await {
                    Ok(Ok(EmbyConnectLoginEvent::Servers(servers))) => {
                        complete(servers);
                        return;
                    }
                    Ok(Err(error)) => show_error(&status, &error),
                    Err(error) => show_error(&status, &error.to_string()),
                    _ => {}
                }
                update();
            }));
        });
        let source = shell.products.source.clone();
        let parent = section.downgrade();
        let dialogs = active_dialog.clone();
        let cancel_code = cancel.clone();
        code_sign_in.connect_clicked(move |_| {
            cancel_code();
            let Some(parent) = parent.upgrade() else {
                return;
            };
            let source = source.clone();
            let complete = accept_servers.clone();
            let dialog = present_code_dialog(&parent, move |view| {
                let events = source.emby_connect_login(EmbyConnectLoginMethod::Pin);
                let complete = complete.clone();
                gtk::glib::spawn_future_local(async move {
                    while let Ok(event) = events.recv().await {
                        match event {
                            Ok(EmbyConnectLoginEvent::Code { code, url }) => {
                                view.show_code(&code, &url)
                            }
                            Ok(EmbyConnectLoginEvent::Servers(servers)) => {
                                view.close();
                                complete(servers);
                                return;
                            }
                            Err(error) => {
                                show_error(&view.status, &error);
                                return;
                            }
                        }
                    }
                })
            });
            *dialogs.borrow_mut() = Some(dialog.downgrade());
        });
        let source = shell.products.source.clone();
        let parent = section.downgrade();
        let local = self.local.clone();
        let state = self.draft.clone();
        let id = source_id.clone();
        let dialogs = active_dialog;
        quick_connect.connect_clicked(move |_| {
            cancel();
            let Some(parent) = parent.upgrade() else {
                return;
            };
            let (address, trust) = {
                let local = local.borrow();
                (local.url.clone(), !local.cert_verify)
            };
            let (source, local, state, id) =
                (source.clone(), local.clone(), state.clone(), id.clone());
            let dialog = present_code_dialog(&parent, move |view| {
                let events = source.jellyfin_quick_connect(address.clone(), trust);
                let (source, local, state, id) =
                    (source.clone(), local.clone(), state.clone(), id.clone());
                gtk::glib::spawn_future_local(async move {
                    while let Ok(event) = events.recv().await {
                        match event {
                            Ok(JellyfinQuickConnectEvent::Code { code, url }) => {
                                view.show_code(&code, &url)
                            }
                            Ok(JellyfinQuickConnectEvent::Authorized(login)) => {
                                let source_name = optional_name(&local.borrow().name);
                                let use_instant_mix = state.borrow().use_instant_mix;
                                view.close();
                                if let Some(source_id) = id {
                                    source.update_source(
                                        SourceSettingsChange::JellyfinQuickConnect {
                                            source_id,
                                            login,
                                            source_name,
                                            use_instant_mix,
                                        },
                                    );
                                } else {
                                    source.configure_source(SourceSetup::JellyfinQuickConnect {
                                        login,
                                        source_name,
                                        use_instant_mix,
                                    });
                                }
                                return;
                            }
                            Err(error) => {
                                show_error(&view.status, &error);
                                return;
                            }
                        }
                    }
                })
            });
            *dialogs.borrow_mut() = Some(dialog.downgrade());
        });
        let source = shell.products.source.clone();
        let state = self.draft.clone();
        let submit_status = status.downgrade();
        let host_for_submit = host.clone();
        action.connect_clicked(move |button| {
            let draft = state.borrow();
            let use_instant_mix = draft.use_instant_mix;
            if source_id.is_none()
                && let Some(status) = submit_status.upgrade()
            {
                begin_connect_attempt(&status, button, &tr("Connecting to music server..."));
            }
            if !draft.manual
                && let Some(server) = draft.servers.get(draft.server).cloned()
            {
                let source_name = optional_name(&draft.name);
                if let Some(source_id) = source_id.clone() {
                    source.update_source(SourceSettingsChange::EmbyConnect {
                        source_id,
                        server,
                        source_name,
                        use_instant_mix,
                    });
                } else {
                    source.configure_source(SourceSetup::EmbyConnect {
                        server,
                        source_name,
                        use_instant_mix,
                    });
                }
            } else {
                let credentials = if !draft.manual {
                    let saved = saved_credentials
                        .as_ref()
                        .expect("saved Connect credentials");
                    CredentialInput {
                        source_name: optional_name(&draft.name),
                        server_url: saved.server_url.clone(),
                        username: saved.username.clone(),
                        secret: String::new(),
                        trust_invalid_cert: saved.trust_invalid_cert,
                    }
                } else {
                    host_for_submit.input()
                };
                if let Some(source_id) = source_id.clone() {
                    source.update_source(SourceSettingsChange::JellyfinEmby {
                        source_id,
                        credentials,
                        use_instant_mix,
                        connect_manually: draft.manual,
                    });
                } else {
                    source.configure_source(SourceSetup::JellyfinEmby {
                        kind: if emby {
                            sources::ServerKind::Emby
                        } else {
                            sources::ServerKind::Jellyfin
                        },
                        credentials,
                        use_instant_mix,
                    });
                }
            }
        });
        if let Some(context) = context {
            let state = self.draft.clone();
            let url = host.url.downgrade();
            let username = host.username.downgrade();
            *context.actions.borrow_mut() = Some(SetupActions {
                status: status.clone(),
                connect: action.clone(),
                ready: Rc::new(move || {
                    let state = state.borrow();
                    if state.manual {
                        url.upgrade()
                            .zip(username.upgrade())
                            .is_some_and(|(url, user)| {
                                server_address_ready(&url.text()) && !user.text().trim().is_empty()
                            })
                    } else {
                        !state.servers.is_empty()
                    }
                }),
            });
        }
        refresh();
        if self.draft.borrow().manual {
            discover();
        }
        section.upcast()
    }
}

fn optional_name(name: &str) -> Option<String> {
    (!name.trim().is_empty()).then(|| name.trim().into())
}

fn server_address_ready(address: &str) -> bool {
    let address = address.trim().trim_end_matches('/');
    !address
        .strip_prefix("http:")
        .or_else(|| address.strip_prefix("https:"))
        .unwrap_or(address)
        .is_empty()
}

fn show_error(status: &gtk::glib::WeakRef<gtk::Label>, error: &str) {
    if let Some(status) = status.upgrade() {
        status.set_text(error);
        status.set_visible(true);
    }
}

#[derive(Clone)]
struct CodeView {
    dialog: gtk::glib::WeakRef<adw::Dialog>,
    code: gtk::glib::WeakRef<gtk::Label>,
    approval: gtk::glib::WeakRef<gtk::LinkButton>,
    status: gtk::glib::WeakRef<gtk::Label>,
}

impl CodeView {
    fn show_code(&self, code: &str, url: &str) {
        if let Some(label) = self.code.upgrade() {
            label.set_text(code);
        }
        if let Some(link) = self.approval.upgrade() {
            link.set_uri(url);
            link.set_sensitive(true);
        }
        if let Some(status) = self.status.upgrade() {
            status.set_text("");
        }
    }

    fn close(&self) {
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.close();
        }
    }
}

fn present_code_dialog(
    parent: &gtk::Box,
    start: impl Fn(CodeView) -> gtk::glib::JoinHandle<()> + 'static,
) -> adw::Dialog {
    let resource = crate::ui_resource::CODE_LOGIN_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, { dialog: adw::Dialog, code_row: gtk::Box, code: gtk::Label, copy: gtk::Button, copy_icon: gtk::Image, approval: gtk::LinkButton, refresh: gtk::Button, status: gtk::Label });
    status.connect_label_notify(|label| label.set_visible(!label.text().is_empty()));
    let copy_button = copy.downgrade();
    let code_row = code_row.downgrade();
    let reset_icon = copy_icon.downgrade();
    code.connect_label_notify(move |label| {
        if let Some(row) = code_row.upgrade() {
            row.set_visible(!label.text().is_empty());
        }
        if let Some(button) = copy_button.upgrade() {
            button.set_sensitive(!label.text().is_empty());
        }
        if let Some(icon) = reset_icon.upgrade() {
            icon.set_icon_name(Some("rufin-edit-copy-symbolic"));
        }
    });
    let copy_code = code.downgrade();
    let copied_icon = copy_icon.downgrade();
    copy.connect_clicked(move |button| {
        if let Some(label) = copy_code.upgrade() {
            button.display().clipboard().set_text(&label.text());
            if let Some(icon) = copied_icon.upgrade() {
                icon.set_icon_name(Some("rufin-object-select-symbolic"));
            }
        }
    });
    let view = CodeView {
        dialog: dialog.downgrade(),
        code: code.downgrade(),
        approval: approval.downgrade(),
        status: status.downgrade(),
    };
    let task = Rc::new(RefCell::new(None::<gtk::glib::JoinHandle<()>>));
    let restart = {
        let task = task.clone();
        move || {
            if let Some(task) = task.borrow_mut().take() {
                task.abort();
            }
            if let Some(code) = view.code.upgrade() {
                code.set_text("");
            }
            if let Some(link) = view.approval.upgrade() {
                link.set_sensitive(false);
            }
            show_error(&view.status, &tr("Connecting to music server..."));
            *task.borrow_mut() = Some(start(view.clone()));
        }
    };
    restart();
    refresh.connect_clicked(move |_| restart());
    dialog.connect_closed(move |_| {
        if let Some(task) = task.borrow_mut().take() {
            task.abort();
        }
    });
    present_light_dismiss_dialog(&dialog, parent);
    dialog
}
