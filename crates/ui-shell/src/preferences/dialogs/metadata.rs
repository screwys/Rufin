use crate::shell::Shell;
use adw::prelude::*;
use localization::{msgid, tr};
use sources::SourceMetadataError;
use std::{cell::Cell, rc::Rc};
use ui_shared::layout::large_popup_content_width;
use ui_shared::metadata::{MetadataItemId, MetadataReceiver, build_dialog, metadata_error_dialog};
pub(crate) fn present_metadata_dialog(shell: &Rc<Shell>, item: MetadataItemId) {
    let receiver = match &item {
        MetadataItemId::Track(uri) => MetadataReceiver::Track(
            rufin_core::metadata::track_metadata(&shell.products.source, uri.clone()),
        ),
        MetadataItemId::Album(uri) => MetadataReceiver::Album(
            rufin_core::metadata::album_metadata(&shell.products.source, uri.clone()),
        ),
        MetadataItemId::Artist(uri) => MetadataReceiver::Artist(
            rufin_core::metadata::artist_metadata(&shell.products.source, uri.clone()),
        ),
    };
    if let Some(load) = shell.selected_ui.metadata_load.borrow_mut().take() {
        load.abort();
    }
    let weak_shell = Rc::downgrade(shell);
    let load = gtk::glib::spawn_future_local(async move {
        let draft = receiver.recv().await;
        let Some(shell) = weak_shell.upgrade() else {
            return;
        };
        match draft {
            Ok(draft) => {
                let retry_shell = Rc::downgrade(&shell);
                let dialog = build_dialog(
                    &shell.products.source,
                    item,
                    draft,
                    shell
                        .settings
                        .current
                        .borrow()
                        .allows_external_metadata_lookup(),
                    shell.chrome.window.height(),
                    Rc::new(move |item| {
                        if let Some(shell) = retry_shell.upgrade() {
                            present_metadata_dialog(&shell, item);
                        }
                    }),
                );
                shell.present_selected_dialog(&dialog);
            }
            Err(SourceMetadataError::LocalAccessRequired {
                source_id,
                source_path,
            }) => {
                let Some(selected) = shell
                    .selected_library()
                    .as_deref()
                    .filter(|selected| selected.source_id == source_id)
                    .cloned()
                else {
                    shell
                        .control_feedback
                        .show_feedback_toast(tr(msgid("Metadata editing requires Local access")));
                    return;
                };
                let retry_shell = Rc::downgrade(&shell);
                present_local_access_recovery(
                    &shell,
                    selected,
                    &source_path,
                    Rc::new(move || {
                        if let Some(shell) = retry_shell.upgrade() {
                            present_metadata_dialog(&shell, item.clone());
                        }
                    }),
                );
            }
            Err(SourceMetadataError::Unavailable) => shell
                .control_feedback
                .show_feedback_toast(tr(msgid("Metadata editing is no longer available"))),
            Err(error) => shell.present_selected_dialog(&metadata_error_dialog(&error.to_string())),
        }
    });
    shell.selected_ui.metadata_load.replace(Some(load));
}
fn present_local_access_recovery(
    shell: &Rc<Shell>,
    selected: rufin_core::runtime::SelectedLibrary,
    source_path: &str,
    on_success: Rc<dyn Fn()>,
) {
    let resource = ui_shared::ui_resource::METADATA_DIALOG_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, {
        recovery_dialog: adw::Dialog,
        recovery_content: gtk::Box,
    });
    recovery_dialog.set_content_width(large_popup_content_width(650));
    let retry_requested = Rc::new(Cell::new(false));
    recovery_dialog.connect_closed({
        let retry_requested = Rc::clone(&retry_requested);
        move |_| {
            if retry_requested.replace(false) {
                let on_success = Rc::clone(&on_success);
                gtk::glib::idle_add_local_once(move || on_success());
            }
        }
    });
    let close = recovery_dialog.downgrade();
    let retry: Rc<dyn Fn()> = Rc::new(move || {
        retry_requested.set(true);
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });
    let access = shell
        .source
        .configured
        .borrow()
        .local_access
        .iter()
        .find(|summary| summary.source_id == selected.source_id)
        .and_then(|summary| summary.access.clone());
    ui_shared::local_access::mount_metadata_local_access_mapping(
        &shell.products.source,
        &shell.chrome.window,
        &selected.source_id,
        access.as_ref(),
        source_path,
        &recovery_content,
        retry,
    );
    shell.present_selected_dialog(&recovery_dialog);
}
