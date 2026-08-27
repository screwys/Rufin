use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::runtime::source::{
    CredentialInput, CredentialPreset, LocalAccessStatus, OpenSubsonicAuthentication, SourceHandle,
    SourceLocalAccess, SourceSummary,
};
use adw::prelude::*;
use gtk::{gio, glib};

use localization::{tr, trn_with};
use sources::SourceId;

use super::field_layout::{
    compact_field_row_group, install_compact_field_row_responsiveness,
    install_compact_field_row_responsiveness_at, style_compact_field_row,
};
use super::login::{
    connect_folder_button, open_subsonic_authentication_switch, source_kind_title,
    source_settings_group,
};
use crate::layout::large_popup_content_width;
use crate::player::state::current_playback_track;
use crate::shell::Shell;
use crate::shell::actions::text_button;

const MANAGE_SERVER_CLAMP_WIDTH: i32 = 560;
const METADATA_RECOVERY_FIELD_STACK_WIDTH: i32 = 520;
const METADATA_RECOVERY_COLUMN_SPACING: i32 = 18;
const METADATA_RECOVERY_ROW_SPACING: i32 = 14;

#[derive(Clone)]
struct ManageServerExitSlot {
    navigation: glib::WeakRef<adw::NavigationView>,
    on_close: Rc<dyn Fn()>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum LocalAccessOperation {
    #[default]
    Editing,
    Pending,
    Failed(String),
}

#[derive(Debug, Eq, PartialEq)]
struct LocalAccessRecoveryView {
    controls_sensitive: bool,
    continue_sensitive: bool,
    checking: bool,
    message: String,
}

struct LocalAccessEditor {
    source: SourceHandle,
    source_id: SourceId,
    folder: Rc<RefCell<Option<PathBuf>>>,
    server_prefix: glib::WeakRef<adw::EntryRow>,
    local_prefix: Option<glib::WeakRef<adw::EntryRow>>,
    sample_source_path: Option<String>,
    operation: RefCell<LocalAccessOperation>,
    on_success: Rc<dyn Fn()>,
}

impl LocalAccessEditor {
    fn new(
        shell: &Rc<Shell>,
        source_id: SourceId,
        folder: Option<PathBuf>,
        server_prefix: &adw::EntryRow,
        local_prefix: Option<&adw::EntryRow>,
        sample_source_path: Option<String>,
        on_success: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        Rc::new(Self {
            source: shell.products.source.clone(),
            source_id,
            folder: Rc::new(RefCell::new(folder)),
            server_prefix: server_prefix.downgrade(),
            local_prefix: local_prefix.map(|row| row.downgrade()),
            sample_source_path,
            operation: RefCell::new(LocalAccessOperation::Editing),
            on_success,
        })
    }

    fn draft(&self) -> LocalAccessDraft {
        LocalAccessDraft {
            folder: self.folder.borrow().clone(),
            server_prefix: self
                .server_prefix
                .upgrade()
                .map(|row| row.text().trim().to_string())
                .unwrap_or_default(),
            local_prefix: self
                .local_prefix
                .as_ref()
                .and_then(glib::WeakRef::upgrade)
                .map(|row| row.text().trim().to_string())
                .unwrap_or_default(),
        }
    }

    fn operation(&self) -> LocalAccessOperation {
        self.operation.borrow().clone()
    }

    fn begin_editing(&self) {
        self.operation.replace(LocalAccessOperation::Editing);
    }

    fn connect_folder_button(
        self: &Rc<Self>,
        window: &gtk::ApplicationWindow,
        button: &gtk::Button,
        row: &adw::ActionRow,
        source_path: Option<String>,
        path_tooltip: bool,
        update: Rc<dyn Fn()>,
    ) {
        let editor = Rc::clone(self);
        let row_for_tooltip = row.downgrade();
        connect_folder_button(window, button, row, Rc::clone(&self.folder), move |path| {
            if path_tooltip && let Some(row) = row_for_tooltip.upgrade() {
                row.set_tooltip_text(Some(&path.display().to_string()));
            }
            if let (Some(source_path), Some(server_prefix)) =
                (source_path.as_deref(), editor.server_prefix.upgrade())
                && server_prefix.text().trim().is_empty()
                && let Some(suggested) = infer_server_prefix_for_root(source_path, &path)
            {
                server_prefix.set_text(&suggested);
            }
            editor.begin_editing();
            update();
        });
    }

    fn connect_changes(self: &Rc<Self>, update: Rc<dyn Fn()>) {
        if let Some(server_prefix) = self.server_prefix.upgrade() {
            let editor = Rc::clone(self);
            let update = Rc::clone(&update);
            server_prefix.connect_text_notify(move |_| {
                editor.begin_editing();
                update();
            });
        }
        if let Some(local_prefix) = self.local_prefix.as_ref().and_then(glib::WeakRef::upgrade) {
            let editor = Rc::clone(self);
            local_prefix.connect_text_notify(move |_| {
                editor.begin_editing();
                update();
            });
        }
    }

    fn save(self: &Rc<Self>, update: Rc<dyn Fn()>) {
        let Some(input) = source_local_access(
            self.source_id.clone(),
            &self.draft(),
            self.sample_source_path.clone(),
        ) else {
            return;
        };
        let receiver = self.source.save_local_access(input);
        self.operation.replace(LocalAccessOperation::Pending);
        update();
        let editor = Rc::downgrade(self);
        let update = Rc::downgrade(&update);
        glib::spawn_future_local(async move {
            let response = receiver.recv().await;
            let (Some(editor), Some(update)) = (editor.upgrade(), update.upgrade()) else {
                return;
            };
            match response {
                Ok(Ok(())) => (editor.on_success)(),
                Ok(Err(error)) => {
                    editor
                        .operation
                        .replace(LocalAccessOperation::Failed(error));
                    update();
                }
                Err(_) => {
                    editor.operation.replace(LocalAccessOperation::Failed(tr(
                        "Local file mapping is no longer available",
                    )));
                    update();
                }
            }
        });
    }
}

pub(crate) fn manage_server_navigation_page(
    shell: &Rc<Shell>,
    server: SourceSummary,
    navigation: &adw::NavigationView,
    preferences_dialog: &adw::Dialog,
    on_close: Rc<dyn Fn()>,
) -> adw::NavigationPage {
    let title = server_display_name(&server);
    let content = manage_server_content(
        shell,
        server,
        ManageServerExitSlot {
            navigation: navigation.downgrade(),
            on_close,
        },
        preferences_dialog,
    );
    adw::NavigationPage::new(&content, &title)
}

fn manage_server_content(
    shell: &Rc<Shell>,
    server: SourceSummary,
    exit: ManageServerExitSlot,
    preferences_dialog: &adw::Dialog,
) -> gtk::Widget {
    let (access, access_status, selected) = {
        let configured = shell.source.configured.borrow();
        let summary = configured
            .local_access
            .iter()
            .find(|summary| summary.source_id == server.id)
            .cloned();
        let access = summary.as_ref().and_then(|summary| summary.access.clone());
        let status = summary
            .as_ref()
            .map(|summary| summary.status.clone())
            .unwrap_or_default();
        let selected = configured.selected_source_id.as_ref() == Some(&server.id);
        (access, status, selected)
    };
    let current_sample = {
        let player = shell.selected_playback();
        let source_id = shell.selected_library().as_deref().and_then(|selected| {
            player
                .as_ref()
                .is_some_and(|player| player.transport.source_id == selected.source_key)
                .then(|| selected.artwork.source_id.clone())
        });
        let source_path = current_playback_track(player.as_deref())
            .and_then(|track| track.media_uri)
            .and_then(|uri| gio::File::for_uri(&uri).path())
            .map(|path| path.to_string_lossy().into_owned());
        source_id.zip(source_path)
    };
    let sample_source_path = preferred_server_sample(
        &server.id,
        current_sample
            .as_ref()
            .map(|(source_id, source_path)| (source_id, source_path.as_str())),
        access_status.sample_source_path.as_deref(),
    );
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(large_popup_content_width(MANAGE_SERVER_CLAMP_WIDTH));
    clamp.set_tightening_threshold(360);
    clamp.set_margin_top(8);
    clamp.set_margin_bottom(20);
    clamp.set_margin_start(24);
    clamp.set_margin_end(24);
    clamp.set_valign(gtk::Align::Start);
    scroller.set_child(Some(&clamp));

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.add_css_class("manage-server-content");
    content.set_hexpand(true);
    clamp.set_child(Some(&content));
    match shell.products.source.configured_source(&server.id) {
        Ok(Some(saved)) => {
            if let Some(settings_group) = source_settings_group(shell, &saved) {
                let settings = match settings_group {
                    Ok(settings) => settings,
                    Err(error) => source_settings_error(&error),
                };
                content.append(&settings);
            }
        }
        Ok(None) => {}
        Err(error) => content.append(&source_settings_error(&error)),
    }

    if let Some(half_stars) = super::half_stars_row(shell, &server) {
        let library = adw::PreferencesGroup::builder()
            .title(tr("Library"))
            .build();
        library.add(&half_stars);
        content.append(&library);
    }

    let saved_folder = access.as_ref().map(|access| access.root_path.clone());
    let saved_local_prefix = access
        .as_ref()
        .and_then(|access| access.local_prefix.as_deref())
        .unwrap_or_default()
        .to_string();
    let saved_server_prefix = access
        .as_ref()
        .and_then(|access| access.server_prefix.as_deref())
        .unwrap_or_default()
        .to_string();
    let mut display_local_prefix = saved_local_prefix.clone();
    let mut display_server_prefix = saved_server_prefix.clone();
    if display_server_prefix.trim().is_empty()
        && let (Some(source_path), Some(local_path)) = (
            access_status.sample_source_path.as_deref(),
            access_status.sample_local_path.as_deref(),
        )
        && let Some((suggested_server_prefix, suggested_local_prefix)) =
            infer_path_prefixes(source_path, local_path)
    {
        display_server_prefix = suggested_server_prefix;
        display_local_prefix = suggested_local_prefix;
    }
    let initial_draft = LocalAccessDraft {
        folder: saved_folder.clone(),
        server_prefix: saved_server_prefix.trim().to_string(),
        local_prefix: saved_local_prefix.trim().to_string(),
    };

    let folder_row = adw::ActionRow::builder()
        .title(tr("Local music folder"))
        .use_markup(false)
        .subtitle(
            access
                .as_ref()
                .map(|access| access.root_path.display().to_string())
                .unwrap_or_else(|| tr("No folder selected")),
        )
        .build();
    let folder_button = gtk::Button::with_label(&tr("Choose"));
    folder_button.set_valign(gtk::Align::Center);
    folder_row.add_suffix(&folder_button);
    folder_row.set_activatable_widget(Some(&folder_button));

    let server_prefix = adw::EntryRow::builder()
        .title(tr("Server Prefix"))
        .text(&display_server_prefix)
        .build();

    let local_prefix = adw::EntryRow::builder()
        .title(tr("Local Prefix"))
        .text(&display_local_prefix)
        .build();

    let sample_subtitle = sample_source_path
        .clone()
        .unwrap_or_else(|| tr("No cached server path yet"));
    let sample_row = adw::ActionRow::builder()
        .title(tr("Server Sample"))
        .use_markup(false)
        .subtitle(sample_subtitle)
        .build();

    let preview_row = adw::ActionRow::builder()
        .title(tr("Mapped Local Path"))
        .use_markup(false)
        .subtitle(preview_local_path_text(
            sample_source_path.as_deref(),
            server_prefix.text().as_str(),
            local_prefix.text().as_str(),
            saved_folder.as_deref(),
        ))
        .build();
    let group = adw::PreferencesGroup::builder()
        .title(tr("Local File Access"))
        .build();
    let subtitle = if access.is_some() {
        tr("Local file access configured")
    } else {
        tr("Use local files for playback, lyrics, and supported metadata editing")
    };
    let mapping_expander = adw::ExpanderRow::builder()
        .title(tr("Local File Mapping"))
        .subtitle(subtitle)
        .build();
    mapping_expander.add_row(&folder_row);
    mapping_expander.add_row(&server_prefix);
    mapping_expander.add_row(&local_prefix);
    mapping_expander.add_row(&sample_row);
    mapping_expander.add_row(&preview_row);
    group.add(&mapping_expander);
    content.append(&group);

    let status = gtk::Label::new(None);
    status.add_css_class("muted");
    status.add_css_class("manage-server-status");
    status.set_wrap(true);
    status.set_xalign(0.0);
    content.append(&status);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let remove = text_button("rufin-edit-clear-symbolic", "Clear Mapping");
    remove.set_visible(access.is_some());
    let save = text_button("rufin-document-save-symbolic", "Save Mapping");
    save.add_css_class("suggested-action");
    actions.append(&remove);
    actions.append(&save);
    content.append(&actions);
    status.set_visible(mapping_expander.is_expanded());
    actions.set_visible(mapping_expander.is_expanded());
    mapping_expander.connect_expanded_notify({
        let status = status.clone();
        let actions = actions.clone();
        move |expander| {
            let expanded = expander.is_expanded();
            status.set_visible(expanded);
            actions.set_visible(expanded);
        }
    });

    content.append(&server_actions_group(
        shell,
        &server,
        selected,
        &exit,
        preferences_dialog,
    ));
    let exit_for_save = exit.clone();
    let editor = LocalAccessEditor::new(
        shell,
        server.id.clone(),
        saved_folder,
        &server_prefix,
        Some(&local_prefix),
        sample_source_path.clone(),
        Rc::new(move || close_manage_server(&exit_for_save)),
    );
    let update_state: Rc<dyn Fn()> = Rc::new({
        let editor = Rc::clone(&editor);
        let sample_source_path = sample_source_path.clone();
        let preview_row = preview_row.downgrade();
        let status = status.downgrade();
        let save = save.downgrade();
        let remove = remove.downgrade();
        let folder_button = folder_button.downgrade();
        let server_prefix = server_prefix.downgrade();
        let local_prefix = local_prefix.downgrade();
        let initial_draft = initial_draft.clone();
        let access_status = access_status.clone();
        move || {
            let (
                Some(preview_row),
                Some(status),
                Some(save),
                Some(remove),
                Some(folder_button),
                Some(server_prefix),
                Some(local_prefix),
            ) = (
                preview_row.upgrade(),
                status.upgrade(),
                save.upgrade(),
                remove.upgrade(),
                folder_button.upgrade(),
                server_prefix.upgrade(),
                local_prefix.upgrade(),
            )
            else {
                return;
            };
            let draft = editor.draft();
            let has_location = draft.folder.is_some();
            let local_prefix_exists = draft.local_prefix.trim().is_empty()
                || Path::new(draft.local_prefix.trim()).is_dir();
            let changed = draft != initial_draft;
            let preview = validate_local_access_path(
                sample_source_path.as_deref(),
                draft.server_prefix.as_str(),
                draft.local_prefix.as_str(),
                draft.folder.as_deref(),
            );
            let operation = editor.operation();
            let pending = matches!(operation, LocalAccessOperation::Pending);
            folder_button.set_sensitive(!pending);
            server_prefix.set_sensitive(!pending);
            local_prefix.set_sensitive(!pending);
            remove.set_sensitive(!pending);
            save.set_sensitive(
                has_location && local_prefix_exists && changed && preview.saveable && !pending,
            );
            preview_row.set_subtitle(&preview.message);
            status.set_text(&match operation {
                LocalAccessOperation::Failed(error) => error,
                LocalAccessOperation::Editing | LocalAccessOperation::Pending => {
                    local_access_status_text(&draft, true, changed, &access_status)
                }
            });
        }
    });
    editor.connect_folder_button(
        &shell.chrome.window,
        &folder_button,
        &folder_row,
        sample_source_path,
        false,
        Rc::clone(&update_state),
    );
    editor.connect_changes(Rc::clone(&update_state));

    let source = shell.products.source.clone();
    let source_id = server.id.clone();
    let exit_for_remove = exit.clone();
    remove.connect_clicked(move |_| {
        source.clear_local_access(source_id.clone());
        close_manage_server(&exit_for_remove);
    });

    save.connect_clicked({
        let editor = Rc::clone(&editor);
        let update_state = Rc::clone(&update_state);
        move |_| editor.save(Rc::clone(&update_state))
    });

    update_state();
    scroller.upcast()
}

pub(crate) fn metadata_local_access_recovery_form(
    shell: &Rc<Shell>,
    source_path: &str,
    selected: &crate::runtime::SelectedLibrary,
    on_success: Rc<dyn Fn()>,
) -> gtk::Widget {
    let summary = shell
        .source
        .configured
        .borrow()
        .local_access
        .iter()
        .find(|summary| summary.source_id == selected.artwork.source_id)
        .cloned();
    let access = summary.as_ref().and_then(|summary| summary.access.clone());
    let suggested = summary
        .as_ref()
        .and_then(|summary| {
            Some((
                summary.status.sample_source_path.as_deref()?,
                summary.status.sample_local_path.as_deref()?,
            ))
        })
        .and_then(|(server, local)| infer_path_prefixes(server, local));
    let server_prefix_text = access
        .as_ref()
        .and_then(|access| access.server_prefix.clone())
        .filter(|value| !value.trim().is_empty())
        .or_else(|| suggested.as_ref().map(|(server, _)| server.clone()))
        .unwrap_or_default();
    let folder = access.as_ref().map(|access| {
        access
            .local_prefix
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| access.root_path.clone())
    });
    let fields = gtk::Box::new(gtk::Orientation::Vertical, METADATA_RECOVERY_ROW_SPACING);
    fields.set_margin_top(8);
    fields.set_margin_bottom(18);
    fields.set_margin_start(24);
    fields.set_margin_end(24);

    let title = gtk::Label::new(Some(&tr("File path replacement")));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("heading");
    fields.append(&title);

    let reported_path = adw::ActionRow::builder()
        .title(tr("Server path"))
        .use_markup(false)
        .subtitle(source_path)
        .build();
    reported_path.set_tooltip_text(Some(source_path));

    let server_prefix = adw::EntryRow::builder()
        .title(tr("Remove prefix"))
        .text(&server_prefix_text)
        .build();
    let local_folder = adw::ActionRow::builder()
        .title(tr("Local music folder"))
        .use_markup(false)
        .subtitle(
            folder
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| tr("No folder selected")),
        )
        .build();
    if let Some(path) = folder.as_deref() {
        local_folder.set_tooltip_text(Some(&path.display().to_string()));
    }
    let choose = gtk::Button::with_label(&tr("Choose"));
    choose.set_valign(gtk::Align::Center);
    local_folder.add_suffix(&choose);
    local_folder.set_activatable_widget(Some(&choose));
    let locations = gtk::Box::new(
        gtk::Orientation::Horizontal,
        METADATA_RECOVERY_COLUMN_SPACING,
    );
    locations.set_homogeneous(true);
    locations.append(&compact_field_row_group(&reported_path));
    locations.append(&compact_field_row_group(&local_folder));
    fields.append(&install_compact_field_row_responsiveness_at(
        &locations,
        METADATA_RECOVERY_FIELD_STACK_WIDTH,
    ));
    fields.append(&compact_field_row_group(&server_prefix));

    let status = gtk::Label::new(None);
    status.set_halign(gtk::Align::Start);
    status.set_wrap(true);
    status.add_css_class("error");
    fields.append(&status);

    let continue_button = gtk::Button::with_label(&tr("Continue"));
    continue_button.add_css_class("destructive-action");
    continue_button.set_halign(gtk::Align::End);
    fields.append(&continue_button);

    let editor = LocalAccessEditor::new(
        shell,
        selected.artwork.source_id.clone(),
        folder,
        &server_prefix,
        None,
        Some(source_path.to_string()),
        on_success,
    );
    let update: Rc<dyn Fn()> = Rc::new({
        let editor = Rc::clone(&editor);
        let server_prefix = server_prefix.downgrade();
        let choose = choose.downgrade();
        let status = status.downgrade();
        let continue_button = continue_button.downgrade();
        let source_path = source_path.to_string();
        move || {
            let (Some(server_prefix), Some(choose), Some(status), Some(continue_button)) = (
                server_prefix.upgrade(),
                choose.upgrade(),
                status.upgrade(),
                continue_button.upgrade(),
            ) else {
                return;
            };
            let draft = editor.draft();
            let view = local_access_recovery_view(
                local_access_replacement_state(
                    source_path.as_str(),
                    draft.server_prefix.as_str(),
                    draft.folder.as_deref(),
                ),
                &editor.operation(),
            );
            server_prefix.set_sensitive(view.controls_sensitive);
            choose.set_sensitive(view.controls_sensitive);
            continue_button.set_sensitive(view.continue_sensitive);
            continue_button.set_label(&if view.checking {
                tr("Checking...")
            } else {
                tr("Continue")
            });
            status.set_text(&view.message);
            status.set_visible(!view.message.is_empty());
        }
    });
    editor.connect_folder_button(
        &shell.chrome.window,
        &choose,
        &local_folder,
        Some(source_path.to_string()),
        true,
        Rc::clone(&update),
    );
    editor.connect_changes(Rc::clone(&update));
    continue_button.connect_clicked({
        let editor = Rc::clone(&editor);
        let update = Rc::clone(&update);
        move |_| editor.save(Rc::clone(&update))
    });

    update();
    fields.upcast()
}

fn source_settings_error(error: &str) -> gtk::Widget {
    let label = gtk::Label::new(Some(error));
    label.set_wrap(true);
    label.upcast()
}

fn close_manage_server(exit: &ManageServerExitSlot) {
    if let Some(navigation) = exit.navigation.upgrade() {
        navigation.pop();
    }
    (exit.on_close)();
}

pub(crate) fn credential_source_settings_group(
    shell: &Rc<Shell>,
    preset: CredentialPreset,
    source_title: &'static str,
    authentication: Option<OpenSubsonicAuthentication>,
    extra: Option<adw::SwitchRow>,
    submit: impl Fn(&SourceHandle, CredentialInput, Option<OpenSubsonicAuthentication>) + 'static,
) -> gtk::Widget {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 8);

    let fields_group = adw::PreferencesGroup::builder()
        .title(tr("Server Settings"))
        .description(tr(source_title))
        .build();

    let (name_address_row, name, address) =
        server_name_address_row(&preset.source_name, &preset.server_url, true);
    fields_group.add(&name_address_row);
    section.append(&fields_group);

    let rows_group = adw::PreferencesGroup::new();

    let username = adw::EntryRow::builder()
        .title(tr("Username"))
        .text(&preset.username)
        .build();
    style_compact_field_row(&username);

    let password = adw::PasswordEntryRow::builder()
        .title(tr("Password"))
        .build();
    style_compact_field_row(&password);
    let authentication = authentication.map(|authentication| Rc::new(Cell::new(authentication)));
    if let Some(authentication) = authentication.as_ref() {
        let api_key =
            open_subsonic_authentication_switch(Rc::clone(authentication), &username, &password);
        rows_group.add(&api_key);
    }
    rows_group.add(&username);
    rows_group.add(&password);

    let cert_verify = adw::SwitchRow::builder()
        .title(tr("Verify server certificate"))
        .subtitle(tr("Off only for a server you control"))
        .active(!preset.trust_invalid_cert)
        .build();
    rows_group.add(&cert_verify);
    if let Some(extra) = extra.as_ref() {
        rows_group.add(extra);
    }

    let save = button_row("Save Server Settings", "rufin-document-save-symbolic");
    save.add_css_class("suggested-action");
    rows_group.add(&save);
    section.append(&rows_group);

    let source = shell.products.source.clone();
    save.connect_activated(move |_| {
        let authentication = authentication
            .as_ref()
            .map(|authentication| authentication.get());
        submit(
            &source,
            CredentialInput {
                source_name: Some(name.text().trim().to_string()),
                server_url: address.text().trim().to_string(),
                username: username.text().trim().to_string(),
                secret: password.text().to_string(),
                trust_invalid_cert: !cert_verify.is_active(),
            },
            authentication,
        );
    });

    section.upcast()
}

fn server_name_address_row(
    name_text: &str,
    address_text: &str,
    show_address: bool,
) -> (gtk::Widget, adw::EntryRow, adw::EntryRow) {
    let fields = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    fields.set_homogeneous(true);
    fields.set_halign(gtk::Align::Fill);
    fields.set_hexpand(true);
    fields.set_margin_top(0);
    fields.set_margin_bottom(0);

    let name = adw::EntryRow::builder()
        .title(tr("Name"))
        .text(name_text)
        .build();
    style_compact_field_row(&name);
    let name_group = compact_field_row_group(&name);
    fields.append(&name_group);

    let address = adw::EntryRow::builder()
        .title(tr("Server Address"))
        .text(address_text)
        .build();
    style_compact_field_row(&address);
    let address_group = compact_field_row_group(&address);
    address_group.set_visible(show_address);
    fields.append(&address_group);

    let fields = install_compact_field_row_responsiveness(&fields).upcast();

    (fields, name, address)
}

fn server_actions_group(
    shell: &Rc<Shell>,
    server: &SourceSummary,
    selected: bool,
    exit: &ManageServerExitSlot,
    preferences_dialog: &adw::Dialog,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(tr("Server Actions"))
        .build();
    let row = adw::PreferencesRow::new();
    let actions = action_button_box();

    if !selected {
        let select = row_action_button("Use This Source", "rufin-object-select-symbolic");
        let source = shell.products.source.clone();
        let source_id = server.id.clone();
        let exit = exit.clone();
        let preferences_dialog = preferences_dialog.downgrade();
        select.connect_clicked(move |_| {
            source.select_source(source_id.clone());
            close_manage_server(&exit);
            if let Some(dialog) = preferences_dialog.upgrade() {
                dialog.close();
            }
        });
        actions.append(&select);
    }

    let resync = row_action_button("Resync Library", "rufin-view-refresh-symbolic");
    let source = shell.products.source.clone();
    let source_id = server.id.clone();
    let preferences_dialog_for_resync = preferences_dialog.downgrade();
    resync.connect_clicked(move |_| {
        source.refresh_source(source_id.clone());
        if let Some(dialog) = preferences_dialog_for_resync.upgrade() {
            dialog.close();
        }
    });
    actions.append(&resync);

    let forget = row_action_button("Forget Server", "rufin-window-close-symbolic");
    forget.add_css_class("destructive-action");
    let forget_shell = Rc::clone(shell);
    let source_id = server.id.clone();
    let server_name = server_display_name(server);
    let exit = exit.clone();
    let preferences_dialog = preferences_dialog.downgrade();
    forget.connect_clicked(move |_| {
        confirm_forget_source(
            &forget_shell,
            source_id.clone(),
            &server_name,
            Rc::new({
                let exit = exit.clone();
                let preferences_dialog = preferences_dialog.clone();
                move || {
                    close_manage_server(&exit);
                    if let Some(dialog) = preferences_dialog.upgrade() {
                        dialog.close();
                    }
                }
            }),
        );
    });
    actions.append(&forget);
    row.set_child(Some(&actions));
    row.set_activatable(false);
    row.set_selectable(false);
    group.add(&row);

    group
}

fn action_button_box() -> gtk::Box {
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    actions.set_homogeneous(true);
    actions.set_halign(gtk::Align::Fill);
    actions.set_hexpand(true);
    actions.set_margin_top(6);
    actions.set_margin_bottom(6);
    actions.set_margin_start(8);
    actions.set_margin_end(8);
    actions
}

fn row_action_button(title: &str, icon_name: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.set_halign(gtk::Align::Fill);
    button.set_hexpand(true);
    button.set_tooltip_text(Some(&tr(title)));
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.set_halign(gtk::Align::Center);
    content.set_valign(gtk::Align::Center);
    content.append(&gtk::Image::from_icon_name(icon_name));
    let label = gtk::Label::new(Some(&tr(title)));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_width_chars(0);
    label.set_max_width_chars(18);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_lines(2);
    content.append(&label);
    button.set_child(Some(&content));
    button
}

fn button_row(title: &str, icon_name: &str) -> adw::ButtonRow {
    let row = adw::ButtonRow::builder()
        .title(tr(title))
        .start_icon_name(icon_name)
        .end_icon_name("rufin-go-next-symbolic")
        .build();
    row.add_css_class("manage-server-action-row");
    row
}

pub(crate) fn confirm_forget_source(
    shell: &Rc<Shell>,
    source_id: SourceId,
    server_name: &str,
    after_forget: Rc<dyn Fn()>,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(tr("Forget Server"))
        .body(format!(
            "{} {}",
            tr("This removes the server, cached library metadata, queue snapshot, and saved token for"),
            server_name
        ))
        .build();
    let cancel = tr("Cancel");
    let forget = tr("Forget Server");
    dialog.add_responses(&[("cancel", cancel.as_str()), ("forget", forget.as_str())]);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("forget", adw::ResponseAppearance::Destructive);
    let source = shell.products.source.clone();
    dialog.choose(
        Some(&shell.chrome.window),
        None::<&gio::Cancellable>,
        move |response| {
            if response.as_str() == "forget" {
                source.forget_source(source_id.clone());
                after_forget();
            }
        },
    );
}

fn server_display_name(server: &SourceSummary) -> String {
    if server.name.trim().is_empty() {
        source_kind_title(&server.kind)
            .map(tr)
            .unwrap_or_else(|| server.kind.clone())
    } else {
        server.name.clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LocalAccessDraft {
    folder: Option<PathBuf>,
    server_prefix: String,
    local_prefix: String,
}

struct LocalAccessMapping {
    root_path: PathBuf,
    server_prefix: Option<String>,
    local_prefix: Option<String>,
}

fn reported_path_is_absolute(value: &str) -> bool {
    Path::new(value).is_absolute()
        || value.as_bytes().get(1) == Some(&b':')
        || value.starts_with("\\\\")
}

fn project_local_access_path(value: &str, mapping: &LocalAccessMapping) -> Option<PathBuf> {
    if let Some(server_prefix) = mapping.server_prefix.as_deref() {
        let suffix = value.strip_prefix(server_prefix)?;
        let suffix = suffix.trim_start_matches(['/', '\\']);
        let base = mapping
            .local_prefix
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| mapping.root_path.clone());
        return Some(base.join(suffix.replace('\\', "/")));
    }
    if reported_path_is_absolute(value) {
        Some(PathBuf::from(value))
    } else {
        Some(mapping.root_path.join(value.replace('\\', "/")))
    }
}

fn source_local_access(
    source_id: SourceId,
    draft: &LocalAccessDraft,
    sample_source_path: Option<String>,
) -> Option<SourceLocalAccess> {
    Some(SourceLocalAccess {
        source_id,
        root_path: draft.folder.clone()?,
        server_prefix: normalized_prefix(&draft.server_prefix),
        local_prefix: normalized_prefix(&draft.local_prefix),
        sample_source_path,
    })
}

fn normalized_prefix(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn local_access_recovery_view(
    validation: (bool, String),
    operation: &LocalAccessOperation,
) -> LocalAccessRecoveryView {
    let (mapping_ready, validation_message) = validation;
    let pending = matches!(operation, LocalAccessOperation::Pending);
    let message = match operation {
        LocalAccessOperation::Failed(error) => error.clone(),
        LocalAccessOperation::Editing | LocalAccessOperation::Pending => validation_message,
    };
    LocalAccessRecoveryView {
        controls_sensitive: !pending,
        continue_sensitive: mapping_ready && !pending,
        checking: pending,
        message,
    }
}

fn preferred_server_sample(
    source_id: &SourceId,
    current: Option<(&SourceId, &str)>,
    cached: Option<&str>,
) -> Option<String> {
    current
        .filter(|(current_source_id, _)| *current_source_id == source_id)
        .map(|(_, source_path)| source_path.to_string())
        .or_else(|| cached.map(str::to_string))
}

fn local_access_replacement_state(
    source_path: &str,
    server_prefix: &str,
    root: Option<&Path>,
) -> (bool, String) {
    let Some(root) = root else {
        return (false, tr("Choose a local music folder"));
    };
    let validation = validate_local_access_path(Some(source_path), server_prefix, "", Some(root));
    let Some(projected) = validation.projected else {
        return (false, validation.message);
    };
    if !validation.saveable || !projected.starts_with(root) {
        return (false, validation.message);
    }
    (true, String::new())
}

fn preview_local_path_text(
    sample_source_path: Option<&str>,
    server_prefix: &str,
    local_prefix: &str,
    folder: Option<&Path>,
) -> String {
    validate_local_access_path(sample_source_path, server_prefix, local_prefix, folder).message
}

struct LocalAccessPathValidation {
    message: String,
    projected: Option<PathBuf>,
    saveable: bool,
}

fn validate_local_access_path(
    sample_source_path: Option<&str>,
    server_prefix: &str,
    local_prefix: &str,
    folder: Option<&Path>,
) -> LocalAccessPathValidation {
    let Some(sample) = sample_source_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        return LocalAccessPathValidation {
            message: tr("No cached server path yet"),
            projected: None,
            saveable: true,
        };
    };
    let server_prefix = server_prefix.trim();
    let local_prefix = local_prefix.trim();
    let base = if local_prefix.is_empty() {
        let Some(folder) = folder else {
            return LocalAccessPathValidation {
                message: tr("Choose a local music folder"),
                projected: None,
                saveable: false,
            };
        };
        folder.to_path_buf()
    } else {
        PathBuf::from(local_prefix)
    };
    let mapping = LocalAccessMapping {
        root_path: folder
            .map(Path::to_path_buf)
            .unwrap_or_else(|| base.clone()),
        server_prefix: (!server_prefix.is_empty()).then(|| server_prefix.to_string()),
        local_prefix: (!local_prefix.is_empty()).then(|| local_prefix.to_string()),
    };
    let projected = project_local_access_path(sample, &mapping);

    if !server_prefix.is_empty() {
        return match projected {
            Some(path) => {
                let message = path.to_string_lossy().into_owned();
                LocalAccessPathValidation {
                    message,
                    projected: Some(path),
                    saveable: true,
                }
            }
            _ => LocalAccessPathValidation {
                message: tr("Server prefix doesn't match"),
                projected: None,
                saveable: false,
            },
        };
    }

    let sample_path = Path::new(sample);
    if !reported_path_is_absolute(sample) {
        let path = projected.unwrap_or_else(|| base.join(sample_path));
        return LocalAccessPathValidation {
            message: path.to_string_lossy().into_owned(),
            projected: Some(path),
            saveable: true,
        };
    }
    if folder.is_some_and(|folder| sample_path.starts_with(folder)) {
        return LocalAccessPathValidation {
            message: sample.to_string(),
            projected: Some(sample_path.to_path_buf()),
            saveable: true,
        };
    }
    LocalAccessPathValidation {
        message: tr("Add a matching server prefix"),
        projected,
        saveable: false,
    }
}

fn local_access_status_text(
    draft: &LocalAccessDraft,
    remote: bool,
    changed: bool,
    status: &LocalAccessStatus,
) -> String {
    if draft.folder.is_none() {
        return tr("Choose a local music folder");
    }
    if !remote {
        return if changed {
            tr("Save to rescan")
        } else {
            tr("Saved")
        };
    }
    if !draft.local_prefix.trim().is_empty() && !Path::new(draft.local_prefix.trim()).is_dir() {
        return tr("Choose an existing local folder");
    }
    if status.total_track_count == 0 {
        return if changed {
            tr("Save to rescan")
        } else {
            tr("Saved")
        };
    }

    let total = status.total_track_count.to_string();
    let direct = status.direct_match_count.to_string();
    let prefix = status.prefix_match_count.to_string();
    let metadata = status.metadata_match_count.to_string();
    let unmatched = status.unmatched_count.to_string();
    let args = [
        ("direct", direct.as_str()),
        ("prefix", prefix.as_str()),
        ("metadata", metadata.as_str()),
        ("unmatched", unmatched.as_str()),
        ("total", total.as_str()),
    ];
    if changed {
        trn_with(
            "Unsaved changes. {direct} direct, {prefix} prefix, {metadata} metadata, {unmatched} unmatched of {total} track",
            "Unsaved changes. {direct} direct, {prefix} prefix, {metadata} metadata, {unmatched} unmatched of {total} tracks",
            status.total_track_count as u64,
            &args,
        )
    } else {
        trn_with(
            "Saved mapping. {direct} direct, {prefix} prefix, {metadata} metadata, {unmatched} unmatched of {total} track",
            "Saved mapping. {direct} direct, {prefix} prefix, {metadata} metadata, {unmatched} unmatched of {total} tracks",
            status.total_track_count as u64,
            &args,
        )
    }
}

fn infer_path_prefixes(source_path: &str, local_path: &str) -> Option<(String, String)> {
    let server_parts = path_component_spans(source_path);
    let local_parts = path_component_spans(local_path);
    let suffix_len = common_suffix_len(&server_parts, &local_parts);
    if suffix_len == 0 || suffix_len > server_parts.len() || suffix_len > local_parts.len() {
        return None;
    }
    let server_prefix = prefix_before_suffix(source_path, &server_parts, suffix_len)?;
    let local_prefix = prefix_before_suffix(local_path, &local_parts, suffix_len)?;
    Some((server_prefix, local_prefix))
}

fn infer_server_prefix_for_root(source_path: &str, root: &Path) -> Option<String> {
    let parts = path_component_spans(source_path);
    for suffix_start in 0..parts.len() {
        let candidate = parts[suffix_start..]
            .iter()
            .fold(root.to_path_buf(), |path, part| path.join(part.value));
        if candidate.is_file() {
            let suffix_len = parts.len().checked_sub(suffix_start)?;
            return prefix_before_suffix(source_path, &parts, suffix_len);
        }
    }
    None
}

fn common_suffix_len(server_parts: &[PathComponent], local_parts: &[PathComponent]) -> usize {
    server_parts
        .iter()
        .rev()
        .zip(local_parts.iter().rev())
        .take_while(|(server, local)| server.value.eq_ignore_ascii_case(local.value))
        .count()
}

fn prefix_before_suffix(value: &str, parts: &[PathComponent], suffix_len: usize) -> Option<String> {
    let suffix_start_index = parts.len().checked_sub(suffix_len)?;
    let prefix_end = parts.get(suffix_start_index)?.start;
    let raw_prefix = value.get(..prefix_end)?;
    let trimmed = raw_prefix.trim_end_matches(['/', '\\']);
    if !trimmed.is_empty() {
        return Some(trimmed.to_string());
    }
    raw_prefix
        .chars()
        .find(|character| *character == '/' || *character == '\\')
        .map(|character| character.to_string())
}

#[derive(Clone, Debug)]
struct PathComponent<'a> {
    value: &'a str,
    start: usize,
}

fn path_component_spans(value: &str) -> Vec<PathComponent<'_>> {
    let mut parts = Vec::new();
    let mut start = None;
    for (index, character) in value.char_indices() {
        if character == '/' || character == '\\' {
            if let Some(part_start) = start.take()
                && part_start < index
            {
                parts.push(PathComponent {
                    value: value.get(part_start..index).unwrap_or_default(),
                    start: part_start,
                });
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(part_start) = start
        && part_start < value.len()
    {
        parts.push(PathComponent {
            value: value.get(part_start..).unwrap_or_default(),
            start: part_start,
        });
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_mapping_defers_file_validation_to_the_exact_check() {
        let directory = tempfile::tempdir().expect("temporary local mapping");
        let root = directory.path().join("Music");
        std::fs::create_dir_all(&root).expect("create mapped music folder");

        assert!(
            local_access_replacement_state(
                "/server/music/Artist/Track.flac",
                "/server/music",
                Some(&root),
            )
            .0
        );
        assert!(
            local_access_replacement_state(
                "/server/music/Artist/Missing.flac",
                "/server/music",
                Some(&root),
            )
            .0
        );
        assert!(
            !local_access_replacement_state(
                "/server/music/Artist/Track.flac",
                "/different/root",
                Some(&root),
            )
            .0
        );
    }

    #[test]
    fn direct_same_path_mapping_does_not_require_prefixes() {
        let directory = tempfile::tempdir().expect("temporary local mapping");
        let root = directory.path().join("Music");
        std::fs::create_dir_all(&root).expect("create local music folder");
        let track = root.join("Track.flac");

        let state =
            local_access_replacement_state(track.to_string_lossy().as_ref(), "", Some(&root));

        assert_eq!(state, (true, String::new()));
    }

    #[test]
    fn relative_server_paths_use_the_selected_music_folder() {
        let directory = tempfile::tempdir().expect("temporary local mapping");
        let state = local_access_replacement_state(
            "Artist/Album/01-01 - Track.flac",
            "",
            Some(directory.path()),
        );

        assert_eq!(state, (true, String::new()));
    }

    #[test]
    fn rooted_server_paths_are_not_appended_to_an_unrelated_folder() {
        let directory = tempfile::tempdir().expect("temporary local mapping");
        let validation = validate_local_access_path(
            Some(r"D:\Music\Artist\Track.flac"),
            "",
            "",
            Some(directory.path()),
        );

        assert!(!validation.saveable);
        assert_eq!(validation.message, "Add a matching server prefix");
    }

    #[test]
    fn current_track_from_the_managed_source_is_the_server_sample() {
        let managed = SourceId::new("navidrome:server:managed");
        let other = SourceId::new("jellyfin:server:other");

        assert_eq!(
            preferred_server_sample(
                &managed,
                Some((&managed, "/music/Current.flac")),
                Some("/music/Cached.flac"),
            )
            .as_deref(),
            Some("/music/Current.flac")
        );
        assert_eq!(
            preferred_server_sample(
                &managed,
                Some((&other, "/other/Current.flac")),
                Some("/music/Cached.flac"),
            )
            .as_deref(),
            Some("/music/Cached.flac")
        );
        assert_eq!(
            preferred_server_sample(&managed, Some((&other, "/other/Current.flac")), None),
            None
        );
    }

    #[test]
    fn chosen_music_folder_is_not_duplicated_as_a_local_prefix() {
        let root = PathBuf::from("/local/music");
        let input = source_local_access(
            SourceId::new("navidrome:server:test"),
            &LocalAccessDraft {
                folder: Some(root.clone()),
                server_prefix: " /music ".to_string(),
                local_prefix: String::new(),
            },
            None,
        )
        .expect("complete local access mapping");

        assert_eq!(input.root_path, root);
        assert_eq!(input.server_prefix.as_deref(), Some("/music"));
        assert_eq!(input.local_prefix, None);
    }

    #[test]
    fn completion_error_stays_visible_for_a_valid_mapping() {
        let view = local_access_recovery_view(
            (true, String::new()),
            &LocalAccessOperation::Failed("Check failed".to_string()),
        );

        assert!(view.controls_sensitive);
        assert!(view.continue_sensitive);
        assert!(!view.checking);
        assert_eq!(view.message, "Check failed");
    }

    #[test]
    fn pending_state_disables_every_mapping_control() {
        let view =
            local_access_recovery_view((true, String::new()), &LocalAccessOperation::Pending);

        assert!(!view.controls_sensitive);
        assert!(!view.continue_sensitive);
        assert!(view.checking);
        assert!(view.message.is_empty());
    }

    #[test]
    fn selected_root_can_suggest_the_server_prefix_without_becoming_a_local_prefix() {
        let directory = tempfile::tempdir().expect("temporary local mapping");
        let track = directory.path().join("Artist/Album/Track.flac");
        std::fs::create_dir_all(track.parent().expect("track parent"))
            .expect("create track parent");
        std::fs::write(&track, b"audio").expect("write track");

        assert_eq!(
            infer_server_prefix_for_root("/music/Artist/Album/Track.flac", directory.path()),
            Some("/music".to_string())
        );
        assert_eq!(
            infer_server_prefix_for_root("/music/Artist/Album/Missing.flac", directory.path()),
            None
        );
    }

    #[test]
    fn mapping_validation_does_not_probe_a_file_before_the_exact_check() {
        let directory = tempfile::tempdir().expect("temporary local mapping");
        let projected = directory.path().join("Artist/Missing.flac");
        let validation =
            validate_local_access_path(Some("Artist/Missing.flac"), "", "", Some(directory.path()));

        assert!(validation.saveable);
        assert_eq!(validation.projected.as_deref(), Some(projected.as_path()));
    }
}
