use gtk_widgets::source_labels::source_kind_title;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use rufin_core::runtime::source::SourceSummary;

use localization::tr;
use sources::SourceId;

use super::login::source_settings_group;
use crate::Preferences;
use gtk_widgets::layout::large_popup_content_width;
use gtk_widgets::local_access::{
    LocalAccessDraft, LocalAccessEditor, LocalAccessOperation, connect_mapping_expander_visibility,
    local_access_status_text, preview_local_path_text,
};

const MANAGE_SERVER_CLAMP_WIDTH: i32 = 560;

#[derive(Clone)]
struct ManageServerExitSlot {
    navigation: glib::WeakRef<adw::NavigationView>,
    on_close: Rc<dyn Fn()>,
}

pub fn manage_server_navigation_page(
    shell: &Rc<Preferences>,
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
    shell: &Rc<Preferences>,
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
    let resource = gtk_widgets::ui_resource::MANAGE_SERVER_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        scroller: gtk::ScrolledWindow,
        clamp: adw::Clamp,
        content: gtk::Box,
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

    if let Some(row) = super::playlist_auto_save_row(shell, &server) {
        content.append(&row);
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
        &shell.products.source,
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
        let folder_row = folder_row.downgrade();
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
                Some(folder_row),
                Some(server_prefix),
                Some(local_prefix),
            ) = (
                preview_row.upgrade(),
                sample_row.upgrade(),
                status.upgrade(),
                save.upgrade(),
                remove.upgrade(),
                folder_row.upgrade(),
                server_prefix.upgrade(),
                local_prefix.upgrade(),
            )
            else {
                return;
            };
            let draft = editor.draft();
            let sample_source_path = editor.sample_source_path();
            let has_location = draft.folder.is_some();
            let changed = draft != initial_draft;
            let preview = preview_local_path_text(
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
            folder_row.set_sensitive(!pending);
            server_prefix.set_sensitive(!pending);
            local_prefix.set_sensitive(!pending);
            remove.set_sensitive(!pending);
            save.set_sensitive(has_location && !pending);
            preview_row.set_subtitle(&preview);
            status.set_text(&match operation {
                LocalAccessOperation::Failed(error) => error,
                LocalAccessOperation::Editing | LocalAccessOperation::Pending => {
                    local_access_status_text(&draft, changed, &access_status)
                }
            });
        }
    });
    editor.connect_folder_row(
        &shell.window,
        &folder_button,
        &folder_row,
        &folder_edit,
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

fn server_actions_group(
    shell: &Rc<Preferences>,
    server: &SourceSummary,
    selected: bool,
    exit: &ManageServerExitSlot,
    preferences_dialog: &adw::Dialog,
) -> adw::PreferencesGroup {
    let resource = crate::ui_resource::SERVER_ACTIONS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
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

pub fn confirm_forget_source(
    shell: &Rc<Preferences>,
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
        Some(&shell.window),
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
