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
    let resource = crate::ui_resource::PLAYLIST_FOLDER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, { picker_toolbar: adw::ToolbarView, picker_content: gtk::Box });
    picker_toolbar.set_content(None::<&gtk::Widget>);
    let popover = surface.popover().downgrade();
    let dismiss = Rc::new(move || {
        if let Some(popover) = popover.upgrade() {
            gtk_widgets::interactions::popdown_native_menu(&popover);
        }
    });
    populate_folder_picker(&builder, menus, members.clone(), dismiss);
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
    let resource = crate::ui_resource::PLAYLIST_FOLDER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, { picker_dialog: adw::Dialog });
    let close = picker_dialog.downgrade();
    populate_folder_picker(
        &builder,
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
    builder: &gtk::Builder,
    menus: &Rc<MediaMenus>,
    members: Vec<library::PlaylistFolderMember>,
    dismiss: Rc<dyn Fn()>,
) {
    let resource = crate::ui_resource::PLAYLIST_FOLDER_RESOURCE;
    gtk_widgets::objects!(builder, resource, { search: gtk::SearchEntry, folders: gtk::ListBox, create_folder: gtk::Button });
    gtk_widgets::controls::configure_search_entry(&search);
    let list = folders.downgrade();
    search.connect_search_changed(move |search| {
        let Some(list) = list.upgrade() else { return };
        let query = search.text().trim().to_lowercase();
        let mut child = list.first_child();
        while let Some(row) = child {
            child = row.next_sibling();
            if let Some(row) = row.downcast_ref::<gtk::ListBoxRow>()
                && let Some(label) = row.child().and_downcast::<gtk::Label>()
            {
                row.set_visible(label.text().to_lowercase().contains(&query));
            }
        }
    });
    let smart = matches!(
        members.first(),
        Some(library::PlaylistFolderMember::SmartPlaylist { .. })
    );
    let create_menus = Rc::downgrade(menus);
    let create_members = members.clone();
    let create_dismiss = Rc::clone(&dismiss);
    create_folder.connect_clicked(move |_| {
        let Some(menus) = create_menus.upgrade() else {
            return;
        };
        let source = menus.source.clone();
        let weak = Rc::downgrade(&menus);
        let members = create_members.clone();
        let dismiss = Rc::clone(&create_dismiss);
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
                        rufin_core::playlists::move_playlist_to_folder(
                            &source,
                            member,
                            Some(folder),
                        )
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
    });
    let database = menus.library.clone();
    let task = menus.runtime.spawn(async move {
        database
            .playlist_folders(smart, &library::ReadCancellation::new())
            .await
    });
    let menus = Rc::downgrade(menus);
    let folders = folders.downgrade();
    gtk::glib::spawn_future_local(async move {
        let (Some(menus), Some(folders)) = (menus.upgrade(), folders.upgrade()) else {
            return;
        };
        let rows = match task.await {
            Ok(Ok(rows)) => rows,
            Ok(Err(error)) => {
                (menus.show_error)(error.to_string());
                return;
            }
            Err(error) => {
                (menus.show_error)(error.to_string());
                return;
            }
        };
        for folder in rows {
            let row = gtk::ListBoxRow::new();
            let label = gtk::Label::new(Some(&folder.name));
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_margin_start(4);
            label.set_margin_end(4);
            label.set_margin_top(2);
            label.set_margin_bottom(2);
            row.set_child(Some(&label));
            let members = members.clone();
            let menus = Rc::downgrade(&menus);
            let dismiss = Rc::clone(&dismiss);
            row.connect_activate(move |_| {
                if let Some(menus) = menus.upgrade() {
                    for member in &members {
                        move_member(&menus, member.clone(), Some(folder.folder_key));
                    }
                }
                dismiss();
            });
            folders.append(&row);
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
    folder: library::PlaylistFolderRow,
    source: gtk_widgets::media_drag::MediaDragSource,
) -> bool {
    use gtk_widgets::media_drag::{CollectionTargets, MediaDragSource};
    if !playlist_drag_matches(&source, folder.smart) {
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
                    library::Collection::Playlist(key) if !folder.smart => {
                        library::PlaylistFolderMember::Playlist { key }
                    }
                    library::Collection::Smart { key, .. } if folder.smart => {
                        library::PlaylistFolderMember::SmartPlaylist { key }
                    }
                    _ => continue,
                };
                rufin_core::playlists::move_playlist_to_folder(
                    &source,
                    member,
                    Some(folder.folder_key),
                )
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
