use crate::media_menus::MediaMenus;
use adw::prelude::*;
use gtk_widgets::interactions::ContextMenuSurface;
use localization::msgid;
use std::rc::Rc;

pub fn folder_name_dialog(
    initial_name: Option<&str>,
    save_name: impl Fn(String) + 'static,
) -> adw::Dialog {
    let resource = crate::ui_resource::PLAYLIST_FOLDER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        name_dialog: adw::Dialog, name: gtk::Entry, cancel: gtk::Button,
        save: gtk::Button, folder_labels: gtk::StringList,
    });
    if let Some(value) = initial_name {
        name.set_text(value);
        name_dialog.set_title(folder_labels.string(0).as_deref().unwrap());
        save.set_label(folder_labels.string(1).as_deref().unwrap());
    }
    let close = name_dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });
    let close = name_dialog.downgrade();
    save.connect_clicked(move |_| {
        let value = name.text().trim().to_owned();
        if value.is_empty() {
            return;
        }
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
        save_name(value);
    });
    name_dialog
}

pub fn present_new_folder_dialog(
    menus: &Rc<MediaMenus>,
    smart: bool,
    members: Vec<library::PlaylistFolderMember>,
    dismiss: Rc<dyn Fn()>,
) {
    let source = menus.source.clone();
    let weak = Rc::downgrade(menus);
    let dialog = folder_name_dialog(None, move |name| {
        let source = source.clone();
        let menus = weak.clone();
        let members = members.clone();
        let dismiss = Rc::clone(&dismiss);
        gtk::glib::spawn_future_local(async move {
            let result = async {
                let folder =
                    rufin_core::playlists::create_playlist_folder(&source, name, smart).await?;
                for member in members {
                    rufin_core::playlists::move_playlist_to_folder(&source, member, Some(folder))
                        .await?;
                }
                Ok::<_, sources::SourceError>(())
            }
            .await;
            match result {
                Ok(()) => dismiss(),
                Err(error) => {
                    if let Some(menus) = menus.upgrade() {
                        (menus.show_error)(error.to_string());
                    }
                }
            }
        });
    });
    (menus.present_dialog)(&dialog);
}

pub fn move_member(
    menus: &Rc<MediaMenus>,
    member: library::PlaylistFolderMember,
    folder: Option<library::PlaylistFolderKey>,
) {
    let source = menus.source.clone();
    let menus = Rc::downgrade(menus);
    gtk::glib::spawn_future_local(async move {
        if let Err(error) =
            rufin_core::playlists::move_playlist_to_folder(&source, member, folder).await
            && let Some(menus) = menus.upgrade()
        {
            (menus.show_error)(error.to_string());
        }
    });
}

pub fn append_folder_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    members: Vec<library::PlaylistFolderMember>,
    in_folder: bool,
) {
    let picker_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    picker_content.add_css_class("context-playlist-submenu");
    picker_content.set_hexpand(true);
    let popover = surface.popover().downgrade();
    let dismiss = Rc::new(move || {
        if let Some(popover) = popover.upgrade() {
            gtk_widgets::interactions::popdown_native_menu(&popover);
        }
    });
    populate_folder_picker(&picker_content, menus, members.clone(), dismiss);
    let picker = Rc::clone(menus);
    let picker_members = members.clone();
    surface.append_fixed_widget_submenu(
        msgid("Add to folder"),
        "folder-picker",
        &picker_content,
        "rufin-folders-symbolic",
        move || present_folder_picker(&picker, picker_members.clone()),
    );
    if in_folder {
        surface.append_fixed_action(
            msgid("Remove from folder"),
            "remove-from-folder",
            gtk_widgets::controls::REMOVE_ICON,
        );
        let menus = Rc::clone(menus);
        surface.add_action("remove-from-folder", move || {
            for member in &members {
                move_member(&menus, member.clone(), None);
            }
        });
    }
}

fn present_folder_picker(menus: &Rc<MediaMenus>, members: Vec<library::PlaylistFolderMember>) {
    let (picker_dialog, picker_content) = crate::playlist_picker::folder_picker_dialog();
    let close = picker_dialog.downgrade();
    populate_folder_picker(
        &picker_content,
        menus,
        members,
        Rc::new(move || {
            if let Some(dialog) = close.upgrade() {
                dialog.close();
            }
        }),
    );
    (menus.present_dialog)(&picker_dialog);
}

fn populate_folder_picker(
    root: &gtk::Box,
    menus: &Rc<MediaMenus>,
    members: Vec<library::PlaylistFolderMember>,
    dismiss: Rc<dyn Fn()>,
) {
    let smart = matches!(
        members.first(),
        Some(library::PlaylistFolderMember::SmartPlaylist { .. })
    );
    let create_menus = Rc::downgrade(menus);
    let create_members = members.clone();
    let create_dismiss = Rc::clone(&dismiss);
    let choose_menus = Rc::downgrade(menus);
    let choose_members = members.clone();
    let choose_dismiss = Rc::clone(&dismiss);
    let update = crate::playlist_picker::populate_context_picker(
        root,
        true,
        Rc::new(move |folder, _, _| {
            if let Some(menus) = choose_menus.upgrade() {
                for member in &choose_members {
                    move_member(&menus, member.clone(), Some(folder));
                }
            }
            choose_dismiss();
        }),
        Rc::new(move |_| {
            let Some(menus) = create_menus.upgrade() else {
                return;
            };
            present_new_folder_dialog(
                &menus,
                smart,
                create_members.clone(),
                Rc::clone(&create_dismiss),
            );
        }),
    );
    let database = menus.library.clone();
    let task = menus.runtime.spawn(async move {
        database
            .playlist_folder_choices(smart, &library::ReadCancellation::new())
            .await
    });
    let menus = Rc::downgrade(menus);
    gtk::glib::spawn_future_local(async move {
        let result = task.await;
        let Some(menus) = menus.upgrade() else { return };
        match result {
            Ok(Ok(rows)) => update(rows),
            Ok(Err(error)) => (menus.show_error)(error.to_string()),
            Err(error) => (menus.show_error)(error.to_string()),
        }
    });
}

pub fn present_folder_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    folder: library::PlaylistFolderRow,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "playlist-folder", position);
    crate::media_menus::install_sidebar_pin_action(
        &surface,
        menus,
        Some(rufin_core::settings::SidebarPin::PlaylistFolder {
            folder_id: folder.object_id.clone(),
            smart: folder.smart,
        }),
    );
    surface.append_fixed_action(
        msgid("Rename folder"),
        "rename",
        gtk_widgets::controls::EDIT_ICON,
    );
    let rename_menus = Rc::clone(menus);
    let rename_folder = folder.clone();
    surface.add_action("rename", move || {
        let source = rename_menus.source.clone();
        let menus = Rc::downgrade(&rename_menus);
        let key = rename_folder.folder_key;
        let dialog = folder_name_dialog(Some(&rename_folder.name), move |name| {
            let source = source.clone();
            let menus = menus.clone();
            gtk::glib::spawn_future_local(async move {
                if let Err(error) =
                    rufin_core::playlists::rename_playlist_folder(&source, key, name).await
                    && let Some(menus) = menus.upgrade()
                {
                    (menus.show_error)(error.to_string());
                }
            });
        });
        (rename_menus.present_dialog)(&dialog);
    });
    surface.append_fixed_action(
        msgid("Delete folder"),
        "delete",
        gtk_widgets::controls::DELETE_ICON,
    );
    let delete_menus = Rc::clone(menus);
    surface.add_action("delete", move || {
        let source = delete_menus.source.clone();
        let menus = Rc::downgrade(&delete_menus);
        let key = folder.folder_key;
        let smart = folder.smart;
        gtk::glib::spawn_future_local(async move {
            let result = rufin_core::playlists::delete_playlist_folder(&source, key).await;
            let Some(menus) = menus.upgrade() else { return };
            match result {
                Ok(()) => {
                    if (menus.current_route)()
                        == (gtk_widgets::route::Route::PlaylistFolder { key, smart })
                    {
                        (menus.navigate)(if smart {
                            gtk_widgets::route::Route::SmartPlaylists
                        } else {
                            gtk_widgets::route::Route::Playlists
                        });
                    }
                }
                Err(error) => (menus.show_error)(error.to_string()),
            }
        });
    });
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

pub fn single_regular_playlist_drag(source: &gtk_widgets::media_drag::MediaDragSource) -> bool {
    use gtk_widgets::media_drag::{CollectionTargets, MediaDragSource};
    let single = match source {
        MediaDragSource::Target { .. } => true,
        MediaDragSource::Targets {
            targets: CollectionTargets::Ready(targets),
        } => targets.len() == 1,
        _ => false,
    };
    single && playlist_drag_matches(source, false)
}

pub fn playlist_drag_matches(
    source: &gtk_widgets::media_drag::MediaDragSource,
    smart: bool,
) -> bool {
    use gtk_widgets::media_drag::{CollectionTargets, MediaDragSource};
    let matches_target = |target: &rufin_core::playback::PlayRequest| match target.selection() {
        Some(library::MemberSelection::Members { read, .. }) => match read.collection {
            library::Collection::Playlist(_) => !smart,
            library::Collection::Smart { .. } => smart,
            _ => false,
        },
        _ => false,
    };
    match source {
        MediaDragSource::Target { target } => matches_target(target),
        MediaDragSource::Targets {
            targets: CollectionTargets::Ready(targets),
        } => targets.iter().any(matches_target),
        MediaDragSource::Targets {
            targets: CollectionTargets::Parents(reads),
        } => reads.iter().any(|(read, _)| match read.kind {
            library::ListKind::Playlists => !smart,
            library::ListKind::SmartPlaylists { .. } => smart,
            _ => false,
        }),
        _ => false,
    }
}

pub fn drop_playlist_on_folder(
    menus: &Rc<MediaMenus>,
    folder_key: library::PlaylistFolderKey,
    smart: bool,
    source: gtk_widgets::media_drag::MediaDragSource,
) -> bool {
    use gtk_widgets::media_drag::{CollectionTargets, MediaDragSource};
    if !playlist_drag_matches(&source, smart) {
        return false;
    }
    let targets = match source {
        MediaDragSource::Target { target, .. } => CollectionTargets::Ready(vec![target]),
        MediaDragSource::Targets { targets, .. } => targets,
        _ => return false,
    };
    let database = menus.library.clone();
    let task = menus
        .runtime
        .spawn(async move { targets.collections(&database).await });
    let source = menus.source.clone();
    let menus = Rc::downgrade(menus);
    gtk::glib::spawn_future_local(async move {
        let result = async {
            let collections = task.await.map_err(|error| error.to_string())??;
            for collection in collections {
                let member = match collection {
                    library::Collection::Playlist(key) if !smart => {
                        library::PlaylistFolderMember::Playlist { key }
                    }
                    library::Collection::Smart { key, .. } if smart => {
                        library::PlaylistFolderMember::SmartPlaylist { key }
                    }
                    _ => continue,
                };
                rufin_core::playlists::move_playlist_to_folder(&source, member, Some(folder_key))
                    .await
                    .map_err(|error| error.to_string())?;
            }
            Ok::<_, String>(())
        }
        .await;
        if let Err(error) = result
            && let Some(menus) = menus.upgrade()
        {
            (menus.show_error)(error);
        }
    });
    true
}
