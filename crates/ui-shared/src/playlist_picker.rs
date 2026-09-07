use crate::downloads::{OperationFeedback, OperationFeedbackKind};
use crate::interactions::popdown_native_menu;
use crate::library_fields::playlist_artwork;
use adw::prelude::*;
use downloads::DownloadSubject;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};
use localization::tr;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
crate::composite_box!(
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
    key: library::PlaylistKey,
    name: String,
    normalized_name: String,
}

#[derive(Clone)]
struct ReadyPlaylistTracks {
    media_uris: Rc<[String]>,
    subject: DownloadSubject,
}

pub fn populate_context_playlist_picker(
    root: &gtk::Box,
    source: rufin_core::runtime::SourceHandle,
    feedback: Rc<dyn Fn(&OperationFeedback)>,
    popover: &gtk::PopoverMenu,
) -> Rc<dyn Fn(Vec<library::PlaylistRow>, Vec<String>, DownloadSubject)> {
    let resource = crate::ui_resource::PLAYLIST_PICKER_CONTEXT_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        content: gtk::Box,
        search: gtk::SearchEntry,
        skip_duplicates: gtk::CheckButton,
        list: gtk::ListView,
        spinner: gtk::Spinner,
    });
    search.set_placeholder_text(Some(&tr("Search")));
    let skip_duplicates_label = tr("Don't duplicate");
    skip_duplicates.set_tooltip_text(Some(&skip_duplicates_label));
    skip_duplicates.update_property(&[gtk::accessible::Property::Label(&skip_duplicates_label)]);
    root.append(&content);

    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let query = Rc::new(RefCell::new(String::new()));
    let filter_query = Rc::clone(&query);
    let filter = gtk::CustomFilter::new(move |item| {
        let Some(item) = item.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        let choice = item.borrow::<PlaylistChoice>();
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
        let Some(label) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        else {
            return;
        };
        let Some(choice) = item
            .item()
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
        else {
            return;
        };
        label.set_label(&choice.borrow::<PlaylistChoice>().name);
    });
    list.set_model(Some(&selection));
    list.set_factory(Some(&factory));

    let search_filter = filter.clone();
    search.connect_search_changed(move |search| {
        *query.borrow_mut() = search.text().trim().to_lowercase();
        search_filter.changed(gtk::FilterChange::Different);
    });

    let ready = Rc::new(RefCell::new(None::<ReadyPlaylistTracks>));
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
        let subject = ready.subject.clone();
        let preview_uris = ready.media_uris.iter().take(4).cloned().collect::<Vec<_>>();
        let destination = choice.name;
        let feedback = Rc::clone(&feedback);
        crate::playlists::add_media_to_playlist(
            &source,
            choice.key,
            ready.media_uris.to_vec(),
            activate_skip.is_active(),
            Rc::new(move |accepted| {
                if accepted > 0 {
                    feedback(&OperationFeedback {
                        subject: subject.clone(),
                        preview_uris: preview_uris.clone(),
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
    Rc::new(move |playlists, tracks, subject| {
        if root.upgrade().is_none() {
            return;
        }
        let choices = playlists
            .into_iter()
            .map(|playlist| {
                glib::BoxedAnyObject::new(PlaylistChoice {
                    key: playlist.playlist_key,
                    normalized_name: playlist.name.to_lowercase(),
                    name: playlist.name,
                })
            })
            .collect::<Vec<_>>();
        store.splice(0, 0, &choices);
        ready.replace(Some(ReadyPlaylistTracks {
            media_uris: tracks.into(),
            subject,
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
}

pub fn playlist_picker_dialog(
    source: rufin_core::runtime::SourceHandle,
    artwork: Rc<crate::artwork::ArtworkState>,
    settings: Rc<crate::settings::SettingsState>,
    create_playlist: Rc<dyn Fn(String, Vec<String>)>,
    feedback: Rc<dyn Fn(&OperationFeedback)>,
    playlists: Vec<library::PlaylistRow>,
    media_uris: Vec<String>,
    subject: DownloadSubject,
) -> (adw::Dialog, Rc<dyn Fn(Vec<library::PlaylistRow>)>) {
    let resource = crate::ui_resource::PLAYLIST_PICKER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        dialog: adw::Dialog,
        search: gtk::SearchEntry,
        list: gtk::Box,
        create: gtk::Button,
        skip: gtk::CheckButton,
        cancel: gtk::Button,
        add: gtk::Button,
    });
    let rows = Rc::new(RefCell::new(Vec::<PickerRow>::new()));
    replace_picker_rows(&artwork, &settings, &list, &create, &rows, &add, playlists);

    let close = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });

    let filter_rows = Rc::clone(&rows);
    let filter_create = create.clone();
    let filter_add = add.clone();
    search.connect_search_changed(move |entry| {
        let text = entry.text();
        let query = text.trim().to_lowercase();
        filter_create.set_label(&format!("+ {} {}", tr("Create"), text.trim()));
        filter_create.set_visible(!query.is_empty());
        for row in filter_rows.borrow().iter() {
            row.row
                .set_visible(query.is_empty() || row.haystack.contains(&query));
        }
        filter_add.set_sensitive(filter_rows.borrow().iter().any(|row| row.check.is_active()));
    });
    let create_tracks = media_uris.clone();
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
            let feedback_subject = subject.clone();
            let preview_uris = media_uris.iter().take(4).cloned().collect::<Vec<_>>();
            let name = playlist.name.clone();
            crate::playlists::add_media_to_playlist(
                &source,
                playlist.playlist_key,
                media_uris.clone(),
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
                            subject: feedback_subject.clone(),
                            preview_uris: preview_uris.clone(),
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
            replace_picker_rows(&artwork, &settings, &list, &create, &rows, &add, playlists);
        }
    });
    (dialog, update)
}

fn replace_picker_rows(
    artwork: &Rc<crate::artwork::ArtworkState>,
    settings: &Rc<crate::settings::SettingsState>,
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
    let prefer_server = settings.current.borrow().prefer_server_playlist_covers;
    for playlist in playlists {
        let row = PlaylistPickerRowView::new();
        let check = row.imp().check.get();
        let bindings = playlist_artwork(&playlist, prefer_server);
        let cover = artwork
            .cover_group_projection_for_artwork(
                &bindings,
                48,
                crate::artwork::THUMB_COVER_SIZE as i32,
            )
            .widget();
        cover.add_css_class("context-playlist-cover");
        row.imp().cover_host.append(&cover);
        row.imp().title.set_label(&playlist.name);
        row.imp()
            .track_count
            .set_label(&localization::track_count_text(
                playlist.track_count.max(0) as u64
            ));
        row.imp().duration.set_label(&crate::format_duration_units(
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
                crate::format_duration_units((playlist.duration_millis.max(0) / 1_000) as u32)
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
