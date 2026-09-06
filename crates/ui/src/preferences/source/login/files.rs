use super::*;
use sources::{FileAuthentication, FileCredentials, FileCredentialsEdit, FileSourceSettings};

#[derive(Clone)]
struct Draft {
    name: String,
    settings: FileSourceSettings,
    secret: String,
    headers: String,
    alternates: String,
    folders: String,
    certificate: String,
    replace_headers: bool,
}

impl Draft {
    fn address(&self, smb: bool) -> Option<gtk::glib::Uri> {
        gtk::glib::Uri::parse(self.settings.url.trim(), gtk::glib::UriFlags::NONE)
            .ok()
            .filter(|url| {
                url.host().is_some_and(|host| !host.is_empty())
                    && url.userinfo().is_none()
                    && if smb {
                        url.scheme() == "smb"
                    } else {
                        matches!(url.scheme().as_str(), "http" | "https")
                    }
            })
    }

    fn ready(&self, smb: bool, editing: bool) -> bool {
        self.address(smb)
            .is_some_and(|url| !smb || url.path().split('/').any(|part| !part.is_empty()))
            && match self.settings.authentication {
                FileAuthentication::Anonymous => true,
                FileAuthentication::Password => {
                    !self.settings.username.trim().is_empty()
                        && (editing || !self.secret.is_empty())
                }
                FileAuthentication::Bearer => editing || !self.secret.is_empty(),
            }
    }

    fn new(saved: Option<&EditableSource>) -> Self {
        let settings = saved
            .and_then(|saved| saved.file_settings.clone())
            .unwrap_or(FileSourceSettings {
                url: String::new(),
                alternate_urls: vec![],
                folders: vec![],
                username: String::new(),
                domain: String::new(),
                authentication: FileAuthentication::Password,
                trust_invalid_certificate: false,
                certificate_pem: None,
                require_smb_encryption: false,
            });
        Self {
            name: saved.map_or(String::new(), |saved| saved.source.name.clone()),
            folders: settings.folders.join("\n"),
            alternates: settings.alternate_urls.join("\n"),
            certificate: settings.certificate_pem.clone().unwrap_or_default(),
            settings,
            secret: String::new(),
            headers: String::new(),
            replace_headers: saved.is_none(),
        }
    }

    fn input(&self) -> Result<(FileSourceSettings, FileCredentials), String> {
        let mut settings = self.settings.clone();
        settings.alternate_urls = self
            .alternates
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        settings.folders = self
            .folders
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        settings.certificate_pem =
            (!self.certificate.trim().is_empty()).then(|| self.certificate.clone());
        let headers = self
            .headers
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let (name, value) = line
                    .split_once(':')
                    .ok_or_else(|| tr("One Name: Value header per line"))?;
                Ok((name.trim().to_string(), value.trim().to_string()))
            })
            .collect::<Result<_, String>>()?;
        Ok((
            settings,
            FileCredentials {
                secret: self.secret.clone(),
                headers,
            },
        ))
    }
}

struct FileSetupFlow {
    presentation: &'static SourcePresentation,
    draft: Rc<RefCell<Draft>>,
}

pub(super) fn setup_flow(
    _: &Rc<Shell>,
    presentation: &'static SourcePresentation,
) -> Rc<dyn SourceSetupFlow> {
    Rc::new(FileSetupFlow {
        presentation,
        draft: Rc::new(RefCell::new(Draft::new(None))),
    })
}

impl SourceSetupFlow for FileSetupFlow {
    fn view(&self, shell: &Rc<Shell>, context: &SetupViewContext) -> gtk::Widget {
        let (scroller, content, _actions, status) =
            setup_scaffold(shell, context, self.presentation);
        let form = form(shell, &self.draft, self.presentation.kind, false, &status);
        content.append(&form.section);
        let draft = Rc::clone(&self.draft);
        let smb = self.presentation.kind == "smb";
        let ready: Rc<dyn Fn() -> bool> = Rc::new(move || draft.borrow().ready(smb, false));
        *context.actions.borrow_mut() = Some(SetupActions {
            status: status.clone(),
            connect: form.connect.clone(),
            ready,
        });
        let source = shell.products.source.clone();
        let draft = Rc::clone(&self.draft);
        let kind = self.presentation.kind;
        let title = self.presentation.title;
        form.connect.connect_clicked(move |button| {
            let value = draft.borrow().clone();
            match value.input() {
                Ok((settings, credentials)) => {
                    begin_connect_attempt(&status, button, &tr("Connecting to music server..."));
                    let name = if value.name.trim().is_empty() {
                        tr(title)
                    } else {
                        value.name.trim().into()
                    };
                    source.configure_source(if kind == "smb" {
                        SourceSetup::Smb {
                            name,
                            settings,
                            credentials,
                        }
                    } else {
                        SourceSetup::WebDav {
                            name,
                            settings,
                            credentials,
                        }
                    });
                }
                Err(error) => {
                    status.set_text(&error);
                    status.set_visible(true);
                }
            }
        });
        scroller.upcast()
    }
}

pub(super) fn settings_group(
    shell: &Rc<Shell>,
    saved: &EditableSource,
    presentation: &'static SourcePresentation,
) -> Result<gtk::Widget, String> {
    let draft = Rc::new(RefCell::new(Draft::new(Some(saved))));
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_visible(false);
    let form = form(shell, &draft, presentation.kind, true, &status);
    let source = shell.products.source.clone();
    let source_id = saved.source.id.clone();
    form.save.connect_clicked(move |_| {
        let value = draft.borrow().clone();
        match value.input() {
            Ok((settings, credentials)) => source.update_source(SourceSettingsChange::Files {
                source_id: source_id.clone(),
                name: value.name.trim().into(),
                settings,
                credentials: FileCredentialsEdit {
                    secret: (!credentials.secret.is_empty()).then_some(credentials.secret),
                    headers: value.replace_headers.then_some(credentials.headers),
                },
            }),
            Err(error) => {
                status.set_text(&error);
                status.set_visible(true);
            }
        }
    });
    Ok(form.section.upcast())
}

struct Form {
    section: gtk::Box,
    connect: gtk::Button,
    save: gtk::Button,
}

fn form(
    shell: &Rc<Shell>,
    draft: &Rc<RefCell<Draft>>,
    kind: &str,
    editing: bool,
    status: &gtk::Label,
) -> Form {
    let resource = crate::ui_resource::FILE_HOST_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        find_shares: gtk::Button, share: adw::ComboRow, share_options: gtk::StringList,
        section: gtk::Box, name: adw::EntryRow, url: adw::EntryRow,
        fields: gtk::Box, fields_group: adw::PreferencesGroup, status_host: gtk::Box,
        username: adw::EntryRow, password: adw::PasswordEntryRow, domain: adw::EntryRow,
        authentication: adw::ComboRow, authentication_options: gtk::StringList,
        encryption: adw::SwitchRow, cert_verify: adw::SwitchRow, nextcloud: gtk::Button,
        folders: gtk::TextView, alternates: gtk::TextView, headers: gtk::TextView, certificate: gtk::TextView,
        headers_group: adw::PreferencesRow, certificate_group: adw::PreferencesRow,
        replace_headers: adw::SwitchRow,
        saved_credential_hint: gtk::Label,
        connect: gtk::Button, save: gtk::Button
    });
    let smb = kind == "smb";
    fields_group.add(&install_compact_field_row_responsiveness_at(&fields, 440));
    status_host.append(status);
    find_shares.set_visible(smb && !editing);
    domain.set_visible(smb);
    encryption.set_visible(smb);
    cert_verify.set_visible(!smb);
    nextcloud.set_visible(!smb);
    headers_group.set_visible(!smb);
    certificate_group.set_visible(!smb);
    replace_headers.set_visible(editing);
    saved_credential_hint.set_visible(editing);
    save.set_visible(editing);
    connect.set_visible(!editing);
    if smb {
        authentication_options.splice(2, 1, &[]);
    }
    let value = draft.borrow().clone();
    let display_name = if value.name.is_empty() && !editing {
        tr(if smb { "SMB / Samba" } else { "WebDAV" })
    } else {
        value.name.clone()
    };
    name.set_text(&display_name);
    url.set_text(&value.settings.url);
    username.set_text(&value.settings.username);
    password.set_text(&value.secret);
    let submit = if editing { &save } else { &connect };
    connect_entry_row_activation(&name, submit);
    connect_entry_row_activation(&url, submit);
    connect_entry_row_activation(&username, submit);
    connect_entry_row_activation(&domain, submit);
    connect_password_entry_row_activation(&password, submit);
    domain.set_text(&value.settings.domain);
    authentication.set_selected(match value.settings.authentication {
        FileAuthentication::Password => 0,
        FileAuthentication::Anonymous => 1,
        FileAuthentication::Bearer => 2,
    });
    encryption.set_active(value.settings.require_smb_encryption);
    cert_verify.set_active(!value.settings.trust_invalid_certificate);
    folders.buffer().set_text(&value.folders);
    alternates.buffer().set_text(&value.alternates);
    headers.buffer().set_text(&value.headers);
    certificate.buffer().set_text(&value.certificate);
    replace_headers.set_active(value.replace_headers);
    headers.set_sensitive(value.replace_headers);
    bind_entry(&name, draft, |draft, value| draft.name = value);
    bind_entry(&url, draft, |draft, value| {
        draft.settings.url = value.trim().into()
    });
    bind_entry(&username, draft, |draft, value| {
        draft.settings.username = value
    });
    bind_entry(&domain, draft, |draft, value| draft.settings.domain = value);
    let state = Rc::clone(draft);
    password.connect_text_notify(move |row| state.borrow_mut().secret = row.text().into());
    let state = Rc::clone(draft);
    encryption.connect_active_notify(move |row| {
        state.borrow_mut().settings.require_smb_encryption = row.is_active()
    });
    let state = Rc::clone(draft);
    cert_verify.connect_active_notify(move |row| {
        state.borrow_mut().settings.trust_invalid_certificate = !row.is_active()
    });
    let state = Rc::clone(draft);
    let headers_view = headers.downgrade();
    replace_headers.connect_active_notify(move |row| {
        state.borrow_mut().replace_headers = row.is_active();
        if let Some(view) = headers_view.upgrade() {
            view.set_sensitive(row.is_active());
        }
    });
    bind_text(&folders, draft, |draft, value| draft.folders = value);
    bind_text(&alternates, draft, |draft, value| draft.alternates = value);
    bind_text(&headers, draft, |draft, value| draft.headers = value);
    bind_text(&certificate, draft, |draft, value| {
        draft.certificate = value
    });
    let update_auth = {
        let username = username.downgrade();
        let password = password.downgrade();
        let domain = domain.downgrade();
        let saved_credential_hint = saved_credential_hint.downgrade();
        move |selected| {
            if let Some(row) = username.upgrade() {
                row.set_visible(selected == 0);
            }
            if let Some(row) = password.upgrade() {
                row.set_visible(selected != 1);
                row.set_title(&tr(if selected == 2 {
                    "Bearer token"
                } else {
                    "Password"
                }));
            }
            if let Some(row) = domain.upgrade() {
                row.set_sensitive(selected == 0);
            }
            if let Some(hint) = saved_credential_hint.upgrade() {
                hint.set_visible(editing && selected != 1);
            }
        }
    };
    update_auth(authentication.selected());
    let state = Rc::clone(draft);
    authentication.connect_selected_notify(move |row| {
        state.borrow_mut().settings.authentication = match row.selected() {
            1 => FileAuthentication::Anonymous,
            2 => FileAuthentication::Bearer,
            _ => FileAuthentication::Password,
        };
        update_auth(row.selected());
    });
    let update_ready: Rc<dyn Fn()> = {
        let state = Rc::clone(draft);
        let submit = submit.downgrade();
        let nextcloud = nextcloud.downgrade();
        let find_shares = find_shares.downgrade();
        Rc::new(move || {
            let value = state.borrow();
            if let Some(button) = submit.upgrade() {
                button.set_sensitive(value.ready(smb, editing));
            }
            if let Some(button) = nextcloud.upgrade() {
                button.set_sensitive(value.address(false).is_some());
            }
            if let Some(button) = find_shares.upgrade() {
                button.set_sensitive(
                    value.address(true).is_some()
                        && (value.settings.authentication == FileAuthentication::Anonymous
                            || !value.settings.username.trim().is_empty()
                                && !value.secret.is_empty()),
                );
            }
        })
    };
    update_ready();
    for row in [&url, &username] {
        let update = Rc::clone(&update_ready);
        row.connect_text_notify(move |_| update());
    }
    let update = Rc::clone(&update_ready);
    password.connect_text_notify(move |_| update());
    authentication.connect_selected_notify(move |_| update_ready());
    let shares = Rc::new(RefCell::new(Vec::<String>::new()));
    let addresses = Rc::clone(&shares);
    let address_row = url.downgrade();
    share.connect_selected_notify(move |row| {
        let address = addresses.borrow().get(row.selected() as usize).cloned();
        if let Some(address) = address
            && let Some(row) = address_row.upgrade()
        {
            row.set_text(&address);
        }
    });
    let listing = Rc::new(RefCell::new(None::<gtk::glib::JoinHandle<()>>));
    let cancel = Rc::clone(&listing);
    section.connect_unmap(move |_| {
        if let Some(task) = cancel.borrow_mut().take() {
            task.abort();
        }
    });
    let source = shell.products.source.clone();
    let state = Rc::clone(draft);
    let list_status = status.downgrade();
    let share = share.downgrade();
    let share_options = share_options.downgrade();
    find_shares.connect_clicked(move |button| {
        let (settings, credentials) = match state.borrow().input() {
            Ok(input) => input,
            Err(error) => {
                if let Some(status) = list_status.upgrade() {
                    status.set_text(&error);
                    status.set_visible(true);
                }
                return;
            }
        };
        if let Some(task) = listing.borrow_mut().take() {
            task.abort();
        }
        let events = source.smb_shares(settings, credentials);
        let (share, share_options, status, shares) = (
            share.clone(),
            share_options.clone(),
            list_status.clone(),
            Rc::clone(&shares),
        );
        button.set_sensitive(false);
        let button = button.downgrade();
        *listing.borrow_mut() = Some(gtk::glib::spawn_future_local(async move {
            if let Ok(result) = events.recv().await {
                match result {
                    Ok(values) => {
                        *shares.borrow_mut() =
                            values.iter().map(|(_, address)| address.clone()).collect();
                        if let Some(row) = share.upgrade() {
                            row.set_selected(gtk::INVALID_LIST_POSITION);
                        }
                        if let Some(options) = share_options.upgrade() {
                            options.splice(
                                0,
                                options.n_items(),
                                &values
                                    .iter()
                                    .map(|(name, _)| name.as_str())
                                    .collect::<Vec<_>>(),
                            );
                        }
                        if let Some(row) = share.upgrade() {
                            row.set_visible(!values.is_empty());
                        }
                    }
                    Err(error) => {
                        if let Some(status) = status.upgrade() {
                            status.set_text(&error);
                            status.set_visible(true);
                        }
                    }
                }
            }
            if let Some(button) = button.upgrade() {
                button.set_sensitive(true);
            }
        }));
    });
    let authorization = Rc::new(RefCell::new(None::<gtk::glib::JoinHandle<()>>));
    let cancel = Rc::clone(&authorization);
    section.connect_unmap(move |_| {
        if let Some(task) = cancel.borrow_mut().take() {
            task.abort();
        }
    });
    let state = Rc::clone(draft);
    let source = shell.products.source.clone();
    let status = status.downgrade();
    let connect_after_login = connect.downgrade();
    let url = url.downgrade();
    let username = username.downgrade();
    let password = password.downgrade();
    let authentication = authentication.downgrade();
    nextcloud.connect_clicked(move |button| {
        let input = state.borrow().input();
        let (settings, credentials) = match input {
            Ok(input) => input,
            Err(error) => {
                if let Some(status) = status.upgrade() {
                    status.set_text(&error);
                    status.set_visible(true);
                }
                return;
            }
        };
        if let Some(task) = authorization.borrow_mut().take() {
            task.abort();
        }
        let events = source.nextcloud_login(settings, credentials);
        let status = status.clone();
        if let Some(status) = status.upgrade() {
            status.set_text(&tr("Connecting to music server..."));
            status.set_visible(true);
        }
        let connect_after_login = connect_after_login.clone();
        let (url, username, password, authentication) = (
            url.clone(),
            username.clone(),
            password.clone(),
            authentication.clone(),
        );
        let button = button.downgrade();
        if let Some(button) = button.upgrade() {
            button.set_sensitive(false);
        }
        *authorization.borrow_mut() = Some(gtk::glib::spawn_future_local(async move {
            while let Ok(event) = events.recv().await {
                match event {
                    Ok(crate::runtime::source::NextcloudLoginEvent::OpenBrowser(address)) => {
                        if let Some(status) = status.upgrade() {
                            status.set_text(&tr("Complete sign-in in your browser"));
                            status.set_visible(true);
                        }
                        if let Err(error) = gtk::gio::AppInfo::launch_default_for_uri_future(
                            &address,
                            None::<&gtk::gio::AppLaunchContext>,
                        )
                        .await
                        {
                            if let Some(status) = status.upgrade() {
                                status.set_text(&error.to_string());
                                status.set_visible(true);
                            }
                            break;
                        }
                    }
                    Ok(crate::runtime::source::NextcloudLoginEvent::Authorized {
                        settings,
                        credentials,
                    }) => {
                        if let Some(row) = url.upgrade() {
                            row.set_text(&settings.url);
                        }
                        if let Some(row) = username.upgrade() {
                            row.set_text(&settings.username);
                        }
                        if let Some(row) = password.upgrade() {
                            row.set_text(&credentials.secret);
                        }
                        if let Some(row) = authentication.upgrade() {
                            row.set_selected(0);
                        }
                        if editing {
                            if let Some(status) = status.upgrade() {
                                status.set_text(&tr_with(
                                    "Connected as {username}",
                                    &[("username", &settings.username)],
                                ));
                            }
                        } else if let Some(button) = connect_after_login.upgrade() {
                            button.emit_clicked();
                        }
                    }
                    Err(error) => {
                        if let Some(status) = status.upgrade() {
                            status.set_text(&error);
                            status.set_visible(true);
                        }
                    }
                }
            }
            if let Some(button) = button.upgrade() {
                button.set_sensitive(true);
            }
        }));
    });
    Form {
        section,
        connect,
        save,
    }
}

fn bind_entry(row: &adw::EntryRow, draft: &Rc<RefCell<Draft>>, write: fn(&mut Draft, String)) {
    let draft = Rc::clone(draft);
    row.connect_text_notify(move |row| write(&mut draft.borrow_mut(), row.text().into()));
}

fn bind_text(view: &gtk::TextView, draft: &Rc<RefCell<Draft>>, write: fn(&mut Draft, String)) {
    let draft = Rc::clone(draft);
    view.buffer().connect_changed(move |buffer| {
        write(
            &mut draft.borrow_mut(),
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .into(),
        )
    });
}
