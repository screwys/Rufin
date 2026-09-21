//! The overview uses the same cells as the library, with period-specific counts.
use crate::{
    CatalogUi,
    columns::{TrackRowPlayingIndicator, song_column_for_key},
    grid_cells::{AlbumGridCell, ArtistGridCell, ReusableCollectionGridCell},
};
use adw::prelude::*;
use gtk::{gio, glib};
use library::{ActivityOverview, HistoryRow};
use rufin_core::{
    playback::PlaybackTarget,
    settings::{LibraryField, LibraryListKey},
};
use std::{cell::RefCell, rc::Rc};
use ui_shared::{mounted_route::RouteCurrentTrack, route::Route};

pub struct ActivityCollections {
    pub tracks: gtk::ScrolledWindow,
    pub artists: gtk::FlowBox,
    pub albums: gtk::FlowBox,
    catalog: Rc<CatalogUi>,
    model: gio::ListStore,
    playing: TrackRowPlayingIndicator,
    artist_cells: RefCell<Vec<ArtistGridCell>>,
    album_cells: RefCell<Vec<AlbumGridCell>>,
    artist_uris: Rc<RefCell<Vec<String>>>,
    album_uris: Rc<RefCell<Vec<String>>>,
}

impl ActivityCollections {
    pub fn new(catalog: Rc<CatalogUi>) -> Self {
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::NoSelection::new(Some(model.clone()));
        let table = gtk::ColumnView::new(Some(selection));
        table.set_show_row_separators(true);
        table.set_single_click_activate(true);
        let playing = TrackRowPlayingIndicator::new();
        for field in [
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::PlayCount,
        ] {
            let column = song_column_for_key::<HistoryRow>(
                &catalog,
                LibraryListKey::History,
                field,
                &playing,
            );
            column.set_expand(field == LibraryField::TitleMerged);
            if field == LibraryField::TitleMerged {
                column.set_fixed_width(-1);
            }
            table.append_column(&column);
        }
        let weak = Rc::downgrade(&catalog);
        let rows = model.clone();
        table.connect_activate(move |_, position| {
            let Some(shell) = weak.upgrade() else {
                return;
            };
            if let Some(item) = rows.item(position).and_downcast::<glib::BoxedAnyObject>() {
                let row = item.borrow::<HistoryRow>();
                (shell.media_menus.play_target)(
                    &PlaybackTarget::Track(row.media_uri.clone()),
                    playback::QueuePlacement::Now,
                    false,
                );
            }
        });
        let tracks = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&table)
            .build();
        let grid = || {
            gtk::FlowBox::builder()
                .homogeneous(true)
                .min_children_per_line(1)
                .max_children_per_line(3)
                .selection_mode(gtk::SelectionMode::None)
                .activate_on_single_click(true)
                .hexpand(true)
                .valign(gtk::Align::Start)
                .build()
        };
        let artists = grid();
        let albums = grid();
        let artist_uris = Rc::new(RefCell::new(Vec::<String>::new()));
        let album_uris = Rc::new(RefCell::new(Vec::<String>::new()));
        for (grid, uris, artist) in [
            (&artists, &artist_uris, true),
            (&albums, &album_uris, false),
        ] {
            let weak = Rc::downgrade(&catalog);
            let uris = Rc::clone(uris);
            grid.connect_child_activated(move |_, child| {
                if let Some(catalog) = weak.upgrade()
                    && let Some(uri) = uris.borrow().get(child.index() as usize)
                {
                    catalog.navigate(if artist {
                        Route::ArtistDetail(uri.clone())
                    } else {
                        Route::AlbumDetail(uri.clone())
                    });
                }
            });
        }
        Self {
            tracks,
            artists,
            albums,
            catalog,
            model,
            playing,
            artist_cells: RefCell::new(Vec::new()),
            album_cells: RefCell::new(Vec::new()),
            artist_uris,
            album_uris,
        }
    }

    pub fn apply(&self, data: &ActivityOverview, limit: usize) {
        let tracks = data
            .tracks
            .iter()
            .take(limit)
            .cloned()
            .map(glib::BoxedAnyObject::new)
            .collect::<Vec<_>>();
        self.model.splice(0, self.model.n_items(), &tracks);
        self.artists.remove_all();
        self.albums.remove_all();
        self.artist_cells.borrow_mut().clear();
        self.album_cells.borrow_mut().clear();
        self.artist_uris.replace(
            data.artists
                .iter()
                .take(limit)
                .map(|row| row.media_uri.clone())
                .collect(),
        );
        self.album_uris.replace(
            data.albums
                .iter()
                .take(limit)
                .map(|row| row.media_uri.clone())
                .collect(),
        );
        for (position, row) in data.artists.iter().take(limit).enumerate() {
            let cell = ArtistGridCell::new(&self.catalog, &[LibraryField::PlayCount], false);
            cell.bind(position as u32, row.clone());
            cell.set_play_count_caption(row.play_count);
            let widget = crate::cards::collection_grid_card_inset(&cell.widget(), 120);
            self.artists.insert(&widget, -1);
            self.artist_cells.borrow_mut().push(cell);
        }
        for (position, row) in data.albums.iter().take(limit).enumerate() {
            let cell = AlbumGridCell::new(&self.catalog, &[LibraryField::PlayCount], None);
            cell.bind(position as u32, row.clone());
            cell.set_play_count_caption(row.play_count);
            let widget = crate::cards::collection_grid_card_inset(&cell.widget(), 120);
            self.albums.insert(&widget, -1);
            self.album_cells.borrow_mut().push(cell);
        }
    }

    pub fn refresh_playback(&self, current: Option<RouteCurrentTrack>) {
        self.playing.set_current(
            current.as_ref().map(|row| row.media_uri.as_str()),
            gtk::INVALID_LIST_POSITION,
        );
        self.playing
            .set_paused(current.as_ref().is_some_and(|row| row.paused));
        self.catalog
            .refresh_current_route_now_playing_selections(current);
    }
}
