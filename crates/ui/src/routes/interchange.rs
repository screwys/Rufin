use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use localization::tr;

use super::route::Route;
use crate::shell::Shell;

pub(crate) enum PlaylistExport {
    Playlist(library::PlaylistKey),
    Smart(library::SmartPlaylistKey),
}

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
    pub(crate) fn import_playlist_dialog(self: &Rc<Self>) {
        let shell = Rc::clone(self);
        glib::spawn_future_local(async move {
            let dialog = gtk::FileDialog::builder()
                .title(tr("Import Playlist"))
                .build();
            let Ok(file) = dialog.open_future(Some(&shell.chrome.window)).await else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            match shell.products.source.import_playlist(path).recv().await {
                Ok(Ok(report)) => {
                    shell.navigate(Route::PlaylistDetail(report.playlist));
                    if report.skipped > 0 {
                        shell.chrome.toast_overlay.add_toast(adw::Toast::new(&tr(
                            "Some playlist entries could not be imported",
                        )));
                    }
                }
                Ok(Err(error)) => shell
                    .chrome
                    .toast_overlay
                    .add_toast(adw::Toast::new(&error)),
                Err(_) => {}
            }
        });
    }

    pub(crate) fn export_playlist_dialog(self: &Rc<Self>, target: PlaylistExport, name: &str) {
        let filename = playlist_export_filename(name);
        let shell = Rc::clone(self);
        let selected = self.selected_library();
        let source = selected.as_ref().map(|selected| selected.source_key);
        let folder = selected
            .as_ref()
            .and_then(|selected| selected.music_folder_key);
        glib::spawn_future_local(async move {
            let dialog = gtk::FileDialog::builder()
                .title(tr("Export Playlist"))
                .initial_name(&filename)
                .build();
            let Ok(file) = dialog.save_future(Some(&shell.chrome.window)).await else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            let database = Arc::clone(&shell.products.library);
            let task = shell.products.runtime.spawn(async move {
                let mut output = std::io::BufWriter::new(std::fs::File::create(&path)?);
                let count = match target {
                    PlaylistExport::Playlist(key) => {
                        database.export_playlist_m3u(key, &path, &mut output).await
                    }
                    PlaylistExport::Smart(key) => {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64;
                        database
                            .export_smart_playlist_m3u(key, source, folder, now, &path, &mut output)
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

    pub(crate) fn export_activity_dialog(
        self: &Rc<Self>,
        format: Option<library::ActivityCsvFormat>,
        source_id: Option<sources::SourceId>,
    ) {
        let shell = Rc::clone(self);
        glib::spawn_future_local(async move {
            let resource = crate::ui_resource::BACKUP_DIALOG_RESOURCE;
            let dialog: gtk::FileDialog = crate::ui_resource::object(
                &crate::ui_resource::builder(resource),
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
