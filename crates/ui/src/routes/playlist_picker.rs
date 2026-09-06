use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use artwork::ArtworkBinding;
use downloads::DownloadSubject;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};
use localization::{msgid, tr};

use crate::downloads::{OperationFeedback, OperationFeedbackKind};
use crate::interactions::{ADD_TO_PLAYLIST_ICON, ContextMenuSurface, popdown_native_menu};
use crate::settings::ContextMenuItem;
use crate::shell::Shell;
use crate::shell::cover::{ArtworkTile, THUMB_COVER_SIZE};

use super::collections::PlaybackTarget;
use super::library_fields::playlist_artwork;
use super::track_selection::{
    PlaylistEntrySelectionSnapshot, TrackSelection, TrackSelectionSnapshot,
};

const PLAYLIST_DRAG_ICON_COVER_SIZE: i32 = 36;
const PLAYLIST_DRAG_ICON_WIDTH: i32 = 180;

crate::ui_resource::composite_box!(
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

#[derive(Clone, Default)]
pub(crate) struct MediaDragPreviewBinding {
    prepared: Rc<RefCell<Option<PlaylistDragPreview>>>,
    active_tile: Rc<RefCell<Option<ArtworkTile>>>,
}

#[derive(Clone)]
struct PlaylistDragPreview {
    title: String,
    artwork: PlaylistDragPreviewArtwork,
}

#[derive(Clone)]
enum PlaylistDragPreviewArtwork {
    Resident(Option<gtk::gdk::Paintable>),
    CacheOnly {
        shell: std::rc::Weak<Shell>,
        binding: ArtworkBinding,
    },
}

impl MediaDragPreviewBinding {
    pub(crate) fn connect(&self, source: &gtk::DragSource) {
        let prepared = Rc::clone(&self.prepared);
        let active_tile = Rc::clone(&self.active_tile);
        source.connect_drag_begin(move |_, drag| {
            let Some(preview) = prepared.borrow().clone() else {
                return;
            };
            active_tile.borrow_mut().take();
            let cover = match preview.artwork {
                PlaylistDragPreviewArtwork::Resident(artwork) => artwork.map(|artwork| {
                    let cover = gtk::Picture::for_paintable(&artwork);
                    cover.set_can_shrink(true);
                    cover.set_content_fit(gtk::ContentFit::Cover);
                    cover.upcast()
                }),
                PlaylistDragPreviewArtwork::CacheOnly { shell, binding } => {
                    shell.upgrade().map(|shell| {
                        let tile = ArtworkTile::new(PLAYLIST_DRAG_ICON_COVER_SIZE);
                        shell.bind_cache_only_artwork_tile(
                            &tile,
                            binding,
                            PLAYLIST_DRAG_ICON_COVER_SIZE,
                            THUMB_COVER_SIZE,
                        );
                        let widget = tile.widget();
                        active_tile.replace(Some(tile));
                        widget
                    })
                }
            };
            let icon = compact_playlist_drag_icon(cover, &preview.title);
            gtk::DragIcon::for_drag(drag).set_child(Some(&icon));
            drag.set_hotspot(
                PLAYLIST_DRAG_ICON_COVER_SIZE / 2,
                PLAYLIST_DRAG_ICON_COVER_SIZE / 2,
            );
        });
        let finished = self.clone();
        source.connect_drag_end(move |_, _, _| finished.clear());
    }

    pub(crate) fn prepare(&self, title: String, artwork: Option<gtk::gdk::Paintable>) {
        self.prepared.replace(Some(PlaylistDragPreview {
            title,
            artwork: PlaylistDragPreviewArtwork::Resident(artwork),
        }));
    }

    pub(crate) fn prepare_cache_only(
        &self,
        shell: &Rc<Shell>,
        title: String,
        binding: ArtworkBinding,
    ) {
        self.prepared.replace(Some(PlaylistDragPreview {
            title,
            artwork: PlaylistDragPreviewArtwork::CacheOnly {
                shell: Rc::downgrade(shell),
                binding,
            },
        }));
    }

    pub(crate) fn clear(&self) {
        self.prepared.borrow_mut().take();
        self.active_tile.borrow_mut().take();
    }
}

#[derive(Clone)]
pub(crate) enum MediaDragSource {
    LiveCollection {
        media_uri: String,
        operations: crate::runtime::source::SourceHandle,
    },
    Target {
        source_key: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
        target: PlaybackTarget,
    },
    Targets {
        source_key: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
        targets: Vec<PlaybackTarget>,
    },
    Selection(TrackSelectionSnapshot),
    PlaylistEntries(PlaylistEntrySelectionSnapshot),
    MediaUris(std::sync::Arc<[String]>),
    Queue {
        occurrences: std::sync::Arc<[playback::OccurrenceId]>,
        media_uris: std::sync::Arc<[String]>,
    },
}

impl MediaDragSource {
    pub(crate) fn capture_target(shell: &Shell, target: PlaybackTarget) -> Self {
        let selected = shell.selected_library();
        Self::Target {
            source_key: selected.as_ref().map(|selected| selected.source_key),
            folder: selected
                .as_ref()
                .and_then(|selected| selected.music_folder_key),
            target,
        }
    }

    pub(crate) fn selection(selection: TrackSelectionSnapshot) -> Self {
        Self::Selection(selection)
    }

    pub(crate) fn track(media_uri: String) -> Self {
        Self::media_uris([media_uri])
    }

    pub(crate) fn media_uris(media_uris: impl IntoIterator<Item = String>) -> Self {
        Self::MediaUris(media_uris.into_iter().collect::<Vec<_>>().into())
    }

    pub(crate) fn playlist_entries(selection: PlaylistEntrySelectionSnapshot) -> Self {
        Self::PlaylistEntries(selection)
    }
}

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

pub(crate) fn refresh_context_playlist_picker(shell: &Rc<Shell>) {
    shell.refresh_playlist_picker();
}

pub(crate) fn media_drag_content_provider(source: MediaDragSource) -> gtk::gdk::ContentProvider {
    let payload = glib::BoxedAnyObject::new(source);
    gtk::gdk::ContentProvider::for_value(&payload.to_value())
}

pub(crate) fn media_drag_source(value: &glib::Value) -> Option<MediaDragSource> {
    let payload = value.get::<glib::BoxedAnyObject>().ok()?;
    Some(payload.try_borrow::<MediaDragSource>().ok()?.clone())
}

pub(crate) fn install_compact_media_drag_source(
    target: &impl IsA<gtk::Widget>,
    artwork: &impl IsA<gtk::Widget>,
    current: impl Fn() -> Option<(MediaDragSource, String)> + 'static,
) {
    let preview = MediaDragPreviewBinding::default();
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::COPY)
        .build();
    source.set_propagation_phase(gtk::PropagationPhase::Capture);
    let prepare_preview = preview.clone();
    let weak_artwork = artwork.as_ref().downgrade();
    let drag_widget = target.as_ref().downgrade();
    source.connect_prepare(move |_, _, _| {
        prepare_preview.clear();
        let (source, title) = current()?;
        let artwork = weak_artwork.upgrade().and_then(|artwork| {
            match artwork.downcast::<gtk::Picture>() {
                Ok(picture) => picture.paintable(),
                // Freeze the displayed collage without retaining the recycled cell.
                Err(widget) => Some(gtk::WidgetPaintable::new(Some(&widget)).current_image()),
            }
        });
        prepare_preview.prepare(title, artwork);
        Some(media_drag_content_provider(capture_collection_selection(
            source,
            &drag_widget,
        )))
    });
    preview.connect(&source);
    target.as_ref().add_controller(source);
}

pub(crate) fn install_media_drag_source(
    target: &impl IsA<gtk::Widget>,
    current: impl Fn() -> Option<(MediaDragSource, String)> + 'static,
) {
    let preview = MediaDragPreviewBinding::default();
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::COPY)
        .build();
    source.set_propagation_phase(gtk::PropagationPhase::Capture);
    let prepare_preview = preview.clone();
    let drag_widget = target.as_ref().downgrade();
    source.connect_prepare(move |_, _, _| {
        prepare_preview.clear();
        let (source, title) = current()?;
        prepare_preview.prepare(title, None);
        Some(media_drag_content_provider(capture_collection_selection(
            source,
            &drag_widget,
        )))
    });
    preview.connect(&source);
    target.as_ref().add_controller(source);
}

fn capture_collection_selection(
    source: MediaDragSource,
    widget: &glib::WeakRef<gtk::Widget>,
) -> MediaDragSource {
    let MediaDragSource::Target {
        source_key,
        folder,
        target,
    } = &source
    else {
        return source;
    };
    let Some(widget) = widget.upgrade() else {
        return source;
    };
    let selection = widget
        .ancestor(gtk::GridView::static_type())
        .and_downcast::<gtk::GridView>()
        .and_then(|grid| grid.model())
        .or_else(|| {
            widget
                .ancestor(gtk::ColumnView::static_type())
                .and_downcast::<gtk::ColumnView>()
                .and_then(|table| table.model())
        });
    let Some(selection) = selection else {
        return source;
    };
    let positions = selection.selection();
    if positions.size() < 2 {
        return source;
    }
    let Some((iter, first)) = gtk::BitsetIter::init_first(&positions) else {
        return source;
    };
    let target = match target {
        PlaybackTarget::Contextual { target, .. } => target.as_ref(),
        target => target,
    };
    let mut contains_dragged = false;
    let targets = std::iter::once(first)
        .chain(iter)
        .map(|position| {
            let object = selection.item(position)?;
            let (target, dragged) = selected_collection_target(&object, target)?;
            contains_dragged |= dragged;
            Some(target)
        })
        .collect::<Option<Vec<_>>>();
    match targets {
        Some(targets) if contains_dragged => MediaDragSource::Targets {
            source_key: *source_key,
            folder: *folder,
            targets,
        },
        _ => source,
    }
}

fn selected_collection_target(
    object: &glib::Object,
    dragged: &PlaybackTarget,
) -> Option<(PlaybackTarget, bool)> {
    use super::sparse_model::{SparseItem, SparseObjectItem};
    // Sparse placeholders already carry collection keys. A large selection needs no row hydration.
    macro_rules! selected {
        ($row:ty, $key:ty, $field:ident, $target:expr, $matches:expr) => {{
            if let Some(row) = super::library_fields::object_item::<$row>(object.clone()) {
                Some(($target(row.$field), $matches(&row)))
            } else if let Some(SparseItem::Placeholder(key)) = object
                .downcast_ref::<SparseObjectItem>()?
                .value::<SparseItem<$key, $row>>()
            {
                Some(($target(key), false))
            } else {
                None
            }
        }};
    }
    match dragged {
        PlaybackTarget::Album(uri) => selected!(
            library::AlbumRow,
            library::AlbumKey,
            album_key,
            PlaybackTarget::AlbumKey,
            |row: &library::AlbumRow| &row.media_uri == uri
        ),
        PlaybackTarget::Artist(uri) | PlaybackTarget::AlbumArtist(uri) => {
            let album_artist = matches!(dragged, PlaybackTarget::AlbumArtist(_));
            selected!(
                library::ArtistRow,
                library::ArtistKey,
                artist_key,
                |key| PlaybackTarget::ArtistKey(key, album_artist),
                |row: &library::ArtistRow| &row.media_uri == uri
            )
        }
        PlaybackTarget::Genre(key) => selected!(
            library::GenreRow,
            library::GenreKey,
            genre_key,
            PlaybackTarget::Genre,
            |row: &library::GenreRow| row.genre_key == *key
        ),
        PlaybackTarget::Mood(key) => selected!(
            library::MoodRow,
            library::MoodKey,
            mood_key,
            PlaybackTarget::Mood,
            |row: &library::MoodRow| row.mood_key == *key
        ),
        PlaybackTarget::Playlist(key) => selected!(
            library::PlaylistRow,
            library::PlaylistKey,
            playlist_key,
            PlaybackTarget::Playlist,
            |row: &library::PlaylistRow| row.playlist_key == *key
        ),
        PlaybackTarget::SmartPlaylist(key) => selected!(
            library::SmartPlaylistRow,
            library::SmartPlaylistKey,
            smart_playlist_key,
            PlaybackTarget::SmartPlaylist,
            |row: &library::SmartPlaylistRow| row.smart_playlist_key == *key
        ),
        _ => None,
    }
}

fn compact_playlist_drag_icon(cover: Option<gtk::Widget>, title: &str) -> gtk::Widget {
    let root = gtk::Overlay::new();
    root.add_css_class("card");
    root.add_css_class("playlist-drag-preview");
    root.set_overflow(gtk::Overflow::Hidden);
    let measure = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    measure.set_size_request(PLAYLIST_DRAG_ICON_WIDTH, PLAYLIST_DRAG_ICON_COVER_SIZE + 8);
    measure.set_can_target(false);
    measure.set_accessible_role(gtk::AccessibleRole::Presentation);
    root.set_child(Some(&measure));

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.set_margin_top(4);
    content.set_margin_bottom(4);
    content.set_margin_start(4);
    content.set_margin_end(4);

    let cover: gtk::Widget = cover.unwrap_or_else(|| {
        let cover = gtk::Image::from_icon_name("rufin-tracks-symbolic");
        cover.add_css_class("muted");
        cover.set_pixel_size(20);
        cover.upcast()
    });
    let cover_slot = gtk::Overlay::new();
    cover_slot.set_overflow(gtk::Overflow::Hidden);
    let cover_measure = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    cover_measure.set_size_request(PLAYLIST_DRAG_ICON_COVER_SIZE, PLAYLIST_DRAG_ICON_COVER_SIZE);
    cover_measure.set_can_target(false);
    cover_measure.set_accessible_role(gtk::AccessibleRole::Presentation);
    cover_slot.set_child(Some(&cover_measure));
    cover.set_hexpand(true);
    cover.set_vexpand(true);
    cover.set_halign(gtk::Align::Fill);
    cover.set_valign(gtk::Align::Fill);
    cover_slot.add_overlay(&cover);
    cover_slot.set_measure_overlay(&cover, false);
    cover_slot.set_clip_overlay(&cover, true);
    content.append(&cover_slot);

    let title = gtk::Label::new(Some(title));
    title.add_css_class("heading");
    title.set_hexpand(true);
    title.set_xalign(0.0);
    title.set_max_width_chars(18);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&title);
    root.add_overlay(&content);
    root.set_measure_overlay(&content, false);
    root.set_clip_overlay(&content, true);
    root.upcast()
}

pub(crate) fn install_track_drag_source(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    selection: TrackSelection,
    current_track: impl Fn() -> Option<(String, String, ArtworkBinding)> + 'static,
) {
    target.as_ref().set_valign(gtk::Align::Fill);
    let preview = MediaDragPreviewBinding::default();
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::COPY)
        .build();
    source.set_propagation_phase(gtk::PropagationPhase::Capture);
    let prepare_preview = preview.clone();
    let preview_shell = Rc::downgrade(shell);
    source.connect_prepare(move |_, _, _| {
        prepare_preview.clear();
        let shell = preview_shell.upgrade()?;
        let (track, title, artwork) = current_track()?;
        prepare_preview.prepare_cache_only(&shell, title, artwork);
        Some(media_drag_content_provider(MediaDragSource::selection(
            selection.dragged_tracks_for(&track),
        )))
    });
    preview.connect(&source);
    target.as_ref().add_controller(source);
}

pub(crate) fn append_context_menu_picker(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    target: PlaybackTarget,
) {
    let source = MediaDragSource::capture_target(shell, target);
    append_context_menu_picker_source(surface, shell, source);
}

pub(crate) fn append_context_menu_picker_selection(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    selection: TrackSelectionSnapshot,
) {
    append_context_menu_picker_source(surface, shell, MediaDragSource::selection(selection));
}

pub(crate) fn append_context_menu_picker_media(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    media_uri: String,
) {
    append_context_menu_picker_source(surface, shell, MediaDragSource::media_uris([media_uri]));
}

pub(crate) fn append_context_menu_picker_media_uris(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    media_uris: impl IntoIterator<Item = String>,
) {
    append_context_menu_picker_source(surface, shell, MediaDragSource::media_uris(media_uris));
}

pub(crate) fn present_playlist_picker_media_uris(
    shell: &Rc<Shell>,
    media_uris: impl IntoIterator<Item = String>,
) {
    open_full_playlist_picker(shell, MediaDragSource::media_uris(media_uris));
}

pub(crate) fn append_context_menu_picker_entries(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    selection: PlaylistEntrySelectionSnapshot,
) {
    append_context_menu_picker_source(surface, shell, MediaDragSource::playlist_entries(selection));
}

fn append_context_menu_picker_source(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    source: MediaDragSource,
) {
    let picker = context_playlist_picker(shell, source.clone(), surface.popover());
    let click_shell = Rc::downgrade(shell);
    surface.append_configurable_widget_submenu(
        ContextMenuItem::AddToPlaylist,
        msgid("Add to Playlist"),
        "playlist-picker",
        &picker,
        ADD_TO_PLAYLIST_ICON,
        move || {
            if let Some(shell) = click_shell.upgrade() {
                open_full_playlist_picker(&shell, source.clone());
            }
        },
    );
}

pub(crate) fn present_playlist_picker_selection(
    shell: &Rc<Shell>,
    selection: TrackSelectionSnapshot,
) {
    open_full_playlist_picker(shell, MediaDragSource::selection(selection));
}

pub(crate) fn present_playlist_picker_entries(
    shell: &Rc<Shell>,
    selection: PlaylistEntrySelectionSnapshot,
) {
    open_full_playlist_picker(shell, MediaDragSource::playlist_entries(selection));
}

fn open_full_playlist_picker(shell: &Rc<Shell>, source: MediaDragSource) {
    let source_key = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.source_key);
    let database = shell.products.library.clone();
    let task = shell.products.runtime.spawn(async move {
        let (tracks, subject) = source.resolve(&database).await?;
        let playlists = database
            .playlist_destinations(source_key, &library::ReadCancellation::new())
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((playlists, tracks, subject))
    });
    let shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let Some((playlists, tracks, subject)) = task.await.ok().and_then(Result::ok) else {
            return;
        };
        if let Some(shell) = shell.upgrade() {
            present_playlist_picker(&shell, source_key, playlists, tracks, subject);
        }
    });
}

fn context_playlist_picker(
    shell: &Rc<Shell>,
    source: MediaDragSource,
    popover: &gtk::PopoverMenu,
) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("context-playlist-submenu");
    root.set_hexpand(true);

    let started = Cell::new(false);
    let picker_shell = Rc::downgrade(shell);
    let picker_popover = popover.downgrade();
    root.connect_map(move |root| {
        if started.replace(true) {
            return;
        }
        let (Some(shell), Some(popover)) = (picker_shell.upgrade(), picker_popover.upgrade())
        else {
            return;
        };
        populate_context_playlist_picker(root, &shell, source.clone(), &popover);
    });
    root
}

fn populate_context_playlist_picker(
    root: &gtk::Box,
    shell: &Rc<Shell>,
    source: MediaDragSource,
    popover: &gtk::PopoverMenu,
) {
    let resource = crate::ui_resource::PLAYLIST_PICKER_CONTEXT_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
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
    let activate_shell = Rc::downgrade(shell);
    let activate_model = filtered.clone();
    let activate_skip = skip_duplicates.clone();
    let activate_popover = popover.downgrade();
    list.connect_activate(move |_, position| {
        let Some(shell) = activate_shell.upgrade() else {
            return;
        };
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
        let feedback_shell = Rc::downgrade(&shell);
        shell.add_media_to_playlist(
            choice.key,
            ready.media_uris.to_vec(),
            activate_skip.is_active(),
            Rc::new(move |accepted| {
                if accepted > 0
                    && let Some(shell) = feedback_shell.upgrade()
                {
                    shell.show_operation_feedback(&OperationFeedback {
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

    let source_key = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.source_key);
    let database = shell.products.library.clone();
    let task = shell.products.runtime.spawn(async move {
        let (tracks, subject) = source.resolve(&database).await?;
        let playlists = database
            .playlist_destinations(source_key, &library::ReadCancellation::new())
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((playlists, tracks, subject))
    });
    let root = root.downgrade();
    let spinner = spinner.downgrade();
    gtk::glib::spawn_future_local(async move {
        let Some((playlists, tracks, subject)) = task.await.ok().and_then(Result::ok) else {
            return;
        };
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
    });
}

#[derive(Clone)]
struct PickerRow {
    row: gtk::Widget,
    check: gtk::CheckButton,
    playlist: library::PlaylistRow,
    haystack: String,
}

fn present_playlist_picker(
    shell: &Rc<Shell>,
    source_key: Option<library::SourceKey>,
    playlists: Vec<library::PlaylistRow>,
    media_uris: Vec<String>,
    subject: DownloadSubject,
) {
    let resource = crate::ui_resource::PLAYLIST_PICKER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        dialog: adw::Dialog,
        search: gtk::SearchEntry,
        list: gtk::Box,
        create: gtk::Button,
        skip: gtk::CheckButton,
        cancel: gtk::Button,
        add: gtk::Button,
    });
    let rows = Rc::new(RefCell::new(Vec::<PickerRow>::new()));
    replace_picker_rows(shell, &list, &create, &rows, &add, playlists);

    let close_shell = Rc::downgrade(shell);
    dialog.connect_closed(move |_| {
        if let Some(shell) = close_shell.upgrade() {
            shell.set_playlist_picker_refresh(None);
        }
    });
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
    let create_shell = Rc::downgrade(shell);
    let create_tracks = media_uris.clone();
    let create_search = search.clone();
    create.connect_clicked(move |_| {
        let Some(shell) = create_shell.upgrade() else {
            return;
        };
        let name = create_search.text().trim().to_string();
        if !name.is_empty() {
            shell.new_playlist_dialog_with(name, create_tracks.clone());
            create_search.set_text("");
        }
    });
    let add_shell = Rc::downgrade(shell);
    let add_rows = Rc::clone(&rows);
    let add_dialog = dialog.downgrade();
    add.connect_clicked(move |_| {
        let Some(shell) = add_shell.upgrade() else {
            return;
        };
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
            let feedback_shell = Rc::downgrade(&shell);
            let feedback_subject = subject.clone();
            let preview_uris = media_uris.iter().take(4).cloned().collect::<Vec<_>>();
            let name = playlist.name.clone();
            shell.add_media_to_playlist(
                playlist.playlist_key,
                media_uris.clone(),
                skip.is_active(),
                Rc::new(move |count| {
                    accepted.set(accepted.get() + count);
                    if count > 0 {
                        accepted_names.borrow_mut().push(name.clone());
                    }
                    pending.set(pending.get().saturating_sub(1));
                    if pending.get() == 0
                        && accepted.get() > 0
                        && let Some(shell) = feedback_shell.upgrade()
                    {
                        let names = accepted_names.borrow();
                        let destination = match names.as_slice() {
                            [name] => name.clone(),
                            _ => format!("{} {}", names.len(), tr("Playlists")),
                        };
                        shell.show_operation_feedback(&OperationFeedback {
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
    let refresh_shell = Rc::downgrade(shell);
    let refresh_list = list.clone();
    let refresh_create = create.clone();
    let refresh_rows = Rc::clone(&rows);
    let refresh_add = add.clone();
    shell.set_playlist_picker_refresh(Some(Rc::new(move || {
        if refresh_dialog.upgrade().is_none() {
            return;
        }
        let Some(shell_now) = refresh_shell.upgrade() else {
            return;
        };
        let database = shell_now.products.library.clone();
        let task = shell_now.products.runtime.spawn(async move {
            database
                .playlist_destinations(source_key, &library::ReadCancellation::new())
                .await
        });
        let shell = refresh_shell.clone();
        let list = refresh_list.clone();
        let create = refresh_create.clone();
        let rows = Rc::clone(&refresh_rows);
        let add = refresh_add.clone();
        let dialog = refresh_dialog.clone();
        gtk::glib::spawn_future_local(async move {
            let Some(playlists) = task.await.ok().and_then(Result::ok) else {
                return;
            };
            if dialog.upgrade().is_some()
                && let Some(shell) = shell.upgrade()
            {
                replace_picker_rows(&shell, &list, &create, &rows, &add, playlists);
            }
        });
    })));
    shell.present_selected_dialog(&dialog);
}

fn replace_picker_rows(
    shell: &Rc<Shell>,
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
    let prefer_server = shell
        .settings
        .current
        .borrow()
        .prefer_server_playlist_covers;
    for playlist in playlists {
        let row = PlaylistPickerRowView::new();
        let check = row.imp().check.get();
        let artwork = playlist_artwork(&playlist, prefer_server);
        let cover = shell
            .cover_group_projection_for_artwork(
                &artwork,
                48,
                crate::shell::cover::THUMB_COVER_SIZE as i32,
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

impl MediaDragSource {
    pub(crate) async fn queue_input(&self) -> Result<library::QueueInput, String> {
        Ok(match self {
            Self::LiveCollection {
                media_uri,
                operations,
            } => prepare_live_collection(operations, media_uri)
                .await?
                .queue_input(None, None),
            Self::Target {
                source_key,
                folder,
                target,
            } => target.queue_input(*source_key, *folder),
            Self::Targets {
                source_key,
                folder,
                targets,
            } => library::QueueInput::Groups(
                targets
                    .iter()
                    .map(|target| target.queue_input(*source_key, *folder))
                    .collect(),
            ),
            Self::Selection(selection) => library::QueueInput::MediaUris {
                order: selection.media_uris.clone(),
                provenance: library::QueueProvenance::Manual,
            },
            Self::PlaylistEntries(selection) => library::QueueInput::PlaylistEntries {
                order: selection.entries.clone(),
                context_id: format!("playlist-selection:{}", selection.playlist).into(),
            },
            Self::MediaUris(order)
            | Self::Queue {
                media_uris: order, ..
            } => library::QueueInput::MediaUris {
                order: order.clone(),
                provenance: library::QueueProvenance::Manual,
            },
        })
    }

    pub(crate) async fn resolve(
        &self,
        database: &library::Database,
    ) -> Result<(Vec<String>, DownloadSubject), String> {
        match self {
            Self::LiveCollection {
                media_uri,
                operations,
            } => {
                let target = prepare_live_collection(operations, media_uri).await?;
                let media_uris = target.resolve_media_uris(database, None, None).await?;
                let subject = target.download_subject();
                Ok((media_uris, subject))
            }
            Self::Target {
                source_key,
                folder,
                target,
            } => {
                let media_uris = target
                    .resolve_media_uris(database, *source_key, *folder)
                    .await?;
                let subject = target.download_subject();
                Ok((media_uris, subject))
            }
            Self::Selection(selection) => {
                Ok((selection.media_uris.to_vec(), selection.download_subject()))
            }
            Self::Targets {
                source_key,
                folder,
                targets,
            } => {
                let mut media_uris = Vec::new();
                for target in targets {
                    media_uris.extend(
                        target
                            .resolve_media_uris(database, *source_key, *folder)
                            .await?,
                    );
                }
                let subject = DownloadSubject::for_media_uris("collections", None, &media_uris);
                Ok((media_uris, subject))
            }
            Self::PlaylistEntries(selection) => {
                let media_uris = selection.media_uris(database).await?;
                let subject =
                    DownloadSubject::for_media_uris("playlist", Some("Playlist"), &media_uris);
                Ok((media_uris, subject))
            }
            Self::MediaUris(media_uris) | Self::Queue { media_uris, .. } => {
                let media_uris = media_uris.to_vec();
                let subject = DownloadSubject::for_media_uris("playlist-media", None, &media_uris);
                Ok((media_uris, subject))
            }
        }
    }
}

async fn prepare_live_collection(
    operations: &crate::runtime::source::SourceHandle,
    media_uri: &str,
) -> Result<PlaybackTarget, String> {
    operations
        .prepare_collection(media_uri.to_string())
        .recv()
        .await
        .map_err(|error| error.to_string())??;
    match library::source_entity_parts(media_uri)
        .map(|(_, kind, _)| kind)
        .as_deref()
    {
        Some("album") => Ok(PlaybackTarget::Album(media_uri.to_string())),
        Some("artist") => Ok(PlaybackTarget::Artist(media_uri.to_string())),
        _ => Err("Collection is no longer available".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display"]
    fn playlist_picker_row_template_builds() {
        gtk::init().expect("GTK display");
        crate::application::verify_interface_resources().expect("compiled interface resources");
        let row = PlaylistPickerRowView::new();
        assert!(row.imp().check.parent().is_some());
        assert!(row.imp().cover_host.parent().is_some());
        assert!(row.imp().genres.parent().is_some());
    }

    #[test]
    fn playlist_drag_value_roundtrips_the_media_uris() {
        let expected = std::sync::Arc::<[String]>::from([
            "https://example.test/three".to_string(),
            "https://example.test/five".to_string(),
        ]);
        let payload = glib::BoxedAnyObject::new(MediaDragSource::MediaUris(expected.clone()));
        let decoded = media_drag_source(&payload.to_value()).expect("scoped playlist drag source");
        let MediaDragSource::MediaUris(decoded) = decoded else {
            panic!("track selection source");
        };
        assert_eq!(decoded, expected);
    }
}
