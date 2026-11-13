use crate::shell::shell::Shell;
use crate::widgets::interactions::ContextMenuSurface;
use adw::prelude::*;
use localization::msgid;
use rufin_core::route::Route;
use std::rc::Rc;

pub fn folder_name_dialog(
    initial_name: Option<&str>,
    save_name: impl Fn(String) + 'static,
) -> adw::Dialog {
    let resource = "routes/playlist_folder.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
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
    shell: &Rc<Shell>,
    members: Vec<library::PlaylistFolderMember>,
    dismiss: Rc<dyn Fn()>,
) {
    let source = shell.products.source.clone();
    let weak = Rc::downgrade(shell);
    let dialog = folder_name_dialog(None, move |name| {
        let source = source.clone();
        let shell = weak.clone();
        let members = members.clone();
        let dismiss = Rc::clone(&dismiss);
        gtk::glib::spawn_future_local(async move {
            let result = async {
                let folder = rufin_core::playlists::create_playlist_folder(&source, name).await?;
                if !members.is_empty() {
                    rufin_core::playlists::add_to_playlist_folder(&source, members, folder).await?;
                }
                Ok::<_, sources::SourceError>(())
            }
            .await;
            match result {
                Ok(()) => dismiss(),
                Err(error) => {
                    if let Some(shell) = shell.upgrade() {
                        shell
                            .control_feedback
                            .show_feedback_toast(error.to_string());
                    }
                }
            }
        });
    });
    shell.present_selected_dialog(&dialog);
}

/// Puts `members` in `folder`, or takes them out of the folder `removed` names: the page's
/// folder, or every folder when None.
fn change_members(
    shell: &Rc<Shell>,
    members: Vec<library::PlaylistFolderMember>,
    folder: Result<library::PlaylistFolderKey, Option<library::PlaylistFolderKey>>,
) {
    let source = shell.products.source.clone();
    let shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let result = match folder {
            Ok(folder) => {
                rufin_core::playlists::add_to_playlist_folder(&source, members, folder).await
            }
            Err(removed) => {
                rufin_core::playlists::remove_from_playlist_folder(&source, members, removed).await
            }
        };
        if let Err(error) = result
            && let Some(shell) = shell.upgrade()
        {
            shell
                .control_feedback
                .show_feedback_toast(error.to_string());
        }
    });
}

pub fn append_folder_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    members: Vec<library::PlaylistFolderMember>,
    in_folder: bool,
) {
    let picker_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    picker_content.add_css_class("context-playlist-submenu");
    picker_content.set_hexpand(true);
    let popover = surface.popover().downgrade();
    let dismiss = Rc::new(move || {
        if let Some(popover) = popover.upgrade() {
            crate::widgets::interactions::popdown_native_menu(&popover);
        }
    });
    populate_folder_picker(&picker_content, shell, members.clone(), dismiss);
    let picker = Rc::clone(shell);
    let picker_members = members.clone();
    surface.append_fixed_widget_submenu(
        msgid("Add to folder"),
        "folder-picker",
        &picker_content,
        "rufin-folders-symbolic",
        move || present_folder_picker(&picker, picker_members.clone()),
    );
    if in_folder {
        let page = match shell.navigation.routes.borrow().current() {
            Route::PlaylistFolder { key, .. } => Some(*key),
            _ => None,
        };
        let shell = Rc::clone(shell);
        surface.append_fixed_action(
            msgid("Remove from folder"),
            crate::widgets::controls::REMOVE_ICON,
            move || change_members(&shell, members.clone(), Err(page)),
        );
    }
}

fn present_folder_picker(shell: &Rc<Shell>, members: Vec<library::PlaylistFolderMember>) {
    let (picker_dialog, picker_content) = crate::playlists::picker::folder_picker_dialog();
    let close = picker_dialog.downgrade();
    populate_folder_picker(
        &picker_content,
        shell,
        members,
        Rc::new(move || {
            if let Some(dialog) = close.upgrade() {
                dialog.close();
            }
        }),
    );
    shell.present_selected_dialog(&picker_dialog);
}

fn populate_folder_picker(
    root: &gtk::Box,
    shell: &Rc<Shell>,
    members: Vec<library::PlaylistFolderMember>,
    dismiss: Rc<dyn Fn()>,
) {
    let create_shell = Rc::downgrade(shell);
    let create_members = members.clone();
    let create_dismiss = Rc::clone(&dismiss);
    let choose_shell = Rc::downgrade(shell);
    let choose_members = members.clone();
    let choose_dismiss = Rc::clone(&dismiss);
    let update = crate::playlists::picker::populate_context_picker(
        root,
        true,
        Rc::new(move |folder, _, _| {
            if let Some(shell) = choose_shell.upgrade() {
                change_members(&shell, choose_members.clone(), Ok(folder));
            }
            choose_dismiss();
        }),
        Rc::new(move |_| {
            let Some(shell) = create_shell.upgrade() else {
                return;
            };
            present_new_folder_dialog(&shell, create_members.clone(), Rc::clone(&create_dismiss));
        }),
    );
    update(shell.products.library.playlist_folder_choices("", &members));
}

/// A playlist folder's Rename folder and Delete folder, as `menu` offers them.
pub fn append_folder_row_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    menu: &rufin_core::menu::Menu,
    folder: &library::PlaylistFolderRow,
) {
    if menu.edit {
        let rename_shell = Rc::clone(shell);
        let rename_folder = folder.clone();
        surface.append_fixed_action(
            msgid("Rename folder"),
            crate::widgets::controls::EDIT_ICON,
            move || {
                let source = rename_shell.products.source.clone();
                let shell = Rc::downgrade(&rename_shell);
                let key = rename_folder.folder_key;
                let dialog = folder_name_dialog(Some(&rename_folder.name), move |name| {
                    let source = source.clone();
                    let shell = shell.clone();
                    gtk::glib::spawn_future_local(async move {
                        if let Err(error) =
                            rufin_core::playlists::rename_playlist_folder(&source, key, name).await
                            && let Some(shell) = shell.upgrade()
                        {
                            shell
                                .control_feedback
                                .show_feedback_toast(error.to_string());
                        }
                    });
                });
                rename_shell.present_selected_dialog(&dialog);
            },
        );
    }
    if !menu.delete {
        return;
    }
    let delete_shell = Rc::clone(shell);
    let folder = folder.clone();
    surface.append_fixed_action(
        msgid("Delete folder"),
        crate::widgets::controls::DELETE_ICON,
        move || {
            let source = delete_shell.products.source.clone();
            let shell = Rc::downgrade(&delete_shell);
            let key = folder.folder_key;
            gtk::glib::spawn_future_local(async move {
                let result = rufin_core::playlists::delete_playlist_folder(&source, key).await;
                let Some(shell) = shell.upgrade() else { return };
                match result {
                    Ok(()) => {
                        let open = match shell.navigation.routes.borrow().current() {
                            Route::PlaylistFolder { key: open, smart } if *open == key => {
                                Some(*smart)
                            }
                            _ => None,
                        };
                        if let Some(smart) = open {
                            shell.open_route(if smart {
                                Route::SmartPlaylists
                            } else {
                                Route::Playlists
                            });
                        }
                    }
                    Err(error) => shell
                        .control_feedback
                        .show_feedback_toast(error.to_string()),
                }
            });
        },
    );
}

pub fn playlist_drag_matches(
    source: &crate::widgets::media_drag::MediaDragSource,
    smart: bool,
) -> bool {
    use crate::widgets::media_drag::{CollectionTargets, MediaDragSource};
    let matches_target = |target: &crate::widgets::selection::PlayTarget| match target {
        crate::widgets::selection::PlayTarget::Route(Route::PlaylistDetail(_)) => !smart,
        crate::widgets::selection::PlayTarget::Route(Route::SmartPlaylistDetail(_)) => smart,
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
            library::ListKind::Playlists { .. } => !smart,
            library::ListKind::SmartPlaylists { .. } => smart,
            _ => false,
        }),
        _ => false,
    }
}

/// Whether a drag holds playlists or smart playlists, which a folder takes.
pub fn folder_takes_drag(source: &crate::widgets::media_drag::MediaDragSource) -> bool {
    playlist_drag_matches(source, false) || playlist_drag_matches(source, true)
}

/// Puts the playlists a drag holds in the folder `onto` names, or with a playlist `onto` names
/// in a new folder the person names. False when the drag holds no playlists.
pub fn drop_playlists(
    shell: &Rc<Shell>,
    onto: Result<library::PlaylistFolderKey, library::PlaylistFolderMember>,
    source: crate::widgets::media_drag::MediaDragSource,
) -> bool {
    use crate::widgets::media_drag::{CollectionTargets, MediaDragSource};
    if !folder_takes_drag(&source) {
        return false;
    }
    let targets = match source {
        MediaDragSource::Target { target, .. } => CollectionTargets::Ready(vec![target]),
        MediaDragSource::Targets { targets, .. } => targets,
        _ => return false,
    };
    let catalog = shell.products.library.clone();
    let task = shell
        .products
        .runtime
        .spawn(async move { targets.collections(&catalog).await });
    let source = shell.products.source.clone();
    let shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let result = async {
            let mut members: Vec<_> = task
                .await
                .map_err(|error| error.to_string())??
                .into_iter()
                .filter_map(|collection| match collection {
                    library::Collection::Playlist(key) => {
                        Some(library::PlaylistFolderMember::Playlist { key })
                    }
                    library::Collection::Smart { key, .. } => {
                        Some(library::PlaylistFolderMember::SmartPlaylist { key })
                    }
                    _ => None,
                })
                .collect();
            match onto {
                Ok(folder) => {
                    rufin_core::playlists::add_to_playlist_folder(&source, members, folder)
                        .await
                        .map_err(|error| error.to_string())
                }
                // A playlist dropped on itself makes no folder.
                Err(with) if !members.contains(&with) => {
                    members.push(with);
                    if let Some(shell) = shell.upgrade() {
                        present_new_folder_dialog(&shell, members, Rc::new(|| {}));
                    }
                    Ok(())
                }
                Err(_) => Ok(()),
            }
        }
        .await;
        if let Err(error) = result
            && let Some(shell) = shell.upgrade()
        {
            shell.control_feedback.show_feedback_toast(error);
        }
    });
    true
}
