use adw::prelude::*;
use std::rc::Rc;

use crate::shell::Shell;
use ::library::{PlaylistKey, PlaylistPathMode};

impl Shell {
    pub(crate) fn edit_playlist_dialog(
        self: &Rc<Self>,
        playlist_id: PlaylistKey,
        current_name: String,
    ) {
        let shell = Rc::clone(self);
        gtk::glib::spawn_future_local(async move {
            let settings =
                match rufin_core::playlist_files::settings(&shell.products.source, playlist_id)
                    .recv()
                    .await
                {
                    Ok(Ok(settings)) => settings,
                    Ok(Err(error)) => {
                        shell.control_feedback.show_feedback_toast(error);
                        return;
                    }
                    Err(_) => return,
                };
            let resource = ui_shared::ui_resource::PLAYLIST_NAME_DIALOG_RESOURCE;
            let builder = ui_shared::ui_resource::builder(resource);
            if !settings.can_link {
                ui_shared::objects!(builder, resource, {
                    rename_dialog: adw::Dialog, rename_entry: gtk::Entry,
                    playlist_public: gtk::Switch, playlist_public_options: gtk::Box,
                    rename_cancel: gtk::Button, rename_apply: gtk::Button,
                });
                let public = match rufin_core::playlists::playlist_public(
                    &shell.products.source,
                    playlist_id,
                )
                .recv()
                .await
                {
                    Ok(Ok(public)) => public,
                    Ok(Err(error)) => {
                        shell.control_feedback.show_feedback_toast(error);
                        None
                    }
                    Err(_) => return,
                };
                playlist_public_options.set_visible(public.is_some());
                playlist_public.set_active(public.unwrap_or(false));
                rename_entry.set_text(&current_name);
                let dialog = rename_dialog.downgrade();
                rename_cancel.connect_clicked(move |_| {
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.close();
                    }
                });
                let dialog = rename_dialog.downgrade();
                let weak_shell = Rc::downgrade(&shell);
                rename_apply.connect_clicked(move |_| {
                    let name = rename_entry.text().trim().to_owned();
                    if name.is_empty() {
                        return;
                    }
                    let changed_name = name != current_name;
                    let changed_public = public
                        .is_some_and(|value| value != playlist_public.is_active())
                        .then_some(playlist_public.is_active());
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.close();
                    }
                    let Some(shell) = weak_shell.upgrade() else {
                        return;
                    };
                    gtk::glib::spawn_future_local(async move {
                        if (changed_name || changed_public.is_some())
                            && let Ok(Err(error)) = rufin_core::playlists::update_playlist(
                                &shell.products.source,
                                playlist_id,
                                changed_name.then_some(name),
                                changed_public,
                            )
                            .recv()
                            .await
                        {
                            shell.control_feedback.show_feedback_toast(error);
                        }
                    });
                });
                shell.present_selected_dialog(&rename_dialog);
                return;
            }
            ui_shared::objects!(builder, resource, {
                edit_dialog: adw::Dialog, name_entry: gtk::Entry,
                save_edit: gtk::Button, cancel_edit: gtk::Button,
                linked_file: adw::ActionRow, file_name: adw::EntryRow, auto_refresh: adw::SwitchRow,
                auto_save: adw::ComboRow, path_mode: adw::ComboRow,
                link_file: gtk::Button, unlink_file: gtk::Button, save_file: gtk::Button,
                reload_file: gtk::Button, delete_file: gtk::Button, file_error: gtk::Label,
                file_actions: gtk::Box,
            });
            let dialog = edit_dialog.downgrade();
            cancel_edit.connect_clicked(move |_| {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
            });
            name_entry.set_text(&current_name);
            let unlinked_subtitle = linked_file.subtitle().unwrap_or_default();
            linked_file.set_subtitle(
                settings
                    .display_path
                    .as_deref()
                    .unwrap_or(&unlinked_subtitle),
            );
            let linked = settings.link.is_some();
            let linked_rows = [
                auto_refresh.clone().upcast::<gtk::Widget>(),
                file_name.clone().upcast(),
                auto_save.clone().upcast(),
                path_mode.clone().upcast(),
                file_actions.upcast(),
            ]
            .map(|row| {
                row.set_visible(linked);
                row.downgrade()
            });
            if let Some(link) = settings.link {
                file_name.set_text(link.path.rsplit(['/', '\\']).next().unwrap_or(&link.path));
                auto_refresh.set_active(link.auto_refresh);
                auto_save.set_selected(match link.auto_save {
                    None => 0,
                    Some(false) => 1,
                    Some(true) => 2,
                });
                path_mode.set_selected(match link.path_mode {
                    PlaylistPathMode::Automatic => 0,
                    PlaylistPathMode::Relative => 1,
                    PlaylistPathMode::Absolute => 2,
                });
                if let Some(error) = link.error {
                    file_error.set_text(&error);
                    file_error.set_visible(true);
                }
            }
            let weak_shell = Rc::downgrade(&shell);
            let linked_row = linked_file.downgrade();
            file_name.connect_apply(move |row| {
                let Some(shell) = weak_shell.upgrade() else {
                    return;
                };
                let name = row.text().to_string();
                let linked_row = linked_row.clone();
                gtk::glib::spawn_future_local(async move {
                    match rufin_core::playlist_files::rename_file(
                        &shell.products.source,
                        playlist_id,
                        name,
                    )
                    .recv()
                    .await
                    {
                        Ok(Err(error)) => shell.control_feedback.show_feedback_toast(error),
                        Ok(Ok(())) => {
                            if let Ok(Ok(settings)) = rufin_core::playlist_files::settings(
                                &shell.products.source,
                                playlist_id,
                            )
                            .recv()
                            .await
                            {
                                if let Some(row) = linked_row.upgrade() {
                                    row.set_subtitle(
                                        settings.display_path.as_deref().unwrap_or(""),
                                    );
                                }
                            }
                        }
                        Err(_) => {}
                    }
                });
            });
            for (button, action) in [
                (save_edit, "edit"),
                (link_file, "link"),
                (unlink_file, "unlink"),
                (save_file, "save"),
                (reload_file, "reload"),
                (delete_file, "delete"),
            ] {
                let weak_shell = Rc::downgrade(&shell);
                let dialog = edit_dialog.downgrade();
                let entry = name_entry.clone();
                let refresh = auto_refresh.clone();
                let save = auto_save.clone();
                let mode = path_mode.clone();
                let rows = linked_rows.clone();
                let linked_row = linked_file.downgrade();
                let error_label = file_error.downgrade();
                let unlinked_subtitle = unlinked_subtitle.clone();
                button.connect_clicked(move |_| {
                    if let Some(shell) = weak_shell.upgrade() {
                        let name = entry.text().trim().to_owned();
                        if name.is_empty() {
                            return;
                        }
                        let result = rufin_core::playlist_files::update(
                            &shell.products.source,
                            playlist_id,
                            name,
                            refresh.is_active(),
                            match save.selected() {
                                1 => Some(false),
                                2 => Some(true),
                                _ => None,
                            },
                            match mode.selected() {
                                1 => PlaylistPathMode::Relative,
                                2 => PlaylistPathMode::Absolute,
                                _ => PlaylistPathMode::Automatic,
                            },
                        );
                        let dialog = dialog.clone();
                        let rows = rows.clone();
                        let linked_row = linked_row.clone();
                        let error_label = error_label.clone();
                        let entry = entry.clone();
                        let unlinked_subtitle = unlinked_subtitle.clone();
                        gtk::glib::spawn_future_local(async move {
                            match result.recv().await {
                                Ok(Ok(())) => {
                                    if matches!(action, "edit" | "link") {
                                        if let Some(dialog) = dialog.upgrade() {
                                            dialog.close();
                                        }
                                        if action == "link" {
                                            shell.link_playlist_file_dialog(playlist_id);
                                        }
                                        return;
                                    }
                                    let completed =
                                        shell.playlist_file_action(playlist_id, action).await;
                                    if action == "delete" && completed {
                                        if let Some(dialog) = dialog.upgrade() {
                                            dialog.close();
                                        }
                                        return;
                                    }
                                    if let Ok(Ok(settings)) = rufin_core::playlist_files::settings(
                                        &shell.products.source,
                                        playlist_id,
                                    )
                                    .recv()
                                    .await
                                    {
                                        for row in &rows {
                                            if let Some(row) = row.upgrade() {
                                                row.set_visible(settings.link.is_some());
                                            }
                                        }
                                        if let Some(row) = linked_row.upgrade() {
                                            row.set_subtitle(
                                                settings
                                                    .display_path
                                                    .as_deref()
                                                    .unwrap_or(&unlinked_subtitle),
                                            );
                                        }
                                        if let Some(label) = error_label.upgrade() {
                                            let error = settings
                                                .link
                                                .as_ref()
                                                .and_then(|link| link.error.as_deref());
                                            label.set_text(error.unwrap_or(""));
                                            label.set_visible(error.is_some());
                                        }
                                    }
                                    if completed
                                        && let Ok(rows) = shell
                                            .products
                                            .library
                                            .playlist_rows(
                                                &[playlist_id],
                                                &library::ReadCancellation::new(),
                                            )
                                            .await
                                        && let Some(row) = rows.first()
                                    {
                                        entry.set_text(&row.name);
                                    }
                                }
                                Ok(Err(error)) => shell.control_feedback.show_feedback_toast(error),
                                Err(_) => {}
                            }
                        });
                    }
                });
            }
            shell.present_selected_dialog(&edit_dialog);
        });
    }

    async fn playlist_file_action(self: &Rc<Self>, key: PlaylistKey, action: &'static str) -> bool {
        let owner = &self.products.source;
        let resource = ui_shared::ui_resource::PLAYLIST_NAME_DIALOG_RESOURCE;
        if action == "delete" {
            let dialog: adw::AlertDialog = ui_shared::ui_resource::object(
                &ui_shared::ui_resource::builder(resource),
                resource,
                "file_delete_confirmation",
            );
            ui_shared::popup::install_light_dismiss(&dialog);
            if dialog.choose_future(Some(&self.chrome.window)).await != "delete" {
                return false;
            }
        }
        let receiver = match action {
            "unlink" => rufin_core::playlist_files::unlink(owner, key),
            "reload" => rufin_core::playlist_files::reload(owner, key, false),
            "delete" => rufin_core::playlist_files::delete_file(owner, key),
            _ => rufin_core::playlist_files::save(owner, key, false),
        };
        let mut result = receiver.recv().await;
        if let Ok(Err(error)) = &result
            && matches!(action, "save" | "reload")
            && rufin_core::playlist_files::is_conflict(error)
        {
            let dialog: adw::AlertDialog = ui_shared::ui_resource::object(
                &ui_shared::ui_resource::builder(resource),
                resource,
                "file_conflict",
            );
            dialog.set_body(error);
            ui_shared::popup::install_light_dismiss(&dialog);
            let receiver = match dialog
                .choose_future(Some(&self.chrome.window))
                .await
                .as_str()
            {
                "load" => rufin_core::playlist_files::reload(owner, key, true),
                "replace" => rufin_core::playlist_files::save(owner, key, true),
                _ => return false,
            };
            result = receiver.recv().await;
        }
        match result {
            Ok(Ok(())) => true,
            Ok(Err(error)) => {
                self.control_feedback.show_feedback_toast(error);
                false
            }
            Err(_) => false,
        }
    }
}
