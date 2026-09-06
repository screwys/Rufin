use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::runtime::source::{LocalAccessStatus, SourceHandle, SourceLocalAccess, SourceSummary};
use adw::prelude::*;
use gtk::{gio, glib};

use localization::{tr, trn_with};
use sources::SourceId;

use super::login::{connect_folder_button, source_kind_title, source_settings_group};
use crate::layout::large_popup_content_width;
use crate::shell::Shell;

const MANAGE_SERVER_CLAMP_WIDTH: i32 = 560;

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
    message: String,
}

struct LocalAccessEditor {
    source: SourceHandle,
    source_id: SourceId,
    folder: Rc<RefCell<Option<PathBuf>>>,
    server_prefix: glib::WeakRef<adw::EntryRow>,
    local_prefix: Option<glib::WeakRef<adw::EntryRow>>,
    sample_source_path: Option<String>,
    wait_until_mapped: bool,
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
        wait_until_mapped: bool,
        on_success: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        Rc::new(Self {
            source: shell.products.source.clone(),
            source_id,
            folder: Rc::new(RefCell::new(folder)),
            server_prefix: server_prefix.downgrade(),
            local_prefix: local_prefix.map(|row| row.downgrade()),
            sample_source_path,
            wait_until_mapped,
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

    fn sample_source_path(&self) -> Option<String> {
        self.sample_source_path.clone()
    }

    fn begin_editing(&self) {
        self.operation.replace(LocalAccessOperation::Editing);
    }

    fn connect_folder_button(
        self: &Rc<Self>,
        window: &gtk::ApplicationWindow,
        button: &gtk::Button,
        row: &adw::ActionRow,
        path_tooltip: bool,
        update: Rc<dyn Fn()>,
    ) {
        let editor = Rc::clone(self);
        let row_for_tooltip = row.downgrade();
        connect_folder_button(window, button, row, Rc::clone(&self.folder), move |path| {
            if path_tooltip && let Some(row) = row_for_tooltip.upgrade() {
                row.set_tooltip_text(Some(&path.display().to_string()));
            }
            editor.begin_editing();
            editor.match_sample();
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
            self.sample_source_path(),
        ) else {
            return;
        };
        let receiver = self.source.save_local_access(input, self.wait_until_mapped);
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

    fn match_sample(&self) {
        let draft = self.draft();
        let (Some(root), Some(source_path)) =
            (draft.folder.as_deref(), self.sample_source_path.as_deref())
        else {
            return;
        };
        let matched = sources::match_local_access_sample(
            root,
            normalized_prefix(&draft.server_prefix).as_deref(),
            normalized_prefix(&draft.local_prefix).as_deref(),
            source_path,
        );
        let Some(server_prefix) = matched else {
            self.operation.replace(LocalAccessOperation::Failed(tr(
                "Mapped local file not found",
            )));
            return;
        };
        self.operation.replace(LocalAccessOperation::Editing);
        if let Some(row) = self.server_prefix.upgrade() {
            row.set_text(server_prefix.as_deref().unwrap_or_default());
        }
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
    let sample_source_path = access_status.sample_source_path.clone();
    let resource = crate::ui_resource::MANAGE_SERVER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        scroller: gtk::ScrolledWindow,
        clamp: adw::Clamp,
        content: gtk::Box,
        mapping_group: adw::PreferencesGroup,
        mapping_expander: adw::ExpanderRow,
        folder_row: adw::ActionRow,
        folder_button: gtk::Button,
        server_prefix: adw::EntryRow,
        local_prefix: adw::EntryRow,
        sample_row: adw::ActionRow,
        preview_row: adw::ActionRow,
        status: gtk::Label,
        actions: gtk::Box,
        remove: gtk::Button,
        save: gtk::Button,
    });
    clamp.set_maximum_size(large_popup_content_width(MANAGE_SERVER_CLAMP_WIDTH));
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
    let display_local_prefix = saved_local_prefix.clone();
    let display_server_prefix = saved_server_prefix.clone();
    let initial_draft = LocalAccessDraft {
        folder: saved_folder.clone(),
        server_prefix: saved_server_prefix.trim().to_string(),
        local_prefix: saved_local_prefix.trim().to_string(),
    };

    folder_row.set_subtitle(
        &access
            .as_ref()
            .map(|access| access.root_path.display().to_string())
            .unwrap_or_else(|| tr("No folder selected")),
    );
    folder_row.set_activatable_widget(Some(&folder_button));

    server_prefix.set_text(&display_server_prefix);

    local_prefix.set_text(&display_local_prefix);

    let sample_subtitle = sample_source_path
        .clone()
        .unwrap_or_else(|| tr("No cached server path yet"));
    sample_row.set_subtitle(&sample_subtitle);

    preview_row.set_subtitle(&preview_local_path_text(
        sample_source_path.as_deref(),
        server_prefix.text().as_str(),
        local_prefix.text().as_str(),
        saved_folder.as_deref(),
    ));
    let subtitle = if access.is_some() {
        tr("Local file access configured")
    } else {
        tr("Use local files for playback, lyrics, and supported metadata editing")
    };
    mapping_expander.set_subtitle(&subtitle);
    content.append(&mapping_group);

    content.append(&status);

    remove.set_visible(access.is_some());
    content.append(&actions);
    connect_mapping_expander_visibility(&mapping_expander, &status, &actions);

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
        false,
        Rc::new(move || close_manage_server(&exit_for_save)),
    );
    let update_state: Rc<dyn Fn()> = Rc::new({
        let editor = Rc::clone(&editor);
        let sample_row = sample_row.downgrade();
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
                Some(sample_row),
                Some(status),
                Some(save),
                Some(remove),
                Some(folder_button),
                Some(server_prefix),
                Some(local_prefix),
            ) = (
                preview_row.upgrade(),
                sample_row.upgrade(),
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
            let sample_source_path = editor.sample_source_path();
            let has_location = draft.folder.is_some();
            let local_prefix_exists = local_prefix_is_directory(&draft);
            let changed = draft != initial_draft;
            let preview = validate_local_access_path(
                sample_source_path.as_deref(),
                draft.server_prefix.as_str(),
                draft.local_prefix.as_str(),
                draft.folder.as_deref(),
            );
            sample_row.set_subtitle(
                &sample_source_path.unwrap_or_else(|| tr("No cached server path yet")),
            );
            let operation = editor.operation();
            let pending = matches!(operation, LocalAccessOperation::Pending);
            folder_button.set_sensitive(!pending);
            server_prefix.set_sensitive(!pending);
            local_prefix.set_sensitive(!pending);
            remove.set_sensitive(!pending);
            save.set_sensitive(has_location && local_prefix_exists && preview.saveable && !pending);
            preview_row.set_subtitle(&preview.message);
            status.set_text(&match operation {
                LocalAccessOperation::Failed(error) => error,
                LocalAccessOperation::Editing | LocalAccessOperation::Pending
                    if preview.projected.is_some() && !preview.saveable =>
                {
                    tr("Mapped local file not found")
                }
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

    let draft = editor.draft();
    if draft.folder.is_some()
        && !validate_local_access_path(
            editor.sample_source_path().as_deref(),
            draft.server_prefix.as_str(),
            draft.local_prefix.as_str(),
            draft.folder.as_deref(),
        )
        .saveable
    {
        editor.match_sample();
        update_state();
    } else {
        update_state();
    }
    scroller.upcast()
}

pub(crate) fn mount_metadata_local_access_mapping(
    shell: &Rc<Shell>,
    source_path: &str,
    selected: &crate::runtime::SelectedLibrary,
    content: &gtk::Box,
    on_success: Rc<dyn Fn()>,
) {
    let summary = shell
        .source
        .configured
        .borrow()
        .local_access
        .iter()
        .find(|summary| summary.source_id == selected.source_id)
        .cloned();
    let access = summary.as_ref().and_then(|summary| summary.access.clone());
    let folder = access.as_ref().map(|access| access.root_path.clone());
    let local_prefix_text = access
        .as_ref()
        .and_then(|access| access.local_prefix.clone())
        .unwrap_or_default();
    let server_prefix_text = access
        .as_ref()
        .and_then(|access| access.server_prefix.clone())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_default();
    let resource = crate::ui_resource::MANAGE_SERVER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        mapping_group: adw::PreferencesGroup,
        mapping_expander: adw::ExpanderRow,
        folder_row: adw::ActionRow,
        folder_button: gtk::Button,
        server_prefix: adw::EntryRow,
        local_prefix: adw::EntryRow,
        sample_row: adw::ActionRow,
        preview_row: adw::ActionRow,
        status: gtk::Label,
        actions: gtk::Box,
        remove: gtk::Button,
        save: gtk::Button,
    });
    content.append(&mapping_group);
    content.append(&status);
    content.append(&actions);
    mapping_expander.set_expanded(true);
    connect_mapping_expander_visibility(&mapping_expander, &status, &actions);
    remove.set_visible(false);
    server_prefix.set_text(&server_prefix_text);
    local_prefix.set_text(&local_prefix_text);
    sample_row.set_subtitle(source_path);
    sample_row.set_tooltip_text(Some(source_path));
    folder_row.set_subtitle(
        &folder
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| tr("No folder selected")),
    );
    if let Some(path) = folder.as_deref() {
        folder_row.set_tooltip_text(Some(&path.display().to_string()));
    }
    folder_row.set_activatable_widget(Some(&folder_button));
    preview_row.set_subtitle(&preview_local_path_text(
        Some(source_path),
        &server_prefix_text,
        &local_prefix_text,
        folder.as_deref(),
    ));

    let editor = LocalAccessEditor::new(
        shell,
        selected.source_id.clone(),
        folder,
        &server_prefix,
        Some(&local_prefix),
        Some(source_path.to_string()),
        true,
        on_success,
    );
    let update: Rc<dyn Fn()> = Rc::new({
        let editor = Rc::clone(&editor);
        let server_prefix = server_prefix.downgrade();
        let local_prefix = local_prefix.downgrade();
        let folder_button = folder_button.downgrade();
        let sample_row = sample_row.downgrade();
        let preview_row = preview_row.downgrade();
        let status = status.downgrade();
        let save = save.downgrade();
        move || {
            let (
                Some(server_prefix),
                Some(local_prefix),
                Some(folder_button),
                Some(sample_row),
                Some(preview_row),
                Some(status),
                Some(save),
            ) = (
                server_prefix.upgrade(),
                local_prefix.upgrade(),
                folder_button.upgrade(),
                sample_row.upgrade(),
                preview_row.upgrade(),
                status.upgrade(),
                save.upgrade(),
            )
            else {
                return;
            };
            let draft = editor.draft();
            let sample_source_path = editor.sample_source_path();
            let view = local_access_recovery_view(
                local_access_replacement_state(
                    sample_source_path.as_deref().unwrap_or_default(),
                    draft.server_prefix.as_str(),
                    draft.local_prefix.as_str(),
                    draft.folder.as_deref(),
                ),
                &editor.operation(),
            );
            server_prefix.set_sensitive(view.controls_sensitive);
            local_prefix.set_sensitive(view.controls_sensitive);
            folder_button.set_sensitive(view.controls_sensitive);
            save.set_sensitive(view.continue_sensitive);
            status.set_text(&view.message);
            status.set_visible(!view.message.is_empty());
            sample_row.set_subtitle(
                &sample_source_path.unwrap_or_else(|| tr("No cached server path yet")),
            );
            preview_row.set_subtitle(&preview_local_path_text(
                editor.sample_source_path().as_deref(),
                draft.server_prefix.as_str(),
                draft.local_prefix.as_str(),
                draft.folder.as_deref(),
            ));
        }
    });
    editor.connect_folder_button(
        &shell.chrome.window,
        &folder_button,
        &folder_row,
        true,
        Rc::clone(&update),
    );
    editor.connect_changes(Rc::clone(&update));
    save.connect_clicked({
        let editor = Rc::clone(&editor);
        let update = Rc::clone(&update);
        move |_| editor.save(Rc::clone(&update))
    });

    let draft = editor.draft();
    if draft.folder.is_some()
        && !validate_local_access_path(
            editor.sample_source_path().as_deref(),
            draft.server_prefix.as_str(),
            draft.local_prefix.as_str(),
            draft.folder.as_deref(),
        )
        .saveable
    {
        editor.match_sample();
    }
    update();
}

fn source_settings_error(error: &str) -> gtk::Widget {
    let label = gtk::Label::new(Some(error));
    label.set_wrap(true);
    label.upcast()
}

fn connect_mapping_expander_visibility(
    expander: &adw::ExpanderRow,
    status: &gtk::Label,
    actions: &gtk::Box,
) {
    status.set_visible(expander.is_expanded());
    actions.set_visible(expander.is_expanded());
    let status = status.clone();
    let actions = actions.clone();
    expander.connect_expanded_notify(move |expander| {
        let expanded = expander.is_expanded();
        status.set_visible(expanded);
        actions.set_visible(expanded);
    });
}

fn close_manage_server(exit: &ManageServerExitSlot) {
    if let Some(navigation) = exit.navigation.upgrade() {
        navigation.pop();
    }
    (exit.on_close)();
}

fn server_actions_group(
    shell: &Rc<Shell>,
    server: &SourceSummary,
    selected: bool,
    exit: &ManageServerExitSlot,
    preferences_dialog: &adw::Dialog,
) -> adw::PreferencesGroup {
    let resource = crate::ui_resource::SERVER_ACTIONS_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        group: adw::PreferencesGroup,
        select: gtk::Button,
        resync: gtk::Button,
        forget: gtk::Button,
    });

    if !selected {
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
    } else {
        select.set_visible(false);
    }

    let source = shell.products.source.clone();
    let source_id = server.id.clone();
    let preferences_dialog_for_resync = preferences_dialog.downgrade();
    resync.connect_clicked(move |_| {
        source.refresh_source(source_id.clone());
        if let Some(dialog) = preferences_dialog_for_resync.upgrade() {
            dialog.close();
        }
    });
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
    group
}

pub(crate) fn confirm_forget_source(
    shell: &Rc<Shell>,
    source_id: SourceId,
    server_name: &str,
    after_forget: Rc<dyn Fn()>,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(tr("Forget Source"))
        .body(format!(
            "{} {}",
            tr("This removes the server, cached library metadata, queue snapshot, and saved token for"),
            server_name
        ))
        .build();
    let cancel = tr("Cancel");
    let forget = tr("Forget Source");
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

fn local_prefix_is_directory(draft: &LocalAccessDraft) -> bool {
    let prefix = draft.local_prefix.trim();
    if prefix.is_empty() {
        return true;
    }
    let path = Path::new(prefix);
    if path.is_absolute() {
        path.is_dir()
    } else {
        draft
            .folder
            .as_deref()
            .is_some_and(|root| root.join(path).is_dir())
    }
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
        message,
    }
}

fn local_access_replacement_state(
    source_path: &str,
    server_prefix: &str,
    local_prefix: &str,
    root: Option<&Path>,
) -> (bool, String) {
    let Some(root) = root else {
        return (false, tr("Choose a local music folder"));
    };
    let local_prefix = local_prefix.trim();
    let local_base = if Path::new(local_prefix).is_absolute() {
        PathBuf::from(local_prefix)
    } else {
        root.join(local_prefix)
    };
    if !local_prefix.is_empty() && !local_base.is_dir() {
        return (false, tr("Choose an existing local folder"));
    }
    let validation =
        validate_local_access_path(Some(source_path), server_prefix, local_prefix, Some(root));
    if validation.projected.is_some() && !validation.saveable {
        return (false, tr("Mapped local file not found"));
    }
    if validation.projected.is_none() {
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
            saveable: false,
        };
    };
    let server_prefix = server_prefix.trim();
    let local_prefix = local_prefix.trim();
    let Some(folder) = folder else {
        return LocalAccessPathValidation {
            message: tr("Choose a local music folder"),
            projected: None,
            saveable: false,
        };
    };
    let projected = sources::project_local_access_path(
        folder,
        (!server_prefix.is_empty()).then_some(server_prefix),
        (!local_prefix.is_empty()).then_some(local_prefix),
        sample,
    );

    if !server_prefix.is_empty() {
        return match projected {
            Some(path) => {
                let message = path.to_string_lossy().into_owned();
                let saveable = mapped_file_exists(folder, &path);
                LocalAccessPathValidation {
                    message,
                    projected: Some(path),
                    saveable,
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
    if !sources::reported_path_is_absolute(sample) {
        let path = projected.unwrap_or_else(|| folder.join(sample_path));
        let saveable = mapped_file_exists(folder, &path);
        return LocalAccessPathValidation {
            message: path.to_string_lossy().into_owned(),
            projected: Some(path),
            saveable,
        };
    }
    if sample_path.starts_with(folder) {
        return LocalAccessPathValidation {
            message: sample.to_string(),
            projected: Some(sample_path.to_path_buf()),
            saveable: mapped_file_exists(folder, sample_path),
        };
    }
    LocalAccessPathValidation {
        message: tr("Add a matching server prefix"),
        projected,
        saveable: false,
    }
}

fn mapped_file_exists(root: &Path, candidate: &Path) -> bool {
    let (Ok(root), Ok(candidate)) = (root.canonicalize(), candidate.canonicalize()) else {
        return false;
    };
    candidate.starts_with(root) && candidate.is_file()
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
    if !local_prefix_is_directory(draft) {
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
    let matched = status.matched_track_count.to_string();
    let args = [("matched", matched.as_str()), ("total", total.as_str())];
    if changed {
        tr("Save to rescan")
    } else {
        trn_with(
            "Saved mapping. {matched} of {total} server path matches",
            "Saved mapping. {matched} of {total} server paths match",
            status.total_track_count as u64,
            &args,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn representative_mapping_requires_one_existing_local_file() {
        let directory = tempfile::tempdir().expect("temporary local mapping");
        let root = directory.path().join("Music");
        std::fs::create_dir_all(&root).expect("create mapped music folder");
        let track = root.join("Artist/Track.flac");
        std::fs::create_dir_all(track.parent().expect("Track parent")).expect("create Artist");
        std::fs::write(&track, b"media").expect("write Track");

        assert!(
            local_access_replacement_state(
                "/server/music/Artist/Track.flac",
                "/server/music",
                "",
                Some(&root),
            )
            .0
        );
        assert!(
            !local_access_replacement_state(
                "/server/music/Artist/Missing.flac",
                "/server/music",
                "",
                Some(&root),
            )
            .0
        );
        assert!(
            !local_access_replacement_state(
                "/server/music/Artist/Track.flac",
                "/different/root",
                "",
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
        std::fs::write(&track, b"media").expect("write Track");

        let state =
            local_access_replacement_state(track.to_string_lossy().as_ref(), "", "", Some(&root));

        assert_eq!(state, (true, String::new()));
    }

    #[test]
    fn relative_server_paths_use_the_selected_music_folder() {
        let directory = tempfile::tempdir().expect("temporary local mapping");
        let track = directory.path().join("Artist/Album/01-01 - Track.flac");
        std::fs::create_dir_all(track.parent().expect("Track parent")).expect("create Album");
        std::fs::write(&track, b"media").expect("write Track");
        let state = local_access_replacement_state(
            "Artist/Album/01-01 - Track.flac",
            "",
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
        assert_eq!(view.message, "Check failed");
    }

    #[test]
    fn pending_state_disables_every_mapping_control() {
        let view =
            local_access_recovery_view((true, String::new()), &LocalAccessOperation::Pending);

        assert!(!view.controls_sensitive);
        assert!(!view.continue_sensitive);
        assert!(view.message.is_empty());
    }

    #[test]
    fn mapping_validation_requires_the_representative_file() {
        let directory = tempfile::tempdir().expect("temporary local mapping");
        let projected = directory.path().join("Artist/Missing.flac");
        let validation =
            validate_local_access_path(Some("Artist/Missing.flac"), "", "", Some(directory.path()));

        assert!(!validation.saveable);
        assert_eq!(validation.projected.as_deref(), Some(projected.as_path()));
    }
}
