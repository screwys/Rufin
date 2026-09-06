use crate::shell::Shell;
use adw::prelude::*;
use gtk::{gio, glib};
use library::{BackupContents, BackupFrequency};
use localization::{msgid, tr, tr_with};
use std::rc::Rc;

const CONTENT_FIELDS: [fn(&mut BackupContents) -> &mut bool; 7] = [
    |v| &mut v.settings,
    |v| &mut v.saved_logins,
    |v| &mut v.playlists,
    |v| &mut v.favorites,
    |v| &mut v.local_imports,
    |v| &mut v.activity,
    |v| &mut v.queue,
];
fn content_rows(builder: &gtk::Builder, mut selected: BackupContents) -> [adw::SwitchRow; 7] {
    let resource = crate::ui_resource::BACKUP_DIALOG_RESOURCE;
    let ids = [
        "settings",
        "saved_logins",
        "playlists",
        "favorites",
        "local_imports",
        "activity",
        "queue",
    ];
    std::array::from_fn(|index| {
        let row: adw::SwitchRow =
            crate::ui_resource::object(builder, resource, &format!("content_{}", ids[index]));
        row.set_active(*CONTENT_FIELDS[index](&mut selected));
        row
    })
}
fn selected_contents(rows: &[adw::SwitchRow; 7]) -> BackupContents {
    let mut contents = BackupContents::default();
    for (index, row) in rows.iter().enumerate() {
        *CONTENT_FIELDS[index](&mut contents) = row.is_active();
    }
    contents
}

pub(super) fn groups(
    shell: &Rc<Shell>,
    navigation: &adw::NavigationView,
    navigation_controls: &super::PreferencesNavigationControls,
) -> [adw::PreferencesGroup; 2] {
    let resource = crate::ui_resource::BACKUP_PREFERENCES_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        backup_group: adw::PreferencesGroup, enabled: adw::SwitchRow, contents_slot: adw::PreferencesGroup,
        retention_count: gtk::SpinButton, schedule_error: adw::ActionRow, export_button: gtk::Button, restore_button: gtk::Button,
        automatic_row: adw::ActionRow, settings_page: adw::NavigationPage,
        folder_row: adw::ActionRow, choose_folder: gtk::Button, reset_folder: gtk::Button, open_folder: gtk::Button,
        encrypt: adw::SwitchRow, scheduled_password: adw::PasswordEntryRow,
        schedule: gtk::DropDown, weekday: gtk::DropDown, weekday_field: gtk::Box, hour: gtk::DropDown,
        activity_group: adw::PreferencesGroup, activity_scope: adw::PreferencesRow,
        listenbrainz: gtk::ToggleButton, jsonl: gtk::ToggleButton, current_source: gtk::ToggleButton, current_source_label: gtk::Label,
        activity_export: gtk::Button,
    });
    let settings = shell.settings.current.borrow().backup.clone();
    enabled.set_active(settings.enabled);
    retention_count.set_value(f64::from(settings.retention_count));
    encrypt.set_active(settings.encrypt);
    scheduled_password.set_visible(settings.encrypt);
    schedule.set_selected(u32::from(
        settings.schedule.frequency == BackupFrequency::Weekly,
    ));
    weekday_field.set_visible(settings.schedule.frequency == BackupFrequency::Weekly);
    weekday.set_selected(u32::from(settings.schedule.weekday));
    hour.set_selected(u32::from(settings.schedule.hour));
    if let Some(error) = shell.products.backup.schedule_error() {
        schedule_error.set_subtitle(&error);
        schedule_error.set_visible(true);
    }
    reset_folder.set_visible(settings.destination_uri.is_some());
    if let Some(uri) = settings.destination_uri {
        let path = gio::File::for_uri(&uri).parse_name();
        folder_row.set_subtitle(&path);
        automatic_row.set_subtitle(&path);
    }
    let navigation = navigation.downgrade();
    let controls = navigation_controls.clone();
    automatic_row.connect_activated(move |_| {
        if let Some(navigation) = navigation.upgrade() {
            navigation.push(&settings_page);
            controls.set_nested_page_visible(true);
        }
    });
    let weak = Rc::downgrade(shell);
    let encryption = encrypt.downgrade();
    enabled.connect_active_notify(move |row| {
        let Some(shell) = weak.upgrade() else { return };
        if row.is_active() == shell.settings.current.borrow().backup.enabled {
            return;
        }
        if !row.is_active() {
            shell.set_app_setting("automatic backups", false, |s| &mut s.backup.enabled);
            return;
        }
        row.set_sensitive(false);
        let row = row.downgrade();
        let encryption = encryption.clone();
        glib::spawn_future_local(async move {
            if let Some((password, _)) = options(&shell, false).await {
                let encrypted = password.is_some();
                let saved = match password {
                    Some(password) => shell
                        .products
                        .backup
                        .save_schedule_password(password)
                        .recv()
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result),
                    None => Ok(()),
                };
                match saved {
                    Ok(()) => {
                        shell.update_app_settings("automatic backups", |settings| {
                            settings.backup.encrypt = encrypted;
                            settings.backup.enabled = true;
                            true
                        });
                    }
                    Err(error) => feedback(&shell, Err(error), ""),
                }
            }
            let settings = shell.settings.current.borrow().backup.clone();
            if let Some(encryption) = encryption.upgrade() {
                encryption.set_active(settings.encrypt);
            }
            if let Some(row) = row.upgrade() {
                row.set_active(settings.enabled);
                row.set_sensitive(true);
            }
        });
    });
    let weak = Rc::downgrade(shell);
    let password = scheduled_password.downgrade();
    encrypt.connect_active_notify(move |row| {
        if let Some(password) = password.upgrade() {
            password.set_visible(row.is_active());
        }
        if let Some(shell) = weak.upgrade() {
            shell.set_app_setting("backup encryption", row.is_active(), |s| {
                &mut s.backup.encrypt
            });
        }
    });
    let weak = Rc::downgrade(shell);
    scheduled_password.connect_apply(move |row| {
        let Some(shell) = weak.upgrade() else { return };
        let password = row.text().to_string();
        let row = row.downgrade();
        glib::spawn_future_local(async move {
            let result = shell
                .products
                .backup
                .save_schedule_password(password)
                .recv()
                .await
                .map_err(|e| e.to_string())
                .and_then(|result| result);
            if result.is_ok() {
                if let Some(row) = row.upgrade() {
                    row.set_text("");
                }
            }
            feedback(&shell, result, msgid("Scheduled backup password saved"));
        });
    });
    let weak = Rc::downgrade(shell);
    let weekday_weak = weekday_field.downgrade();
    schedule.connect_selected_notify(move |row| {
        let weekly = row.selected() == 1;
        if let Some(weekday) = weekday_weak.upgrade() {
            weekday.set_visible(weekly);
        }
        if let Some(shell) = weak.upgrade() {
            shell.set_app_setting(
                "backup schedule",
                if weekly {
                    BackupFrequency::Weekly
                } else {
                    BackupFrequency::Daily
                },
                |s| &mut s.backup.schedule.frequency,
            );
        }
    });
    let weak = Rc::downgrade(shell);
    weekday.connect_selected_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.set_app_setting("backup weekday", row.selected() as u8, |s| {
                &mut s.backup.schedule.weekday
            });
        }
    });
    let weak = Rc::downgrade(shell);
    hour.connect_selected_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.set_app_setting("backup hour", row.selected() as u8, |s| {
                &mut s.backup.schedule.hour
            });
        }
    });
    let weak = Rc::downgrade(shell);
    open_folder.connect_clicked(move |_| {
        let Some(shell) = weak.upgrade() else { return };
        let folder = shell
            .settings
            .current
            .borrow()
            .backup
            .destination_uri
            .as_deref()
            .map(gio::File::for_uri)
            .unwrap_or_else(|| gio::File::for_path(shell.products.backup.default_directory()));
        super::open_folder(&shell, folder);
    });
    let weak = Rc::downgrade(shell);
    let folder = folder_row.downgrade();
    let summary = automatic_row.downgrade();
    let reset = reset_folder.downgrade();
    choose_folder.connect_clicked(move |_| {
        let Some(shell) = weak.upgrade() else { return };
        let (folder, summary, reset) = (folder.clone(), summary.clone(), reset.clone());
        glib::spawn_future_local(async move {
            if let Ok(selected) = chooser("folder_chooser")
                .select_folder_future(Some(&shell.chrome.window))
                .await
            {
                if shell
                    .set_app_setting(
                        "backup destination",
                        Some(selected.uri().to_string()),
                        |s| &mut s.backup.destination_uri,
                    )
                    .is_some()
                {
                    if let Some(row) = folder.upgrade() {
                        row.set_subtitle(&selected.parse_name());
                    }
                    if let Some(row) = summary.upgrade() {
                        row.set_subtitle(&selected.parse_name());
                    }
                    if let Some(reset) = reset.upgrade() {
                        reset.set_visible(true);
                    }
                }
            }
        });
    });
    let weak = Rc::downgrade(shell);
    let folder = folder_row.downgrade();
    let summary = automatic_row.downgrade();
    reset_folder.connect_clicked(move |button| {
        let Some(shell) = weak.upgrade() else { return };
        if shell
            .set_app_setting("backup destination", None, |s| {
                &mut s.backup.destination_uri
            })
            .is_some()
        {
            if let Some(row) = folder.upgrade() {
                row.set_subtitle(&tr("Rufin data folder"));
            }
            if let Some(row) = summary.upgrade() {
                row.set_subtitle(&tr("Rufin data folder"));
            }
            button.set_visible(false);
        }
    });
    let weak = Rc::downgrade(shell);
    export_button.connect_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            export_dialog(&shell);
        }
    });
    let weak = Rc::downgrade(shell);
    restore_button.connect_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            import_dialog(&shell);
        }
    });
    let selected = {
        let configured = shell.source.configured.borrow();
        configured
            .sources
            .iter()
            .find(|source| Some(&source.id) == configured.selected_source_id.as_ref())
            .cloned()
    };
    activity_scope.set_visible(selected.is_some());
    if let Some(source) = &selected {
        let name = super::source::configured_source_display_name(source);
        current_source_label.set_label(&name);
        current_source.set_tooltip_text(Some(&name));
        current_source.update_property(&[gtk::accessible::Property::Label(&name)]);
    }
    let weak = Rc::downgrade(shell);
    let (listenbrainz, jsonl, current_source) = (
        listenbrainz.downgrade(),
        jsonl.downgrade(),
        current_source.downgrade(),
    );
    activity_export.connect_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            let format = if jsonl.upgrade().is_some_and(|button| button.is_active()) {
                None
            } else if listenbrainz
                .upgrade()
                .is_some_and(|button| button.is_active())
            {
                Some(library::ActivityCsvFormat::ListenBrainz)
            } else {
                Some(library::ActivityCsvFormat::LastFm)
            };
            let source = selected
                .as_ref()
                .filter(|_| {
                    current_source
                        .upgrade()
                        .is_some_and(|button| button.is_active())
                })
                .map(|source| source.id.clone());
            shell.export_activity_dialog(format, source);
        }
    });
    let resource = crate::ui_resource::BACKUP_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    let contents_group: adw::PreferencesGroup =
        crate::ui_resource::object(&builder, resource, "contents_group");
    for (index, row) in content_rows(&builder, settings.contents)
        .into_iter()
        .enumerate()
    {
        contents_group.remove(&row);
        contents_slot.add(&row);
        let weak = Rc::downgrade(shell);
        row.connect_active_notify(move |row| {
            if let Some(shell) = weak.upgrade() {
                shell.set_app_setting("backup contents", row.is_active(), |s| {
                    CONTENT_FIELDS[index](&mut s.backup.contents)
                });
            }
        });
    }
    let weak = Rc::downgrade(shell);
    retention_count.connect_value_changed(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.set_app_setting("backup retention", row.value() as u32, |s| {
                &mut s.backup.retention_count
            });
        }
    });
    [backup_group, activity_group]
}

fn export_dialog(shell: &Rc<Shell>) {
    let shell = Rc::clone(shell);
    glib::spawn_future_local(async move {
        let Some((passphrase, _)) = options(&shell, false).await else {
            return;
        };
        let Ok(destination) = chooser("export_chooser")
            .save_future(Some(&shell.chrome.window))
            .await
        else {
            return;
        };
        let result = async {
            let path = shell
                .products
                .backup
                .export(passphrase)
                .recv()
                .await
                .map_err(|e| e.to_string())??;
            let input = gio::File::for_path(&path)
                .read_future(glib::Priority::DEFAULT)
                .await
                .map_err(|e| e.to_string())?;
            let output = destination
                .replace_future(
                    None,
                    false,
                    gio::FileCreateFlags::PRIVATE | gio::FileCreateFlags::REPLACE_DESTINATION,
                    glib::Priority::DEFAULT,
                )
                .await
                .map_err(|e| e.to_string())?;
            output
                .splice_future(
                    &input,
                    gio::OutputStreamSpliceFlags::CLOSE_SOURCE
                        | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
                    glib::Priority::DEFAULT,
                )
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        .await;
        feedback(&shell, result, msgid("Backup exported"));
    });
}

pub(crate) fn import_dialog(shell: &Rc<Shell>) {
    let shell = Rc::clone(shell);
    glib::spawn_future_local(async move {
        let Some((passphrase, contents)) = options(&shell, true).await else {
            return;
        };
        let Ok(input) = chooser("restore_chooser")
            .open_future(Some(&shell.chrome.window))
            .await
        else {
            return;
        };
        let result = async {
            let temporary = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
            input
                .copy_future(
                    &gio::File::for_path(temporary.path()),
                    gio::FileCopyFlags::OVERWRITE,
                    glib::Priority::DEFAULT,
                )
                .0
                .await
                .map_err(|e| e.to_string())?;
            shell
                .products
                .backup
                .stage(temporary.path().to_path_buf(), passphrase, contents)
                .recv()
                .await
                .map_err(|e| e.to_string())?
        }
        .await;
        let preview = match result {
            Ok(preview) => preview,
            Err(error) => {
                feedback(&shell, Err(error), "");
                return;
            }
        };
        let resource = crate::ui_resource::BACKUP_DIALOG_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        let confirm: adw::AlertDialog =
            crate::ui_resource::object(&builder, resource, "restore_confirm");
        if preview.contents.settings && !preview.removed_sources.is_empty() {
            confirm.set_body(&tr_with("Replace the selected contents and remove these configured sources?\n{sources}\nTheir retained user state and saved logins will remain unless selected.",
                &[("sources", &preview.removed_sources.join("\n"))]));
        }
        if confirm
            .choose_future(Some(&shell.chrome.window))
            .await
            .as_str()
            != "restore"
        {
            return;
        }
        let affected = preview.contents;
        match shell
            .products
            .backup
            .restore(preview)
            .recv()
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r)
        {
            Ok(report) => {
                shell.complete_add_server_dialog();
                *shell.settings.current.borrow_mut() = shell.settings.persistence.load();
                if let Some(dialog) = shell.preferences.active_dialog() {
                    dialog.close();
                }
                if affected.settings {
                    shell.appearance.apply(&shell.settings.current.borrow());
                    shell.update_layout();
                    shell.refresh_tray_private_mode();
                    shell.update_media_controls();
                    shell.render_lyrics_panel();
                }
                if affected.settings || affected.playlists {
                    shell.rebuild_sidebar_navigation();
                }
                shell.render_current_route();
                if affected.queue {
                    shell.render_queue_panel();
                }
                if !report.warnings.is_empty() {
                    shell.show_feedback_toast(tr_with(
                        "Backup restored with warnings: {warnings}",
                        &[("warnings", &report.warnings.join("\n"))],
                    ));
                } else if report.skipped_playlist_entries > 0 || report.skipped_listens > 0 {
                    shell.show_feedback_toast(tr_with("Backup restored with {entries} skipped playlist entries and {listens} skipped listens.",
                        &[("entries", &report.skipped_playlist_entries.to_string()), ("listens", &report.skipped_listens.to_string())]));
                } else {
                    feedback(&shell, Ok(()), msgid("Backup restored"));
                }
            }
            Err(error) => feedback(&shell, Err(error), ""),
        }
    });
}
async fn options(shell: &Shell, importing: bool) -> Option<(Option<String>, BackupContents)> {
    let resource = crate::ui_resource::BACKUP_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        options: adw::AlertDialog, options_box: gtk::Box, password: adw::PasswordEntryRow,
        contents_group: adw::PreferencesGroup, export_options: adw::AlertDialog,
        do_not_encrypt: adw::SwitchRow, export_password: adw::PasswordEntryRow,
    });
    let rows = content_rows(&builder, BackupContents::default());
    if importing {
        options_box.append(&contents_group);
    }
    let (options, password) = if importing {
        (options, password)
    } else {
        export_options.set_response_enabled("continue", false);
        let dialog = export_options.downgrade();
        let skip = do_not_encrypt.downgrade();
        export_password.connect_text_notify(move |row| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.set_response_enabled(
                    "continue",
                    !row.text().is_empty() || skip.upgrade().is_some_and(|skip| skip.is_active()),
                );
            }
        });
        let dialog = export_options.downgrade();
        let password = export_password.downgrade();
        do_not_encrypt.connect_active_notify(move |row| {
            if let (Some(dialog), Some(password)) = (dialog.upgrade(), password.upgrade()) {
                password.set_sensitive(!row.is_active());
                dialog.set_response_enabled(
                    "continue",
                    row.is_active() || !password.text().is_empty(),
                );
            }
        });
        (export_options, export_password)
    };
    if options
        .choose_future(Some(&shell.chrome.window))
        .await
        .as_str()
        != "continue"
    {
        return None;
    }
    Some((
        Some(password.text().to_string())
            .filter(|v| !v.is_empty() && (importing || !do_not_encrypt.is_active())),
        selected_contents(&rows),
    ))
}
fn feedback(shell: &Shell, result: Result<(), String>, success: &str) {
    match result {
        Ok(()) => shell.show_feedback_toast(tr(success)),
        Err(error) => {
            shell.show_feedback_toast(tr_with("Backup failed: {error}", &[("error", &error)]))
        }
    }
}
fn chooser(id: &str) -> gtk::FileDialog {
    let resource = crate::ui_resource::BACKUP_DIALOG_RESOURCE;
    crate::ui_resource::object(&crate::ui_resource::builder(resource), resource, id)
}
