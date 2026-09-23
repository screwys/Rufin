use adw::prelude::*;
use gtk::glib;
use localization::{tr, trn_with};
use rufin_core::runtime::source::{LocalAccessStatus, SourceHandle, SourceLocalAccess};
use sources::SourceId;
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum LocalAccessOperation {
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

pub struct LocalAccessEditor {
    source: SourceHandle,
    source_id: SourceId,
    folder: RefCell<Option<PathBuf>>,
    server_prefix: glib::WeakRef<adw::EntryRow>,
    local_prefix: Option<glib::WeakRef<adw::EntryRow>>,
    sample_source_path: Option<String>,
    wait_until_mapped: bool,
    operation: RefCell<LocalAccessOperation>,
    on_success: Rc<dyn Fn()>,
}

impl LocalAccessEditor {
    pub fn new(
        source: &SourceHandle,
        source_id: SourceId,
        folder: Option<PathBuf>,
        server_prefix: &adw::EntryRow,
        local_prefix: Option<&adw::EntryRow>,
        sample_source_path: Option<String>,
        wait_until_mapped: bool,
        on_success: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        Rc::new(Self {
            source: source.clone(),
            source_id,
            folder: RefCell::new(folder),
            server_prefix: server_prefix.downgrade(),
            local_prefix: local_prefix.map(|row| row.downgrade()),
            sample_source_path,
            wait_until_mapped,
            operation: RefCell::new(LocalAccessOperation::Editing),
            on_success,
        })
    }

    pub fn draft(&self) -> LocalAccessDraft {
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

    pub fn operation(&self) -> LocalAccessOperation {
        self.operation.borrow().clone()
    }

    pub fn sample_source_path(&self) -> Option<String> {
        self.sample_source_path.clone()
    }

    pub fn begin_editing(&self) {
        self.operation.replace(LocalAccessOperation::Editing);
    }

    pub fn connect_folder_row(
        self: &Rc<Self>,
        window: &gtk::ApplicationWindow,
        button: &gtk::Button,
        row: &adw::EntryRow,
        edit: &gtk::ToggleButton,
        update: Rc<dyn Fn()>,
    ) {
        if let Some(path) = self.folder.borrow().as_deref() {
            row.set_text(&path.to_string_lossy());
        }
        edit.connect_toggled({
            let row = row.downgrade();
            move |edit| {
                if let Some(row) = row.upgrade() {
                    row.set_editable(edit.is_active());
                    if edit.is_active() {
                        row.grab_focus();
                    }
                }
            }
        });
        row.connect_text_notify({
            let editor = Rc::clone(self);
            let update = Rc::clone(&update);
            move |row| {
                let text = row.text();
                *editor.folder.borrow_mut() =
                    (!text.is_empty()).then(|| PathBuf::from(text.as_str()));
                editor.begin_editing();
                update();
            }
        });
        let window = window.downgrade();
        let row = row.downgrade();
        let edit = edit.downgrade();
        let editor = Rc::clone(self);
        button.connect_clicked(move |_| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let selected_folder = editor
                .folder
                .borrow()
                .as_ref()
                .map(gtk::gio::File::for_path);
            let row = row.clone();
            let edit = edit.clone();
            let editor = Rc::downgrade(&editor);
            let update = Rc::downgrade(&update);
            glib::spawn_future_local(async move {
                let dialog = gtk::FileDialog::builder()
                    .title(tr("Select Music Folder"))
                    .build();
                if let Some(folder) = selected_folder.as_ref() {
                    dialog.set_initial_folder(Some(folder));
                }
                let Ok(folder) = dialog.select_folder_future(Some(&window)).await else {
                    return;
                };
                let Some(path) = folder.path() else { return };
                let (Some(row), Some(edit), Some(editor), Some(update)) = (
                    row.upgrade(),
                    edit.upgrade(),
                    editor.upgrade(),
                    update.upgrade(),
                ) else {
                    return;
                };
                row.set_text(&path.to_string_lossy());
                *editor.folder.borrow_mut() = Some(path);
                edit.set_active(false);
                editor.begin_editing();
                update();
            });
        });
    }

    pub fn connect_changes(self: &Rc<Self>, update: Rc<dyn Fn()>) {
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

    pub fn save(self: &Rc<Self>, update: Rc<dyn Fn()>) {
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
}

pub fn mount_metadata_local_access_mapping(
    source: &SourceHandle,
    window: &gtk::ApplicationWindow,
    source_id: &SourceId,
    access: Option<&rufin_core::runtime::source::SourceLocalAccess>,
    source_path: &str,
    content: &gtk::Box,
    on_success: Rc<dyn Fn()>,
) {
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
    crate::objects!(builder, resource, {
        mapping_group: adw::PreferencesGroup,
        mapping_expander: adw::ExpanderRow,
        folder_row: adw::EntryRow,
        folder_edit: gtk::ToggleButton,
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
    preview_row.set_subtitle(&preview_local_path_text(
        Some(source_path),
        &server_prefix_text,
        &local_prefix_text,
        folder.as_deref(),
    ));

    let editor = LocalAccessEditor::new(
        source,
        source_id.clone(),
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
        let folder_row = folder_row.downgrade();
        let sample_row = sample_row.downgrade();
        let preview_row = preview_row.downgrade();
        let status = status.downgrade();
        let save = save.downgrade();
        move || {
            let (
                Some(server_prefix),
                Some(local_prefix),
                Some(folder_row),
                Some(sample_row),
                Some(preview_row),
                Some(status),
                Some(save),
            ) = (
                server_prefix.upgrade(),
                local_prefix.upgrade(),
                folder_row.upgrade(),
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
            let view = local_access_recovery_view(draft.folder.is_some(), &editor.operation());
            server_prefix.set_sensitive(view.controls_sensitive);
            local_prefix.set_sensitive(view.controls_sensitive);
            folder_row.set_sensitive(view.controls_sensitive);
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
    editor.connect_folder_row(
        window,
        &folder_button,
        &folder_row,
        &folder_edit,
        Rc::clone(&update),
    );
    editor.connect_changes(Rc::clone(&update));
    save.connect_clicked({
        let editor = Rc::clone(&editor);
        let update = Rc::clone(&update);
        move |_| editor.save(Rc::clone(&update))
    });

    update();
}
pub fn connect_mapping_expander_visibility(
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalAccessDraft {
    pub folder: Option<PathBuf>,
    pub server_prefix: String,
    pub local_prefix: String,
}

pub fn source_local_access(
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
    has_location: bool,
    operation: &LocalAccessOperation,
) -> LocalAccessRecoveryView {
    let pending = matches!(operation, LocalAccessOperation::Pending);
    let message = match operation {
        LocalAccessOperation::Failed(error) => error.clone(),
        _ if !has_location => tr("Choose a local music folder"),
        _ => String::new(),
    };
    LocalAccessRecoveryView {
        controls_sensitive: !pending,
        continue_sensitive: has_location && !pending,
        message,
    }
}

pub fn preview_local_path_text(
    sample_source_path: Option<&str>,
    server_prefix: &str,
    local_prefix: &str,
    folder: Option<&Path>,
) -> String {
    let Some(sample) = sample_source_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        return tr("No cached server path yet");
    };
    let server_prefix = server_prefix.trim();
    let local_prefix = local_prefix.trim();
    let Some(folder) = folder else {
        return tr("Choose a local music folder");
    };
    let projected = sources::project_local_access_path(
        folder,
        (!server_prefix.is_empty()).then_some(server_prefix),
        (!local_prefix.is_empty()).then_some(local_prefix),
        sample,
    );

    if !server_prefix.is_empty() {
        return projected
            .as_deref()
            .map(desktop_integration::display_path)
            .unwrap_or_else(|| tr("Server prefix doesn't match"));
    }

    let sample_path = Path::new(sample);
    if !sources::reported_path_is_absolute(sample) {
        let path = projected.unwrap_or_else(|| folder.join(sample_path));
        return desktop_integration::display_path(&path);
    }
    if sample_path.starts_with(folder) {
        return desktop_integration::display_path(sample_path);
    }
    tr("Add a matching server prefix")
}

pub fn local_access_status_text(
    draft: &LocalAccessDraft,
    changed: bool,
    status: &LocalAccessStatus,
) -> String {
    if draft.folder.is_none() {
        return tr("Choose a local music folder");
    }
    if changed {
        return tr("Reload to apply changes");
    }
    if status.total_track_count == 0 {
        return tr("Saved");
    }

    let total = status.total_track_count.to_string();
    let matched = status.matched_track_count.to_string();
    let args = [("matched", matched.as_str()), ("total", total.as_str())];
    trn_with(
        "Saved mapping. {matched} of {total} server path matches",
        "Saved mapping. {matched} of {total} server paths match",
        status.total_track_count as u64,
        &args,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_does_not_require_an_available_folder_or_file() {
        let directory = tempfile::tempdir().expect("temporary mapping path");
        let root = directory.path().join("unmounted-share");
        let projected = root.join("Artist/Track.flac");
        assert_eq!(
            preview_local_path_text(
                Some("/server/Artist/Track.flac"),
                "/server",
                "",
                Some(&root)
            ),
            desktop_integration::display_path(&projected),
        );
        assert_eq!(
            preview_local_path_text(Some("Artist/Track.flac"), "", "", Some(&root)),
            desktop_integration::display_path(&projected),
        );
        assert!(!root.exists());
    }

    #[test]
    fn rooted_server_paths_need_a_prefix_for_an_unrelated_folder() {
        let directory = tempfile::tempdir().expect("temporary mapping path");
        assert_eq!(
            preview_local_path_text(
                Some(r"D:\Music\Artist\Track.flac"),
                "",
                "",
                Some(directory.path())
            ),
            "Add a matching server prefix",
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
            true,
            &LocalAccessOperation::Failed("Check failed".to_string()),
        );

        assert!(view.controls_sensitive);
        assert!(view.continue_sensitive);
        assert_eq!(view.message, "Check failed");
    }

    #[test]
    fn pending_state_disables_every_mapping_control() {
        let view = local_access_recovery_view(true, &LocalAccessOperation::Pending);

        assert!(!view.controls_sensitive);
        assert!(!view.continue_sensitive);
        assert!(view.message.is_empty());
    }
}
