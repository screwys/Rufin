use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use localization::tr;

use crate::shell::Shell;
use rufin_core::runtime::source::PlaylistExport;
use ui_shared::route::Route;

fn playlist_export_filename(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim().trim_matches('.');
    let name = if name.is_empty() { "playlist" } else { name };
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            });
    format!("{}{name}.m3u8", if reserved { "_" } else { "" })
}

impl Shell {
    pub(crate) fn link_playlist_file_dialog(self: &Rc<Self>, key: library::PlaylistKey) {
        let shell = Rc::clone(self);
        glib::spawn_future_local(async move {
            let resource = ui_shared::ui_resource::PLAYLIST_NAME_DIALOG_RESOURCE;
            let dialog: adw::AlertDialog = ui_shared::ui_resource::object(
                &ui_shared::ui_resource::builder(resource),
                resource,
                "link_choice",
            );
            let replace = match dialog
                .choose_future(Some(&shell.chrome.window))
                .await
                .as_str()
            {
                "load" => false,
                "replace" => true,
                _ => return,
            };
            let Some(location) = playlist_file_location(&shell, "playlist.m3u8").await else {
                return;
            };
            let result = match location {
                FileLocation::Source(source, path) => rufin_core::playlist_files::link_source(
                    &shell.products.source,
                    key,
                    source,
                    path,
                    replace,
                ),
                FileLocation::Device => {
                    let dialog = playlist_chooser();
                    let file = if replace {
                        dialog.save_future(Some(&shell.chrome.window)).await
                    } else {
                        dialog.open_future(Some(&shell.chrome.window)).await
                    };
                    let Ok(file) = file else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        return;
                    };
                    rufin_core::playlist_files::link_local(
                        &shell.products.source,
                        key,
                        path,
                        replace,
                    )
                }
            };
            if let Ok(Err(error)) = result.recv().await {
                shell.control_feedback.show_feedback_toast(error);
            }
        });
    }

    pub(crate) fn import_playlist_dialog(self: &Rc<Self>) {
        let shell = Rc::clone(self);
        glib::spawn_future_local(async move {
            let resource = crate::ui_resource::PLAYLIST_FILE_DIALOG_RESOURCE;
            let builder = ui_shared::ui_resource::builder(resource);
            ui_shared::objects!(builder, resource, { import_options: adw::AlertDialog, import_copy: adw::SwitchRow });
            if import_options
                .choose_future(Some(&shell.chrome.window))
                .await
                != "import"
            {
                return;
            }
            let linked = !import_copy.is_active();
            let Some(location) = playlist_file_location(&shell, "").await else {
                return;
            };
            let receivers = if let FileLocation::Source(source, path) = location {
                vec![rufin_core::playlists::import_source_playlist(
                    &shell.products.source,
                    source,
                    path,
                    linked,
                )]
            } else {
                let dialog = playlist_chooser();
                let Ok(files) = dialog
                    .open_multiple_future(Some(&shell.chrome.window))
                    .await
                else {
                    return;
                };
                files
                    .iter::<gtk::gio::File>()
                    .filter_map(Result::ok)
                    .filter_map(|file| file.path())
                    .map(|path| {
                        rufin_core::playlists::import_playlist(
                            &shell.products.source,
                            path,
                            shell
                                .source
                                .configured
                                .borrow()
                                .selected_source_id
                                .as_ref()
                                .and_then(|id| shell.products.source.configuration(id)),
                            linked,
                        )
                    })
                    .collect::<Vec<_>>()
            };
            for receiver in receivers {
                match receiver.recv().await {
                    Ok(Ok(report)) => {
                        shell.navigate(Route::PlaylistDetail(report.playlist));
                        if report.skipped > 0 {
                            shell.control_feedback.show_feedback_toast(tr(
                                "Some playlist entries could not be imported",
                            ));
                        }
                    }
                    Ok(Err(error)) => shell.control_feedback.show_feedback_toast(error),
                    Err(_) => {}
                }
            }
        });
    }

    pub(crate) fn export_playlist_dialog(self: &Rc<Self>, target: PlaylistExport, name: &str) {
        let mut filename = playlist_export_filename(name);
        let shell = Rc::clone(self);
        let selected = self.selected_library();
        let source = selected.as_ref().map(|selected| selected.source_key);
        let folder = selected
            .as_ref()
            .and_then(|selected| selected.music_folder_key);
        glib::spawn_future_local(async move {
            let resource = crate::ui_resource::PLAYLIST_FILE_DIALOG_RESOURCE;
            let builder = ui_shared::ui_resource::builder(resource);
            ui_shared::objects!(builder, resource, { export_options: adw::AlertDialog, export_format: adw::ComboRow, export_path_mode: adw::ComboRow, export_link: adw::SwitchRow });
            let can_link = if let PlaylistExport::Playlist(key) = &target {
                match rufin_core::playlist_files::settings(&shell.products.source, *key)
                    .recv()
                    .await
                {
                    Ok(Ok(settings)) => settings.can_link,
                    Ok(Err(error)) => {
                        shell.control_feedback.show_feedback_toast(error);
                        return;
                    }
                    Err(_) => return,
                }
            } else {
                false
            };
            export_link.set_visible(can_link);
            ui_shared::popup::install_light_dismiss(&export_options);
            if export_options
                .choose_future(Some(&shell.chrome.window))
                .await
                != "export"
            {
                return;
            }
            let extension = match export_format.selected() {
                1 => "m3u",
                2 => "pls",
                3 => "xspf",
                _ => "m3u8",
            };
            filename = format!("{}.{}", filename.trim_end_matches(".m3u8"), extension);
            let mode = match export_path_mode.selected() {
                1 => library::PlaylistPathMode::Relative,
                2 => library::PlaylistPathMode::Absolute,
                _ => library::PlaylistPathMode::Automatic,
            };
            let linked = can_link && export_link.is_active();
            let Some(location) = playlist_file_location(&shell, &filename).await else {
                return;
            };
            if let FileLocation::Source(destination, path) = location {
                if let Ok(Err(error)) = rufin_core::playlists::export_playlist(
                    &shell.products.source,
                    Some(destination),
                    path.into(),
                    target,
                    source.map(|source| (source, folder)),
                    mode,
                    linked,
                )
                .recv()
                .await
                {
                    shell.control_feedback.show_feedback_toast(error);
                }
                return;
            }
            let dialog = playlist_chooser();
            dialog.set_initial_name(Some(&filename));
            let Ok(file) = dialog.save_future(Some(&shell.chrome.window)).await else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            if let Ok(Err(error)) = rufin_core::playlists::export_playlist(
                &shell.products.source,
                None,
                path,
                target,
                source.map(|source| (source, folder)),
                mode,
                linked,
            )
            .recv()
            .await
            {
                shell.control_feedback.show_feedback_toast(error);
            }
        });
    }

    pub(crate) fn export_activity_dialog(
        self: &Rc<Self>,
        format: Option<library::ActivityCsvFormat>,
        source_id: Option<sources::SourceId>,
    ) {
        let shell = Rc::clone(self);
        glib::spawn_future_local(async move {
            let resource = crate::ui_resource::BACKUP_DIALOG_RESOURCE;
            let dialog: gtk::FileDialog = ui_shared::ui_resource::object(
                &ui_shared::ui_resource::builder(resource),
                resource,
                "activity_chooser",
            );
            dialog.set_initial_name(Some(if format.is_some() {
                "activity.csv"
            } else {
                "activity.jsonl"
            }));
            let Ok(file) = dialog.save_future(Some(&shell.chrome.window)).await else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            let database = Arc::clone(&shell.products.library);
            let task = shell.products.runtime.spawn(async move {
                let mut output = std::io::BufWriter::new(std::fs::File::create(path)?);
                let count = match format {
                    Some(format) => {
                        database
                            .export_activity_csv(&mut output, format, source_id.as_ref())
                            .await
                    }
                    None => {
                        database
                            .export_activity_jsonl(&mut output, source_id.as_ref())
                            .await
                    }
                }?;
                output.flush()?;
                Ok::<_, library::LibraryError>(count)
            });
            if let Ok(Err(error)) = task.await {
                shell
                    .chrome
                    .toast_overlay
                    .add_toast(adw::Toast::new(&error.to_string()));
            }
        });
    }
}

enum FileLocation {
    Device,
    Source(sources::SourceId, String),
}

fn playlist_chooser() -> gtk::FileDialog {
    let resource = crate::ui_resource::PLAYLIST_FILE_DIALOG_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, { chooser: gtk::FileDialog, playlist_filter: gtk::FileFilter });
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&playlist_filter);
    chooser.set_filters(Some(&filters));
    chooser.set_default_filter(Some(&playlist_filter));
    chooser
}

async fn playlist_file_location(shell: &Rc<Shell>, filename: &str) -> Option<FileLocation> {
    let selected = shell
        .selected_library()
        .map(|selected| selected.source_id.clone());
    let source =
        selected.and_then(|id| shell.products.source.configured_source(&id).ok().flatten());
    let Some(source) = source.filter(|source| source.file_settings.is_some()) else {
        return Some(FileLocation::Device);
    };
    let resource = crate::ui_resource::PLAYLIST_FILE_DIALOG_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, { dialog: adw::AlertDialog, path: adw::EntryRow });
    dialog.set_body(&source.source.name);
    path.set_text(filename);
    dialog.set_response_enabled("source", !filename.is_empty());
    let weak = dialog.downgrade();
    path.connect_changed(move |path| {
        if let Some(dialog) = weak.upgrade() {
            dialog.set_response_enabled("source", !path.text().trim().is_empty());
        }
    });
    ui_shared::popup::install_light_dismiss(&dialog);
    match dialog
        .choose_future(Some(&shell.chrome.window))
        .await
        .as_str()
    {
        "device" => Some(FileLocation::Device),
        "source" => Some(FileLocation::Source(
            source.source.id,
            path.text().trim().into(),
        )),
        _ => None,
    }
}
