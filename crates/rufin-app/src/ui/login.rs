use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use crate::controller::LoginRequest;
use crate::i18n::tr;
use crate::providers::StreamingProvider;
use adw::prelude::*;

use super::{
    Shell, icon_button,
    layout::{large_popup_content_height, large_popup_content_width},
    text_button,
};

const ADD_SERVER_DIALOG_WIDTH: i32 = 620;
const ADD_SERVER_DIALOG_HEIGHT: i32 = 680;
const ADD_SERVER_CLAMP_WIDTH: i32 = 560;

impl Shell {
    pub(super) fn present_add_server_dialog(self: &Rc<Self>) {
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new(&tr("Add Server"), "");
        header.set_title_widget(Some(&title));
        let close = icon_button("window-close-symbolic", "Close");
        header.pack_end(&close);
        toolbar.add_top_bar(&header);

        let dialog = adw::Dialog::builder()
            .content_width(large_popup_content_width(ADD_SERVER_DIALOG_WIDTH))
            .content_height(large_popup_content_height(
                self.window.height(),
                ADD_SERVER_DIALOG_HEIGHT,
            ))
            .build();
        let dialog_for_connect = dialog.clone();
        let child = self.add_server_view_with_success_handler(Some(Rc::new(move || {
            dialog_for_connect.close();
        })));
        toolbar.set_content(Some(&child));
        dialog.set_child(Some(&toolbar));
        let dialog_for_close = dialog.clone();
        close.connect_clicked(move |_| {
            dialog_for_close.close();
        });
        dialog.present(Some(&self.window));
    }

    pub(super) fn add_server_view(self: &Rc<Self>) -> gtk::Widget {
        self.add_server_view_with_success_handler(None)
    }

    fn add_server_view_with_success_handler(
        self: &Rc<Self>,
        on_connect_succeeded: Option<Rc<dyn Fn()>>,
    ) -> gtk::Widget {
        if self.state.first_run_connection_pending.get() {
            return self.connection_progress_view();
        }

        self.start_server_discovery_once();

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);

        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(large_popup_content_width(ADD_SERVER_CLAMP_WIDTH));
        clamp.set_tightening_threshold(360);
        clamp.set_margin_top(36);
        clamp.set_margin_bottom(36);
        clamp.set_margin_start(24);
        clamp.set_margin_end(24);
        clamp.set_valign(gtk::Align::Start);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("first-run-content");
        content.set_hexpand(true);

        let intro = gtk::Box::new(gtk::Orientation::Vertical, 6);
        intro.set_margin_bottom(4);
        let intro_title = gtk::Label::new(Some(&tr("Connect to Music Server")));
        intro_title.add_css_class("title-1");
        intro_title.set_xalign(0.0);
        intro_title.set_wrap(true);
        let intro_description = gtk::Label::new(Some(&tr(
            "Choose a provider, pick a discovered server, or enter the address manually",
        )));
        intro_description.add_css_class("muted");
        intro_description.set_xalign(0.0);
        intro_description.set_wrap(true);
        intro.append(&intro_title);
        intro.append(&intro_description);
        content.append(&intro);

        let provider_titles = StreamingProvider::ALL
            .iter()
            .map(|provider| tr(provider.title()))
            .collect::<Vec<_>>();
        let provider_title_refs = provider_titles
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let provider_options = gtk::StringList::new(&provider_title_refs);
        let provider = adw::ComboRow::builder()
            .title(tr("Provider"))
            .model(&provider_options)
            .selected(0)
            .build();
        let url = adw::EntryRow::builder().title(tr("Server Address")).build();
        url.set_text("http://");
        let username = adw::EntryRow::builder().title(tr("Username")).build();
        let password = adw::PasswordEntryRow::builder()
            .title(tr("Password"))
            .build();
        let trust = adw::SwitchRow::builder()
            .title(tr("Trust invalid certificate"))
            .subtitle(tr("Only use this for a server you control"))
            .active(false)
            .build();

        let server_group = adw::PreferencesGroup::builder().title(tr("Server")).build();
        server_group.add(&provider);
        server_group.add(&url);
        server_group.add(&username);
        server_group.add(&password);
        server_group.add(&trust);
        content.append(&server_group);

        let default_local_folder = default_music_folder();
        let local_folders = Rc::new(RefCell::new(
            default_local_folder.clone().into_iter().collect::<Vec<_>>(),
        ));
        let local_group = adw::PreferencesGroup::builder()
            .title(tr("Local Library"))
            .description(tr(
                "Choose one or more folders to scan and play directly from this computer",
            ))
            .build();
        let local_folder_row = adw::ActionRow::builder()
            .title(tr("Music Folders"))
            .subtitle(local_folders_subtitle(&local_folders.borrow()))
            .build();
        let local_folder_button = gtk::Button::with_label(&tr("Choose"));
        local_folder_button.set_valign(gtk::Align::Center);
        local_folder_row.add_suffix(&local_folder_button);
        local_folder_row.set_activatable_widget(Some(&local_folder_button));
        local_group.add(&local_folder_row);
        let add_local_folder_row = adw::ActionRow::builder()
            .title(tr("Add Folder"))
            .subtitle(tr("Add another folder to the Local source"))
            .build();
        let add_local_folder_button = gtk::Button::with_label(&tr("Add"));
        add_local_folder_button.set_valign(gtk::Align::Center);
        add_local_folder_row.add_suffix(&add_local_folder_button);
        add_local_folder_row.set_activatable_widget(Some(&add_local_folder_button));
        local_group.add(&add_local_folder_row);
        local_group.set_visible(false);
        content.append(&local_group);

        let discovered_group = self.discovered_servers_group(&provider, &url);
        content.append(&discovered_group);

        let status_text = self.state.library.borrow().sync_status.clone();
        let status = gtk::Label::new(Some(&status_text));
        status.add_css_class("muted");
        status.set_wrap(true);
        status.set_xalign(0.0);
        status.set_visible(!status_text.trim().is_empty());
        if let Some(error) = &self.state.library.borrow().last_error {
            status.set_text(error);
            status.add_css_class("error-text");
            status.set_visible(true);
        }

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        actions.set_halign(gtk::Align::End);
        let login = text_button("network-server-symbolic", "Connect");
        login.add_css_class("suggested-action");
        connect_entry_row_activation(&url, &login);
        connect_entry_row_activation(&username, &login);
        connect_password_entry_row_activation(&password, &login);
        provider.add_controller(local_provider_enter_controller(&provider, &login));
        let controller = self.controller.clone();
        let url_input = url.clone();
        let username_input = username.clone();
        let password_input = password.clone();
        let trust_input = trust.clone();
        let provider_input = provider.clone();
        let local_folders_input = Rc::clone(&local_folders);
        let status_input = status.clone();
        let shell = Rc::clone(self);
        let connect_attempt_started = Rc::new(Cell::new(false));
        let connect_attempt_started_for_click = Rc::clone(&connect_attempt_started);
        let login_for_click = login.clone();
        login.connect_clicked(move |_| {
            let provider = StreamingProvider::from_index(provider_input.selected());
            if provider == StreamingProvider::Local {
                let roots = local_folders_input.borrow().clone();
                if roots.is_empty() {
                    status_input.set_text(&tr("Choose at least one local music folder"));
                    status_input.set_visible(true);
                    return;
                }
                let message = tr("Caching local library...");
                connect_attempt_started_for_click.set(true);
                status_input.remove_css_class("error-text");
                status_input.set_text(&message);
                status_input.set_visible(true);
                login_for_click.set_sensitive(false);
                shell.begin_first_run_connection(&message);
                controller.add_local_server_folders(roots);
            } else {
                if !remote_login_ready(&url_input, &username_input, &password_input) {
                    status_input.set_text(&tr("Enter a server address, username, and password"));
                    status_input.set_visible(true);
                    return;
                }
                let message = tr("Connecting to music server...");
                connect_attempt_started_for_click.set(true);
                status_input.remove_css_class("error-text");
                status_input.set_text(&message);
                status_input.set_visible(true);
                login_for_click.set_sensitive(false);
                shell.begin_first_run_connection(&message);
                controller.login(LoginRequest {
                    provider,
                    server_url: url_input.text().to_string(),
                    username: username_input.text().to_string(),
                    password: password_input.text().to_string(),
                    trust_invalid_cert: trust_input.is_active(),
                    local_access_root: None,
                    path_replace_from: None,
                });
            }
        });
        connect_add_server_status_watcher(AddServerStatusWatcher {
            shell: self,
            status: &status,
            login: &login,
            provider: &provider,
            local_folders: &local_folders,
            url: &url,
            username: &username,
            password: &password,
            connect_attempt_started,
            on_connect_succeeded,
        });
        actions.append(&login);
        content.append(&actions);

        content.append(&status);

        let remote_widgets = vec![
            url.clone().upcast::<gtk::Widget>(),
            username.clone().upcast::<gtk::Widget>(),
            password.clone().upcast::<gtk::Widget>(),
            trust.clone().upcast::<gtk::Widget>(),
            discovered_group.clone().upcast::<gtk::Widget>(),
        ];
        update_provider_rows(
            StreamingProvider::from_index(provider.selected()),
            &remote_widgets,
            &local_group,
        );
        update_connect_button(
            StreamingProvider::from_index(provider.selected()),
            &local_folders,
            &url,
            &username,
            &password,
            &login,
        );
        let refresh_connect_button: Rc<dyn Fn()> = Rc::new({
            let provider = provider.clone();
            let local_folders = Rc::clone(&local_folders);
            let url = url.clone();
            let username = username.clone();
            let password = password.clone();
            let login = login.clone();
            move || {
                update_connect_button(
                    StreamingProvider::from_index(provider.selected()),
                    &local_folders,
                    &url,
                    &username,
                    &password,
                    &login,
                );
            }
        });
        let local_group_for_provider = local_group.clone();
        let refresh_for_provider = Rc::clone(&refresh_connect_button);
        provider.connect_selected_notify(move |row| {
            let provider = StreamingProvider::from_index(row.selected());
            update_provider_rows(provider, &remote_widgets, &local_group_for_provider);
            refresh_for_provider();
        });
        for entry in [
            url.clone().upcast::<gtk::Editable>(),
            username.clone().upcast::<gtk::Editable>(),
            password.clone().upcast::<gtk::Editable>(),
        ] {
            let refresh = Rc::clone(&refresh_connect_button);
            entry.connect_text_notify(move |_| {
                refresh();
            });
        }

        connect_folder_button(
            &self.window,
            &local_folder_button,
            &local_folder_row,
            Rc::new(RefCell::new(default_local_folder)),
            {
                let login = login.clone();
                let provider = provider.clone();
                let local_folders = Rc::clone(&local_folders);
                let local_folder_row = local_folder_row.clone();
                let url = url.clone();
                let username = username.clone();
                let password = password.clone();
                move |path| {
                    replace_primary_local_folder(&local_folders, path);
                    local_folder_row.set_subtitle(&local_folders_subtitle(&local_folders.borrow()));
                    update_connect_button(
                        StreamingProvider::from_index(provider.selected()),
                        &local_folders,
                        &url,
                        &username,
                        &password,
                        &login,
                    );
                }
            },
        );
        connect_add_local_folder_button(
            &self.window,
            &add_local_folder_button,
            &local_folder_row,
            Rc::clone(&local_folders),
            {
                let login = login.clone();
                let provider = provider.clone();
                let local_folders = Rc::clone(&local_folders);
                let url = url.clone();
                let username = username.clone();
                let password = password.clone();
                move || {
                    update_connect_button(
                        StreamingProvider::from_index(provider.selected()),
                        &local_folders,
                        &url,
                        &username,
                        &password,
                        &login,
                    );
                }
            },
        );
        clamp.set_child(Some(&content));
        scroller.set_child(Some(&clamp));
        scroller.upcast()
    }

    fn begin_first_run_connection(self: &Rc<Self>, status: &str) {
        self.state.first_run_connection_pending.set(true);
        self.state.first_run_connection_ready.set(false);
        {
            let mut library = self.state.library.borrow_mut();
            library.sync_status = status.to_string();
            library.last_error = None;
        }
        self.render_current_route();
    }

    fn connection_progress_view(self: &Rc<Self>) -> gtk::Widget {
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);

        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(440);
        clamp.set_tightening_threshold(320);
        clamp.set_margin_top(72);
        clamp.set_margin_bottom(72);
        clamp.set_margin_start(24);
        clamp.set_margin_end(24);
        clamp.set_valign(gtk::Align::Center);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        content.add_css_class("first-run-progress");
        content.set_halign(gtk::Align::Center);
        content.set_valign(gtk::Align::Center);
        content.set_hexpand(true);

        let spinner = gtk::Spinner::new();
        spinner.set_halign(gtk::Align::Center);
        spinner.start();
        content.append(&spinner);

        let title = gtk::Label::new(Some(&tr("Caching Library")));
        title.add_css_class("title-1");
        title.set_justify(gtk::Justification::Center);
        title.set_wrap(true);
        content.append(&title);

        let status_text = self.state.library.borrow().sync_status.clone();
        let status = gtk::Label::new(Some(&status_text));
        status.add_css_class("muted");
        status.set_justify(gtk::Justification::Center);
        status.set_wrap(true);
        status.set_xalign(0.5);
        content.append(&status);

        clamp.set_child(Some(&content));
        scroller.set_child(Some(&clamp));
        scroller.upcast()
    }

    fn start_server_discovery_once(&self) {
        if self.state.server_discovery_started.replace(true) {
            return;
        }
        self.state.server_discovery_running.set(true);
        *self.state.server_discovery_status.borrow_mut() =
            "Searching for Jellyfin servers on the local network...".to_string();
        self.controller.discover_servers();
    }

    fn refresh_server_discovery(self: &Rc<Self>) {
        if self.state.server_discovery_running.get() {
            return;
        }
        self.state.server_discovery_running.set(true);
        *self.state.discovered_servers.borrow_mut() = Vec::new();
        *self.state.server_discovery_status.borrow_mut() =
            "Searching for Jellyfin servers on the local network...".to_string();
        self.controller.discover_servers();
        self.render_current_route();
    }

    fn discovered_servers_group(
        self: &Rc<Self>,
        provider: &adw::ComboRow,
        url: &adw::EntryRow,
    ) -> adw::PreferencesGroup {
        let status = self.state.server_discovery_status.borrow().clone();
        let running = self.state.server_discovery_running.get();
        let servers = self.state.discovered_servers.borrow().clone();
        let group = adw::PreferencesGroup::builder()
            .title(tr("Found Servers"))
            .description(status)
            .build();

        if servers.is_empty() {
            let row_title = if running {
                tr("Searching Local Network")
            } else {
                tr("No Servers Found")
            };
            let row = adw::ActionRow::builder().title(row_title).build();
            row.add_prefix(&gtk::Image::from_icon_name("network-server-symbolic"));
            if running {
                let spinner = gtk::Spinner::new();
                spinner.start();
                row.add_suffix(&spinner);
            }
            group.add(&row);
        } else {
            for server in servers {
                let subtitle = format!("{} - {}", server.provider, server.address);
                let row = adw::ActionRow::builder()
                    .title(server.name)
                    .subtitle(subtitle)
                    .build();
                row.add_prefix(&gtk::Image::from_icon_name("network-server-symbolic"));
                row.set_activatable(true);
                let provider = provider.clone();
                let url = url.clone();
                let address = server.address;
                row.connect_activated(move |_| {
                    provider.set_selected(0);
                    url.set_text(&address);
                });
                group.add(&row);
            }
        }

        let search_title = if running {
            tr("Searching...")
        } else {
            tr("Search Again")
        };
        let search = adw::ButtonRow::builder()
            .title(search_title)
            .start_icon_name("view-refresh-symbolic")
            .build();
        search.set_sensitive(!running);
        let shell = Rc::clone(self);
        search.connect_activated(move |_| {
            shell.refresh_server_discovery();
        });
        group.add(&search);

        group
    }
}

fn update_provider_rows(
    provider: StreamingProvider,
    remote_widgets: &[gtk::Widget],
    local_group: &adw::PreferencesGroup,
) {
    let local = provider == StreamingProvider::Local;
    for widget in remote_widgets {
        widget.set_visible(!local);
    }
    local_group.set_visible(local);
}

fn update_connect_button(
    provider: StreamingProvider,
    local_folders: &Rc<RefCell<Vec<PathBuf>>>,
    url: &adw::EntryRow,
    username: &adw::EntryRow,
    password: &adw::PasswordEntryRow,
    login: &gtk::Button,
) {
    let ready = if provider == StreamingProvider::Local {
        !local_folders.borrow().is_empty()
    } else {
        remote_login_ready(url, username, password)
    };
    login.set_sensitive(ready);
}

struct AddServerStatusWatcher<'a> {
    shell: &'a Rc<Shell>,
    status: &'a gtk::Label,
    login: &'a gtk::Button,
    provider: &'a adw::ComboRow,
    local_folders: &'a Rc<RefCell<Vec<PathBuf>>>,
    url: &'a adw::EntryRow,
    username: &'a adw::EntryRow,
    password: &'a adw::PasswordEntryRow,
    connect_attempt_started: Rc<Cell<bool>>,
    on_connect_succeeded: Option<Rc<dyn Fn()>>,
}

fn connect_add_server_status_watcher(watcher: AddServerStatusWatcher<'_>) {
    let AddServerStatusWatcher {
        shell,
        status,
        login,
        provider,
        local_folders,
        url,
        username,
        password,
        connect_attempt_started,
        on_connect_succeeded,
    } = watcher;
    let shell = Rc::clone(shell);
    let status = status.clone();
    let login = login.clone();
    let provider = provider.clone();
    let local_folders = Rc::clone(local_folders);
    let url = url.clone();
    let username = username.clone();
    let password = password.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
        if status.root().is_none() {
            return gtk::glib::ControlFlow::Break;
        }

        let pending = shell.state.first_run_connection_pending.get();
        let (sync_status, last_error) = {
            let library = shell.state.library.borrow();
            (library.sync_status.clone(), library.last_error.clone())
        };

        if pending {
            status.remove_css_class("error-text");
            status.set_text(&sync_status);
            status.set_visible(!sync_status.trim().is_empty());
            login.set_sensitive(false);
            return gtk::glib::ControlFlow::Continue;
        }

        if let Some(error) = last_error {
            connect_attempt_started.set(false);
            status.set_text(&error);
            status.add_css_class("error-text");
            status.set_visible(true);
            update_connect_button(
                StreamingProvider::from_index(provider.selected()),
                &local_folders,
                &url,
                &username,
                &password,
                &login,
            );
            return gtk::glib::ControlFlow::Continue;
        }

        if connect_attempt_started.get() {
            if let Some(on_connect_succeeded) = on_connect_succeeded.as_ref() {
                on_connect_succeeded();
            }
            return gtk::glib::ControlFlow::Break;
        }

        gtk::glib::ControlFlow::Continue
    });
}

fn connect_entry_row_activation(entry: &adw::EntryRow, login: &gtk::Button) {
    let login = login.clone();
    entry.connect_entry_activated(move |_| {
        activate_connect_if_ready(&login);
    });
}

fn connect_password_entry_row_activation(entry: &adw::PasswordEntryRow, login: &gtk::Button) {
    let login = login.clone();
    entry.connect_entry_activated(move |_| {
        activate_connect_if_ready(&login);
    });
}

fn local_provider_enter_controller(
    provider: &adw::ComboRow,
    login: &gtk::Button,
) -> gtk::EventControllerKey {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let login = login.clone();
    let provider = provider.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        let local = StreamingProvider::from_index(provider.selected()) == StreamingProvider::Local;
        let enter = key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter;
        if local && enter && activate_connect_if_ready(&login) {
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    controller
}

fn activate_connect_if_ready(login: &gtk::Button) -> bool {
    if !login.is_sensitive() {
        return false;
    }
    login.emit_clicked();
    true
}

fn remote_login_ready(
    url: &adw::EntryRow,
    username: &adw::EntryRow,
    password: &adw::PasswordEntryRow,
) -> bool {
    let address = url.text();
    let address = address.trim().trim_end_matches('/');
    let address_without_scheme = address
        .strip_prefix("http://")
        .or_else(|| address.strip_prefix("https://"))
        .unwrap_or(address);
    !address_without_scheme.trim().is_empty()
        && !username.text().trim().is_empty()
        && !password.text().trim().is_empty()
}

fn default_music_folder() -> Option<PathBuf> {
    let user_dirs = directories::UserDirs::new()?;
    Some(
        user_dirs
            .audio_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| user_dirs.home_dir().join("Music")),
    )
}

fn path_subtitle(path: &Path) -> String {
    path.display().to_string()
}

fn local_folders_subtitle(folders: &[PathBuf]) -> String {
    match folders {
        [] => tr("No folders selected"),
        [folder] => path_subtitle(folder),
        folders => format!("{} {}", folders.len(), tr("folders selected")),
    }
}

fn replace_primary_local_folder(folders: &Rc<RefCell<Vec<PathBuf>>>, path: PathBuf) {
    let mut folders = folders.borrow_mut();
    if let Some(index) = folders.iter().position(|folder| folder == &path) {
        if index != 0 {
            folders.remove(index);
            folders.insert(0, path);
        }
        return;
    }
    if folders.is_empty() {
        folders.push(path);
    } else {
        folders[0] = path;
    }
}

fn append_local_folder(folders: &Rc<RefCell<Vec<PathBuf>>>, path: PathBuf) {
    let mut folders = folders.borrow_mut();
    if !folders.iter().any(|folder| folder == &path) {
        folders.push(path);
    }
}

pub(super) fn connect_folder_button(
    window: &adw::ApplicationWindow,
    button: &gtk::Button,
    row: &adw::ActionRow,
    target: Rc<RefCell<Option<PathBuf>>>,
    on_changed: impl Fn(PathBuf) + 'static,
) {
    let window = window.clone();
    let row = row.clone();
    let on_changed: Rc<dyn Fn(PathBuf)> = Rc::new(on_changed);
    button.connect_clicked(move |_| {
        let window = window.clone();
        let row = row.clone();
        let target = Rc::clone(&target);
        let on_changed = Rc::clone(&on_changed);
        gtk::glib::spawn_future_local(async move {
            let selected_folder = target.borrow().as_ref().map(gtk::gio::File::for_path);
            let dialog = gtk::FileDialog::builder()
                .title(tr("Select Music Folder"))
                .build();
            if let Some(folder) = selected_folder.as_ref() {
                dialog.set_initial_folder(Some(folder));
            }
            let Ok(folder) = dialog.select_folder_future(Some(&window)).await else {
                return;
            };
            let Some(path) = folder.path() else {
                return;
            };
            row.set_subtitle(&path_subtitle(&path));
            *target.borrow_mut() = Some(path);
            if let Some(path) = target.borrow().as_ref() {
                on_changed(path.clone());
            }
        });
    });
}

fn connect_add_local_folder_button(
    window: &adw::ApplicationWindow,
    button: &gtk::Button,
    row: &adw::ActionRow,
    folders: Rc<RefCell<Vec<PathBuf>>>,
    on_changed: impl Fn() + 'static,
) {
    let window = window.clone();
    let row = row.clone();
    let on_changed: Rc<dyn Fn()> = Rc::new(on_changed);
    button.connect_clicked(move |_| {
        let window = window.clone();
        let row = row.clone();
        let folders = Rc::clone(&folders);
        let on_changed = Rc::clone(&on_changed);
        gtk::glib::spawn_future_local(async move {
            let selected_folder = folders.borrow().last().map(gtk::gio::File::for_path);
            let dialog = gtk::FileDialog::builder()
                .title(tr("Select Music Folder"))
                .build();
            if let Some(folder) = selected_folder.as_ref() {
                dialog.set_initial_folder(Some(folder));
            }
            let Ok(folder) = dialog.select_folder_future(Some(&window)).await else {
                return;
            };
            let Some(path) = folder.path() else {
                return;
            };
            append_local_folder(&folders, path);
            row.set_subtitle(&local_folders_subtitle(&folders.borrow()));
            on_changed();
        });
    });
}
