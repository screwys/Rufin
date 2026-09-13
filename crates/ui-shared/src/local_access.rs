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
    folder: Rc<RefCell<Option<PathBuf>>>,
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
            folder: Rc::new(RefCell::new(folder)),
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

    pub fn connect_folder_button(
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
                row.set_tooltip_text(Some(&crate::path_display::display_path(&path)));
            }
            editor.begin_editing();
            editor.match_sample();
            update();
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

    pub fn match_sample(&self) {
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
            .map(crate::path_display::display_path)
            .unwrap_or_else(|| tr("No folder selected")),
    );
    if let Some(path) = folder.as_deref() {
        folder_row.set_tooltip_text(Some(&crate::path_display::display_path(path)));
    }
    folder_row.set_activatable_widget(Some(&folder_button));
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
        window,
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

pub fn local_prefix_is_directory(draft: &LocalAccessDraft) -> bool {
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

pub fn preview_local_path_text(
    sample_source_path: Option<&str>,
    server_prefix: &str,
    local_prefix: &str,
    folder: Option<&Path>,
) -> String {
    validate_local_access_path(sample_source_path, server_prefix, local_prefix, folder).message
}

pub struct LocalAccessPathValidation {
    pub message: String,
    pub projected: Option<PathBuf>,
    pub saveable: bool,
}

pub fn validate_local_access_path(
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
                let message = crate::path_display::display_path(&path);
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
            message: crate::path_display::display_path(&path),
            projected: Some(path),
            saveable,
        };
    }
    if sample_path.starts_with(folder) {
        return LocalAccessPathValidation {
            message: crate::path_display::display_path(sample_path),
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

pub fn local_access_status_text(
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

pub fn connect_folder_button(
    window: &gtk::ApplicationWindow,
    button: &gtk::Button,
    row: &adw::ActionRow,
    target: Rc<RefCell<Option<PathBuf>>>,
    on_changed: impl Fn(PathBuf) + 'static,
) {
    let window = window.downgrade();
    let row = row.downgrade();
    let on_changed: Rc<dyn Fn(PathBuf)> = Rc::new(on_changed);
    button.connect_clicked(move |_| {
        let Some(window) = window.upgrade() else {
            return;
        };
        let target = Rc::clone(&target);
        let row = row.clone();
        let on_changed = Rc::downgrade(&on_changed);
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
            let (Some(row), Some(on_changed)) = (row.upgrade(), on_changed.upgrade()) else {
                return;
            };
            row.set_subtitle(&crate::path_display::display_path(&path));
            *target.borrow_mut() = Some(path.clone());
            on_changed(path);
        });
    });
}
