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
struct PlaylistChoice<Key> {
    key: Key,
    name: String,
    normalized_name: String,
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
    new_playlist: Rc<dyn Fn(String, library::MemberSelection)>,
) -> Rc<dyn Fn(Vec<library::PlaylistRow>, library::MemberSelection, String, Vec<String>)> {
    let ready = Rc::new(RefCell::new(None::<ReadyPlaylistTracks>));
    let create_ready = Rc::clone(&ready);
    let create_popover = popover.downgrade();
    let activate_ready = Rc::clone(&ready);
    let activate_popover = popover.downgrade();
    let update = populate_context_picker(
        root,
        false,
        Rc::new(move |key, destination, skip_duplicates| {
            let Some(ready) = activate_ready.borrow().clone() else {
                return;
            };
            let feedback = Rc::clone(&feedback);
            gtk_widgets::playlists::add_media_to_playlist(
                &source,
                key,
                ready.selection,
                skip_duplicates,
                Rc::new(move |accepted| {
                    if accepted > 0 {
                        feedback(&OperationFeedback {
                            title: ready.title.clone(),
                            artwork: OperationArtwork::MediaUris(ready.preview_uris.clone()),
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
        }),
        Rc::new(move |name| {
            let selection = create_ready
                .borrow()
                .as_ref()
                .map(|ready| ready.selection.clone());
            let Some(selection) = selection else { return };
            if let Some(popover) = create_popover.upgrade() {
                popdown_native_menu(&popover);
            }
            new_playlist(name, selection);
        }),
    );
    Rc::new(move |playlists, selection, title, preview_uris| {
        ready.replace(Some(ReadyPlaylistTracks {
            selection,
            title,
            preview_uris,
        }));
        update(
            playlists
                .into_iter()
                .map(|row| (row.playlist_key, row.name))
                .collect(),
        );
    })
}

pub(crate) fn populate_context_picker<Key: Clone + 'static>(
    root: &gtk::Box,
    folders: bool,
    activate: Rc<dyn Fn(Key, String, bool)>,
    create: Rc<dyn Fn(String)>,
) -> Rc<dyn Fn(Vec<(Key, String)>)> {
    let resource = crate::ui_resource::PLAYLIST_PICKER_CONTEXT_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        content: gtk::Box,
        search: gtk::SearchEntry,
        skip_duplicates: gtk::CheckButton,
        list: gtk::ListView,
        spinner: gtk::Spinner,
        create_playlist: gtk::Button,
        create_label: gtk::Label,
        folder_labels: gtk::StringList,
    });
    gtk_widgets::controls::configure_search_entry(&search);
    skip_duplicates.set_visible(!folders);
    if folders {
        create_label.set_label(folder_labels.string(0).as_deref().unwrap());
    }
    root.append(&content);

    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let query = Rc::new(RefCell::new(String::new()));
    let filter_query = Rc::clone(&query);
    let filter = gtk::CustomFilter::new(move |item| {
        let Some(item) = item.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        let choice = item.borrow::<PlaylistChoice<Key>>();
        let query = filter_query.borrow();
        query.is_empty() || choice.normalized_name.contains(query.as_str())
    });
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::NoSelection::new(Some(filtered.clone()));
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_margin_top(2);
        label.set_margin_bottom(2);
        label.set_margin_start(4);
        label.set_margin_end(4);
        item.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let Some(choice) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        label.set_label(&choice.borrow::<PlaylistChoice<Key>>().name);
    });
    list.set_model(Some(&selection));
    list.set_factory(Some(&factory));

    let search_filter = filter.clone();
    search.connect_search_changed(move |search| {
        *query.borrow_mut() = search.text().trim().to_lowercase();
        search_filter.changed(gtk::FilterChange::Different);
    });

    create_playlist.connect_clicked(move |_| create(search.text().trim().to_owned()));
    let activate_model = filtered.clone();
    list.connect_activate(move |_, position| {
        if let Some(choice) = activate_model
            .item(position)
            .and_downcast::<glib::BoxedAnyObject>()
        {
            let choice = choice.borrow::<PlaylistChoice<Key>>();
            activate(
                choice.key.clone(),
                choice.name.clone(),
                skip_duplicates.is_active(),
            );
        }
    });
    let root = root.downgrade();
    let create_playlist = create_playlist.downgrade();
    let spinner = spinner.downgrade();
    Rc::new(move |choices| {
        if root.upgrade().is_none() {
            return;
        }
        let choices = choices
            .into_iter()
            .map(|(key, name)| {
                glib::BoxedAnyObject::new(PlaylistChoice {
                    key,
                    normalized_name: name.to_lowercase(),
                    name,
                })
            })
            .collect::<Vec<_>>();
        store.splice(0, store.n_items(), &choices);
        if let Some(create_playlist) = create_playlist.upgrade() {
            create_playlist.set_sensitive(true);
        }
        if let Some(spinner) = spinner.upgrade() {
            spinner.stop();
            spinner.set_visible(false);
        }
    })
}

pub(crate) fn folder_picker_dialog() -> (adw::Dialog, gtk::Box) {
    let resource = crate::ui_resource::PLAYLIST_PICKER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        dialog: adw::Dialog, title: adw::WindowTitle, search: gtk::SearchEntry,
        list: gtk::Box, skip: gtk::CheckButton, cancel: gtk::Button, add: gtk::Button,
        folder_labels: gtk::StringList,
    });
    let label = folder_labels.string(0).unwrap();
    dialog.set_title(&label);
    title.set_title(&label);
    search.set_visible(false);
    skip.set_visible(false);
    add.set_visible(false);
    let close = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });
    (dialog, list)
}

#[derive(Clone)]
struct PickerRow {
    row: gtk::Widget,
    check: gtk::CheckButton,
    playlist: library::PlaylistRow,
    haystack: String,
}

#[allow(clippy::too_many_arguments)]
pub fn playlist_picker_dialog(
    source: rufin_core::runtime::SourceHandle,
    artwork: Rc<gtk_widgets::artwork::ArtworkState>,
    create_playlist: Rc<dyn Fn(String, library::MemberSelection)>,
    feedback: Rc<dyn Fn(&OperationFeedback)>,
    playlists: Vec<library::PlaylistRow>,
    selection: library::MemberSelection,
    title: String,
    preview_uris: Vec<String>,
) -> (adw::Dialog, Rc<dyn Fn(Vec<library::PlaylistRow>)>) {
    let resource = crate::ui_resource::PLAYLIST_PICKER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        dialog: adw::Dialog,
        search: gtk::SearchEntry,
        list: gtk::Box,
        create: gtk::Button,
        create_label: gtk::Label,
        skip: gtk::CheckButton,
        cancel: gtk::Button,
        add: gtk::Button,
    });
    gtk_widgets::controls::configure_search_entry(&search);
    let rows = Rc::new(RefCell::new(Vec::<PickerRow>::new()));
    replace_picker_rows(&artwork, &list, &create, &rows, &add, playlists);

    let close = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });

    let filter_rows = Rc::clone(&rows);
    let filter_create = create.clone();
    let filter_create_label = create_label.clone();
    let filter_add = add.clone();
    search.connect_search_changed(move |entry| {
        let text = entry.text();
        let query = text.trim().to_lowercase();
        filter_create_label.set_label(&format!("{} {}", tr("Create"), text.trim()));
        filter_create.set_visible(!query.is_empty());
        for row in filter_rows.borrow().iter() {
            row.row
                .set_visible(query.is_empty() || row.haystack.contains(&query));
        }
        filter_add.set_sensitive(filter_rows.borrow().iter().any(|row| row.check.is_active()));
    });
    let create_tracks = selection.clone();
    let create_search = search.clone();
    create.connect_clicked(move |_| {
        let name = create_search.text().trim().to_string();
        if !name.is_empty() {
            create_playlist(name, create_tracks.clone());
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
    let update = Rc::new(move |playlists| {
        if refresh_dialog.upgrade().is_some() {
            replace_picker_rows(&artwork, &list, &create, &rows, &add, playlists);
        }
    });
    (dialog, update)
}

fn replace_picker_rows(
    artwork: &Rc<gtk_widgets::artwork::ArtworkState>,
    list: &gtk::Box,
    create: &gtk::Button,
    rows: &Rc<RefCell<Vec<PickerRow>>>,
    add: &gtk::Button,
    playlists: Vec<library::PlaylistRow>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    list.append(create);
    rows.borrow_mut().clear();
    for playlist in playlists {
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
