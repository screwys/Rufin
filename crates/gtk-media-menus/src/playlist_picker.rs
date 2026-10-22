use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};
use gtk_widgets::downloads::{OperationArtwork, OperationFeedback, OperationFeedbackKind};
use gtk_widgets::interactions::popdown_native_menu;
use gtk_widgets::library_fields::playlist_artwork;
use localization::tr;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
gtk_widgets::composite_box!(
    pub(crate) PlaylistPickerRowView,
    playlist_picker_row_view_imp,
    "RufinPlaylistPickerRowView",
    "/io/github/screwys/Rufin/ui/routes/playlist_picker_row.ui",
    {
        check: gtk::CheckButton,
        cover_host: gtk::Box,
        title: gtk::Label,
        track_count: gtk::Label,
        duration: gtk::Label,
        genres: gtk::Box,
    }
);

#[derive(Clone, Debug)]
struct PlaylistChoice {
    key: PlaylistChoiceKey,
    folder: Option<library::PlaylistFolderKey>,
    name: String,
    normalized_name: String,
}
#[derive(Clone, Debug)]
enum PlaylistChoiceKey {
    Playlist(library::PlaylistKey),
    Folder(library::PlaylistFolderKey),
}

#[derive(Clone)]
pub struct PlaylistPickerData {
    pub playlists: Vec<library::PlaylistRow>,
    pub folders: Vec<library::PlaylistFolderRow>,
    pub memberships: Vec<(library::PlaylistKey, library::PlaylistFolderKey)>,
}

#[derive(Clone)]
struct ReadyPlaylistTracks {
    selection: library::MemberSelection,
    title: String,
    preview_uris: Vec<String>,
}

pub fn populate_context_playlist_picker(
    root: &gtk::Box,
    source: rufin_core::runtime::SourceHandle,
    feedback: Rc<dyn Fn(&OperationFeedback)>,
    popover: &gtk::PopoverMenu,
) -> Rc<dyn Fn(PlaylistPickerData, library::MemberSelection, String, Vec<String>)> {
    let resource = crate::ui_resource::PLAYLIST_PICKER_CONTEXT_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        content: gtk::Box,
        search: gtk::SearchEntry,
        skip_duplicates: gtk::CheckButton,
        list: gtk::ListView,
        spinner: gtk::Spinner,
        back: gtk::Button,
    });
    gtk_widgets::controls::configure_search_entry(&search);
    search.set_placeholder_text(Some(&tr("Search")));
    let skip_duplicates_label = tr("Don't duplicate");
    skip_duplicates.set_tooltip_text(Some(&skip_duplicates_label));
    skip_duplicates.update_property(&[gtk::accessible::Property::Label(&skip_duplicates_label)]);
    root.append(&content);

    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let folder = Rc::new(Cell::new(None::<library::PlaylistFolderKey>));
    let filter_folder = Rc::clone(&folder);
    let query = Rc::new(RefCell::new(String::new()));
    let filter_query = Rc::clone(&query);
    let filter = gtk::CustomFilter::new(move |item| {
        let Some(item) = item.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        let choice = item.borrow::<PlaylistChoice>();
        let query = filter_query.borrow();
        let current = filter_folder.get();
        let location_matches = match choice.key {
            PlaylistChoiceKey::Folder(_) => current.is_none(),
            PlaylistChoiceKey::Playlist(_) => {
                (!query.is_empty() && current.is_none()) || choice.folder == current
            }
        };
        location_matches && (query.is_empty() || choice.normalized_name.contains(query.as_str()))
    });
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::NoSelection::new(Some(filtered.clone()));
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        item.set_child(Some(&glib::Object::new::<
            gtk_widgets::recycled_cells::RecycledBadgedTextCell,
        >()));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = item
            .child()
            .and_downcast::<gtk_widgets::recycled_cells::RecycledBadgedTextCell>()
        else {
            return;
        };
        let Some(choice) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let choice = choice.borrow::<PlaylistChoice>();
        cell.label().set_label(&choice.name);
        cell.set_folder(matches!(choice.key, PlaylistChoiceKey::Folder(_)));
    });
    list.set_model(Some(&selection));
    list.set_factory(Some(&factory));

    let search_filter = filter.clone();
    search.connect_search_changed(move |search| {
        *query.borrow_mut() = search.text().trim().to_lowercase();
        search_filter.changed(gtk::FilterChange::Different);
    });

    let ready = Rc::new(RefCell::new(None::<ReadyPlaylistTracks>));
    let activate_folder = Rc::clone(&folder);
    let activate_filter = filter.clone();
    let activate_search = search.clone();
    let activate_back = back.downgrade();
    let back_folder = Rc::clone(&folder);
    let back_filter = filter.clone();
    let back_search = search.clone();
    back.connect_clicked(move |back| {
        back_folder.set(None);
        back_search.set_text("");
        back.set_visible(false);
        back_filter.changed(gtk::FilterChange::Different);
    });
    let activate_ready = Rc::clone(&ready);
    let activate_model = filtered.clone();
    let activate_skip = skip_duplicates.clone();
    let activate_popover = popover.downgrade();
    list.connect_activate(move |_, position| {
        let Some(ready) = activate_ready.borrow().clone() else {
            return;
        };
        let Some(choice) = activate_model
            .item(position)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .map(|item| item.borrow::<PlaylistChoice>().clone())
        else {
            return;
        };
        let key = match choice.key {
            PlaylistChoiceKey::Playlist(key) => key,
            PlaylistChoiceKey::Folder(key) => {
                activate_folder.set(Some(key));
                activate_search.set_text("");
                if let Some(back) = activate_back.upgrade() {
                    back.set_visible(true);
                }
                activate_filter.changed(gtk::FilterChange::Different);
                return;
            }
        };
        let title = ready.title.clone();
        let preview_uris = ready.preview_uris.clone();
        let destination = choice.name;
        let feedback = Rc::clone(&feedback);
        gtk_widgets::playlists::add_media_to_playlist(
            &source,
            key,
            ready.selection,
            activate_skip.is_active(),
            Rc::new(move |accepted| {
                if accepted > 0 {
                    feedback(&OperationFeedback {
                        title: title.clone(),
                        artwork: OperationArtwork::MediaUris(preview_uris.clone()),
                        item_count: accepted,
                        kind: OperationFeedbackKind::PlaylistAdded {
                            destination: destination.clone(),
                        },
                    });
                }
            }),
        );
        if let Some(popover) = activate_popover.upgrade() {
            popdown_native_menu(&popover);
        }
    });

    let root = root.downgrade();
    let spinner = spinner.downgrade();
    Rc::new(move |data, selection, title, preview_uris| {
        if root.upgrade().is_none() {
            return;
        }
        let memberships = data
            .memberships
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        let mut choices = data
            .playlists
            .into_iter()
            .map(|playlist| {
                glib::BoxedAnyObject::new(PlaylistChoice {
                    key: PlaylistChoiceKey::Playlist(playlist.playlist_key),
                    folder: memberships.get(&playlist.playlist_key).copied(),
                    normalized_name: playlist.name.to_lowercase(),
                    name: playlist.name,
                })
            })
            .collect::<Vec<_>>();
        choices.extend(data.folders.into_iter().map(|folder| {
            glib::BoxedAnyObject::new(PlaylistChoice {
                key: PlaylistChoiceKey::Folder(folder.folder_key),
                folder: None,
                normalized_name: folder.name.to_lowercase(),
                name: folder.name,
            })
        }));
        store.splice(0, store.n_items(), &choices);
        ready.replace(Some(ReadyPlaylistTracks {
            selection,
            title,
            preview_uris,
        }));
        if let Some(spinner) = spinner.upgrade() {
            spinner.stop();
            spinner.set_visible(false);
        }
    })
}

#[derive(Clone)]
struct PickerRow {
    row: gtk::Widget,
    check: gtk::CheckButton,
    playlist: library::PlaylistRow,
    haystack: String,
    folder: Option<library::PlaylistFolderKey>,
}
#[derive(Clone)]
struct PickerFolderRow {
    row: gtk::Widget,
    name: String,
}

fn filter_picker(
    rows: &[PickerRow],
    folders: &[PickerFolderRow],
    current: Option<library::PlaylistFolderKey>,
    query: &str,
) {
    for row in rows {
        row.row.set_visible(
            ((!query.is_empty() && current.is_none()) || row.folder == current)
                && (query.is_empty() || row.haystack.contains(query)),
        );
    }
    for row in folders {
        row.row
            .set_visible(current.is_none() && (query.is_empty() || row.name.contains(query)));
    }
}

#[allow(clippy::too_many_arguments)]
pub fn playlist_picker_dialog(
    source: rufin_core::runtime::SourceHandle,
    artwork: Rc<gtk_widgets::artwork::ArtworkState>,
    create_playlist: Rc<
        dyn Fn(String, library::MemberSelection, Option<library::PlaylistFolderKey>),
    >,
    feedback: Rc<dyn Fn(&OperationFeedback)>,
    data: PlaylistPickerData,
    selection: library::MemberSelection,
    title: String,
    preview_uris: Vec<String>,
) -> (adw::Dialog, Rc<dyn Fn(PlaylistPickerData)>) {
    let resource = crate::ui_resource::PLAYLIST_PICKER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        dialog: adw::Dialog,
        search: gtk::SearchEntry,
        list: gtk::Box,
        create: gtk::Button,
        skip: gtk::CheckButton,
        cancel: gtk::Button,
        add: gtk::Button,
        back: gtk::Button,
    });
    gtk_widgets::controls::configure_search_entry(&search);
    let rows = Rc::new(RefCell::new(Vec::<PickerRow>::new()));
    let folder_rows = Rc::new(RefCell::new(Vec::<PickerFolderRow>::new()));
    let folder = Rc::new(Cell::new(None::<library::PlaylistFolderKey>));
    let change_folder: Rc<dyn Fn(Option<library::PlaylistFolderKey>)> = {
        let folder = Rc::clone(&folder);
        let rows = Rc::clone(&rows);
        let folder_rows = Rc::clone(&folder_rows);
        let search = search.downgrade();
        let back = back.downgrade();
        Rc::new(move |next| {
            folder.set(next);
            if let Some(search) = search.upgrade() {
                search.set_text("");
            }
            if let Some(back) = back.upgrade() {
                back.set_visible(next.is_some());
            }
            filter_picker(&rows.borrow(), &folder_rows.borrow(), next, "");
        })
    };
    replace_picker_rows(
        &artwork,
        &list,
        &create,
        &rows,
        &folder_rows,
        &add,
        data,
        Rc::clone(&change_folder),
    );
    change_folder(None);
    let go_back = Rc::clone(&change_folder);
    back.connect_clicked(move |_| go_back(None));

    let close = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });

    let filter_rows = Rc::clone(&rows);
    let filter_folder_rows = Rc::clone(&folder_rows);
    let filter_folder = Rc::clone(&folder);
    let filter_create = create.clone();
    let filter_add = add.clone();
    search.connect_search_changed(move |entry| {
        let text = entry.text();
        let query = text.trim().to_lowercase();
        filter_create.set_label(&format!("+ {} {}", tr("Create"), text.trim()));
        filter_create.set_visible(!query.is_empty());
        filter_picker(
            &filter_rows.borrow(),
            &filter_folder_rows.borrow(),
            filter_folder.get(),
            &query,
        );
        filter_add.set_sensitive(filter_rows.borrow().iter().any(|row| row.check.is_active()));
    });
    let create_tracks = selection.clone();
    let create_search = search.clone();
    let create_folder = Rc::clone(&folder);
    create.connect_clicked(move |_| {
        let name = create_search.text().trim().to_string();
        if !name.is_empty() {
            create_playlist(name, create_tracks.clone(), create_folder.get());
            create_search.set_text("");
        }
    });
    let add_rows = Rc::clone(&rows);
    let add_dialog = dialog.downgrade();
    add.connect_clicked(move |_| {
        let destinations = add_rows
            .borrow()
            .iter()
            .filter(|row| row.check.is_active())
            .map(|row| row.playlist.clone())
            .collect::<Vec<_>>();
        let pending = Rc::new(Cell::new(destinations.len()));
        let accepted = Rc::new(Cell::new(0));
        let accepted_names = Rc::new(RefCell::new(Vec::new()));
        for playlist in destinations {
            let pending = Rc::clone(&pending);
            let accepted = Rc::clone(&accepted);
            let accepted_names = Rc::clone(&accepted_names);
            let feedback = Rc::clone(&feedback);
            let feedback_title = title.clone();
            let preview_uris = preview_uris.clone();
            let name = playlist.name.clone();
            gtk_widgets::playlists::add_media_to_playlist(
                &source,
                playlist.playlist_key,
                selection.clone(),
                skip.is_active(),
                Rc::new(move |count| {
                    accepted.set(accepted.get() + count);
                    if count > 0 {
                        accepted_names.borrow_mut().push(name.clone());
                    }
                    pending.set(pending.get().saturating_sub(1));
                    if pending.get() == 0 && accepted.get() > 0 {
                        let names = accepted_names.borrow();
                        let destination = match names.as_slice() {
                            [name] => name.clone(),
                            _ => format!("{} {}", names.len(), tr("Playlists")),
                        };
                        feedback(&OperationFeedback {
                            title: feedback_title.clone(),
                            artwork: OperationArtwork::MediaUris(preview_uris.clone()),
                            item_count: accepted.get(),
                            kind: OperationFeedbackKind::PlaylistAdded { destination },
                        });
                    }
                }),
            );
        }
        if let Some(dialog) = add_dialog.upgrade() {
            dialog.close();
        }
    });
    let refresh_dialog = dialog.downgrade();
    let update = Rc::new(move |data| {
        if refresh_dialog.upgrade().is_some() {
            replace_picker_rows(
                &artwork,
                &list,
                &create,
                &rows,
                &folder_rows,
                &add,
                data,
                Rc::clone(&change_folder),
            );
            change_folder(folder.get());
        }
    });
    (dialog, update)
}

#[allow(clippy::too_many_arguments)]
fn replace_picker_rows(
    artwork: &Rc<gtk_widgets::artwork::ArtworkState>,
    list: &gtk::Box,
    create: &gtk::Button,
    rows: &Rc<RefCell<Vec<PickerRow>>>,
    folder_rows: &Rc<RefCell<Vec<PickerFolderRow>>>,
    add: &gtk::Button,
    data: PlaylistPickerData,
    change_folder: Rc<dyn Fn(Option<library::PlaylistFolderKey>)>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    list.append(create);
    rows.borrow_mut().clear();
    folder_rows.borrow_mut().clear();
    let memberships = data
        .memberships
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
    for folder in data.folders {
        let resource = crate::ui_resource::PLAYLIST_PICKER_FOLDER_ROW_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, { folder_row: gtk::Button, cover_host: gtk::Box, title: gtk::Label, track_count: gtk::Label, duration: gtk::Label });
        let bindings = folder
            .artwork_binding
            .iter()
            .map(|binding| gtk_widgets::library_fields::opaque_artwork(Some(binding.as_slice())))
            .collect::<Vec<_>>();
        cover_host.append(
            &artwork
                .cover_group_projection_for_artwork(&bindings, 48, 48, false)
                .widget(),
        );
        title.set_label(&folder.name);
        track_count.set_label(&localization::track_count_text(
            folder.track_count.max(0) as u64
        ));
        duration.set_label(&gtk_widgets::format_duration_units(
            (folder.duration_millis.max(0) / 1_000) as u32,
        ));
        let change = Rc::downgrade(&change_folder);
        folder_row.connect_clicked(move |_| {
            if let Some(change) = change.upgrade() {
                change(Some(folder.folder_key));
            }
        });
        list.append(&folder_row);
        folder_rows.borrow_mut().push(PickerFolderRow {
            row: folder_row.upcast(),
            name: folder.name.to_lowercase(),
        });
    }
    for playlist in data.playlists {
        let row = PlaylistPickerRowView::new();
        let check = row.imp().check.get();
        let bindings = playlist_artwork(&playlist);
        let cover = artwork
            .cover_group_projection_for_artwork(&bindings, 48, 48, false)
            .widget();
        cover.add_css_class("context-playlist-cover");
        row.imp().cover_host.append(&cover);
        row.imp().title.set_label(&playlist.name);
        row.imp()
            .track_count
            .set_label(&localization::track_count_text(
                playlist.track_count.max(0) as u64
            ));
        row.imp()
            .duration
            .set_label(&gtk_widgets::format_duration_units(
                (playlist.duration_millis.max(0) / 1_000) as u32,
            ));
        for genre in playlist.genres.iter().take(2) {
            let pill = gtk::Label::new(Some(&genre.name));
            pill.add_css_class("album-detail-genre-pill");
            row.imp().genres.append(&pill);
        }
        row.imp()
            .genres
            .set_visible(row.imp().genres.first_child().is_some());
        let widget = row.upcast::<gtk::Widget>();
        list.append(&widget);
        rows.borrow_mut().push(PickerRow {
            haystack: format!(
                "{} {} {}",
                playlist.name,
                playlist.track_count,
                gtk_widgets::format_duration_units(
                    (playlist.duration_millis.max(0) / 1_000) as u32
                )
            )
            .to_lowercase(),
            folder: memberships.get(&playlist.playlist_key).copied(),
            playlist,
            row: widget,
            check: check.clone(),
        });
        let rows_for_check = Rc::downgrade(rows);
        let add_for_check = add.downgrade();
        check.connect_toggled(move |_| {
            if let (Some(rows), Some(add)) = (rows_for_check.upgrade(), add_for_check.upgrade()) {
                add.set_sensitive(rows.borrow().iter().any(|row| row.check.is_active()));
            }
        });
    }
}
