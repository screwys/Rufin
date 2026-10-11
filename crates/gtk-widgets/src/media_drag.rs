use crate::selection::collection_play_request;
use crate::{
    artwork::{ArtworkTile, THUMB_COVER_SIZE},
    selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot},
};
use adw::prelude::*;
use artwork::ArtworkBinding;
use downloads::DownloadSubject;
use gtk::glib;
use playback::PlayRequest;
use std::{cell::RefCell, rc::Rc};
use std::{future::Future, ops::Range, pin::Pin, sync::Arc};

pub type CollectionPageLoad = Arc<
    dyn Fn(Range<usize>) -> Pin<Box<dyn Future<Output = Result<Vec<PlayRequest>, String>> + Send>>
        + Send
        + Sync,
>;

pub type CollectionRowsLoad<Q, R> = Arc<
    dyn Fn(
            Q,
            Range<usize>,
            library::ReadCancellation,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<R>, String>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub enum CollectionTargets {
    Ready(Vec<PlayRequest>),
    Query {
        ranges: Vec<Range<usize>>,
        load: CollectionPageLoad,
    },
}

impl CollectionTargets {
    async fn resolve(&self) -> Result<Vec<PlayRequest>, String> {
        match self {
            Self::Ready(targets) => Ok(targets.clone()),
            Self::Query { ranges, load } => {
                let mut targets = Vec::new();
                for range in ranges {
                    for start in (range.start..range.end).step_by(64) {
                        targets.extend(load(start..range.end.min(start + 64)).await?);
                    }
                }
                Ok(targets)
            }
        }
    }
}

pub fn install_collection_selection<K, R, Q>(
    sparse: &Rc<crate::sparse_model::SparseRouteModel<K, R>>,
    request: Rc<RefCell<Q>>,
    load: CollectionRowsLoad<Q, R>,
    target: impl Fn(&R) -> PlayRequest + Send + Sync + 'static,
) where
    K: Clone + Eq + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    Q: Clone + Send + Sync + 'static,
{
    let weak = Rc::downgrade(sparse);
    let target = Arc::new(target);
    sparse
        .list_model()
        .set_collection_selection(Box::new(move |dragged, positions| {
            let sparse = weak.upgrade()?;
            let position = sparse.ready_position(|row| {
                match (target(row).selection(), dragged.selection()) {
                    (
                        Some(library::SongSelection::Query { query: left, .. }),
                        Some(library::SongSelection::Query { query: right, .. }),
                    ) => match (&left.scope, &right.scope) {
                        (
                            library::SongScope::Smart {
                                key: left,
                                source: left_source,
                                ..
                            },
                            library::SongScope::Smart {
                                key: right,
                                source: right_source,
                                ..
                            },
                        ) => left == right && left_source == right_source,
                        _ => left == right,
                    },
                    (left, right) => left == right,
                }
            })?;
            positions.contains(position).then(|| {
                let request = request.borrow().clone();
                let load = Arc::clone(&load);
                let target = Arc::clone(&target);
                CollectionTargets::Query {
                    ranges: crate::selection::selected_ranges(positions),
                    load: Arc::new(move |range| {
                        let rows = load(request.clone(), range, library::ReadCancellation::new());
                        let target = Arc::clone(&target);
                        Box::pin(
                            async move { Ok(rows.await?.iter().map(|row| target(row)).collect()) },
                        )
                    }),
                }
            })
        }));
}
const PLAYLIST_DRAG_ICON_WIDTH: i32 = 180;
const PLAYLIST_DRAG_ICON_COVER_SIZE: i32 = 36;

fn drop_item(widget: &gtk::Widget) -> (gtk::Widget, bool) {
    let mut item = widget.clone();
    while let Some(parent) = item.parent() {
        if parent.is::<gtk::GridView>() {
            return (item, true);
        }
        if item.css_name() == "row" {
            return (item, false);
        }
        item = parent;
    }
    (widget.clone(), false)
}

pub fn order_drop_after(widget: &gtk::Widget, x: f64, y: f64) -> bool {
    if drop_item(widget).1 {
        let right = x >= f64::from(widget.width()) / 2.0;
        right != (widget.direction() == gtk::TextDirection::Rtl)
    } else {
        y >= f64::from(widget.height()) / 2.0
    }
}

pub fn style_order_drop_target(
    drop: &gtk::DropTarget,
    split: bool,
    is_order: impl Fn(&gtk::DropTarget) -> bool + 'static,
) {
    const CLASSES: &[&str] = &[
        "drop-feedback",
        "media-drop-row",
        "order-drop-before",
        "order-drop-after",
        "order-drop-left",
        "order-drop-right",
    ];
    let current = Rc::new(RefCell::new(None::<gtk::glib::WeakRef<gtk::Widget>>));
    let motion_current = Rc::clone(&current);
    let position = Rc::new(std::cell::Cell::new((0.0, 0.0)));
    let motion_position = Rc::clone(&position);
    let motion = Rc::new(move |drop: &gtk::DropTarget, x: f64, y: f64| {
        motion_position.set((x, y));
        if let Some(previous) = motion_current
            .borrow_mut()
            .take()
            .and_then(|weak| weak.upgrade())
        {
            for class in CLASSES {
                previous.remove_css_class(class);
            }
        }
        let Some(widget) = drop.widget() else {
            return gtk::gdk::DragAction::empty();
        };
        let (row, grid) = drop_item(&widget);
        let order = is_order(drop);
        let after = split && order_drop_after(&widget, x, y);
        row.add_css_class("drop-feedback");
        row.add_css_class(if !order {
            "media-drop-row"
        } else if grid {
            if after != (widget.direction() == gtk::TextDirection::Rtl) {
                "order-drop-right"
            } else {
                "order-drop-left"
            }
        } else if after {
            "order-drop-after"
        } else {
            "order-drop-before"
        });
        motion_current.replace(Some(row.downgrade()));
        let actions = drop
            .current_drop()
            .map_or(drop.actions(), |current| current.actions() & drop.actions());
        if order && actions.contains(gtk::gdk::DragAction::MOVE) {
            gtk::gdk::DragAction::MOVE
        } else {
            gtk::gdk::DragAction::COPY
        }
    });
    let enter = Rc::clone(&motion);
    drop.connect_enter(move |drop, x, y| enter(drop, x, y));
    let changed = Rc::clone(&motion);
    let active = Rc::clone(&current);
    drop.set_preload(true);
    drop.connect_notify_local(Some("value"), move |drop, _| {
        let entered = active.borrow().is_some();
        if entered {
            let (x, y) = position.get();
            changed(drop, x, y);
        }
    });
    drop.connect_motion(move |drop, x, y| motion(drop, x, y));
    drop.connect_leave(move |_| {
        if let Some(row) = current.borrow_mut().take().and_then(|weak| weak.upgrade()) {
            for class in CLASSES {
                row.remove_css_class(class);
            }
        }
    });
}

#[derive(Clone, Default)]
pub struct MediaDragPreviewBinding {
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
        artwork: std::rc::Weak<crate::artwork::ArtworkState>,
        binding: ArtworkBinding,
    },
}

impl MediaDragPreviewBinding {
    pub fn connect(&self, source: &gtk::DragSource) {
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
                PlaylistDragPreviewArtwork::CacheOnly { artwork, binding } => {
                    artwork.upgrade().map(|artwork| {
                        let tile = ArtworkTile::new(PLAYLIST_DRAG_ICON_COVER_SIZE);
                        artwork.bind_cache_only_artwork_tile(
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

    pub fn prepare(&self, title: String, artwork: Option<gtk::gdk::Paintable>) {
        self.prepared.replace(Some(PlaylistDragPreview {
            title,
            artwork: PlaylistDragPreviewArtwork::Resident(artwork),
        }));
    }

    pub fn prepare_cache_only(
        &self,
        artwork: &Rc<crate::artwork::ArtworkState>,
        title: String,
        binding: ArtworkBinding,
    ) {
        self.prepared.replace(Some(PlaylistDragPreview {
            title,
            artwork: PlaylistDragPreviewArtwork::CacheOnly {
                artwork: Rc::downgrade(artwork),
                binding,
            },
        }));
    }

    pub fn clear(&self) {
        self.prepared.borrow_mut().take();
        self.active_tile.borrow_mut().take();
    }
}

#[derive(Clone)]
pub enum MediaDragSource {
    LiveCollection {
        media_uri: String,
        operations: rufin_core::runtime::source::SourceHandle,
    },
    Target {
        target: PlayRequest,
    },
    Targets {
        targets: CollectionTargets,
    },
    Selection(TrackSelectionSnapshot),
    PlaylistEntries(PlaylistEntrySelectionSnapshot),
    MediaUris(std::sync::Arc<[String]>),
    Queue {
        selection: playback::QueueSelection,
        media_uris: std::sync::Arc<[String]>,
    },
}

impl MediaDragSource {
    pub fn capture_target(target: PlayRequest) -> Self {
        Self::Target { target }
    }

    pub fn selection(selection: TrackSelectionSnapshot) -> Self {
        Self::Selection(selection)
    }

    pub fn track(media_uri: String) -> Self {
        Self::media_uris([media_uri])
    }

    pub fn media_uris(media_uris: impl IntoIterator<Item = String>) -> Self {
        Self::MediaUris(media_uris.into_iter().collect::<Vec<_>>().into())
    }

    pub fn playlist_entries(selection: PlaylistEntrySelectionSnapshot) -> Self {
        Self::PlaylistEntries(selection)
    }
}

pub fn media_drag_content_provider(source: MediaDragSource) -> gtk::gdk::ContentProvider {
    let payload = glib::BoxedAnyObject::new(source);
    gtk::gdk::ContentProvider::for_value(&payload.to_value())
}

pub fn media_drag_source(value: &glib::Value) -> Option<MediaDragSource> {
    let payload = value.get::<glib::BoxedAnyObject>().ok()?;
    Some(payload.try_borrow::<MediaDragSource>().ok()?.clone())
}

pub fn install_compact_media_drag_source(
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

pub fn install_media_drag_source(
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
    let MediaDragSource::Target { target } = &source else {
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
        })
        .or_else(|| {
            widget
                .ancestor(gtk::ListView::static_type())
                .and_downcast::<gtk::ListView>()
                .and_then(|list| list.model())
        });
    let Some(selection) = selection else {
        return source;
    };
    let positions = selection.selection();
    if positions.size() < 2 {
        return source;
    }
    if let Some(targets) = selection
        .downcast_ref::<crate::selection::PositionSelectionModel>()
        .and_then(|selection| selection.collection_selection(target, &positions))
    {
        return MediaDragSource::Targets { targets };
    }
    source
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

impl MediaDragSource {
    pub async fn song_selection(
        &self,
        database: &library::Database,
    ) -> Result<library::SongSelection, String> {
        match self {
            Self::MediaUris(uris)
            | Self::Queue {
                media_uris: uris, ..
            } => database
                .remember_media_uris(uris)
                .await
                .map(|keys| library::SongSelection::Tracks(keys.into()))
                .map_err(|error| error.to_string()),
            _ => Ok(self
                .play_request(database)
                .await?
                .selection()
                .expect("song-backed collection drag")),
        }
    }

    pub async fn play_request(&self, database: &library::Database) -> Result<PlayRequest, String> {
        let placement = playback::QueuePlacement::Now;
        Ok(match self {
            Self::LiveCollection {
                media_uri,
                operations,
            } => prepare_live_collection(database, operations, media_uri).await?,
            Self::Target { target } => target.clone(),
            Self::Targets { targets } => PlayRequest::group(targets.resolve().await?, placement),
            Self::Selection(selection) => {
                PlayRequest::ordered(selection.input.clone(), 0, placement, false)
            }
            Self::PlaylistEntries(selection) => {
                PlayRequest::ordered(selection.input.clone(), 0, placement, false)
            }
            Self::MediaUris(order)
            | Self::Queue {
                media_uris: order, ..
            } => PlayRequest::uris(order.to_vec(), placement),
        })
    }

    pub async fn resolve(
        &self,
        database: &library::Database,
    ) -> Result<(Vec<String>, DownloadSubject), String> {
        let mut prepared = None;
        let uris = match self {
            Self::MediaUris(uris)
            | Self::Queue {
                media_uris: uris, ..
            } => uris.to_vec(),
            _ => {
                let request = self.play_request(database).await?;
                let selection = request.selection().expect("song-backed collection drag");
                prepared = Some(request);
                database
                    .selected_track_uris(&selection, &library::ReadCancellation::new())
                    .await
                    .map_err(|error| error.to_string())?
            }
        };
        let subject = match self {
            Self::Target { .. } | Self::LiveCollection { .. } => {
                let target = prepared.as_ref().expect("prepared collection");
                let title = match target.selection() {
                    Some(library::SongSelection::Tracks(_)) => localization::tr("Track"),
                    Some(library::SongSelection::Query { query, .. }) => match query.scope {
                        library::SongScope::Album(_) => localization::tr("Album"),
                        library::SongScope::Artist { .. } => localization::tr("Artist"),
                        library::SongScope::Genre(_) => localization::tr("Genre"),
                        library::SongScope::Mood(_) => localization::tr("Mood"),
                        library::SongScope::Playlist(_) => localization::tr("Playlist"),
                        library::SongScope::Smart { .. } => localization::tr("Smart Playlist"),
                        _ => localization::tr("Folder"),
                    },
                    _ => localization::tr("Selection"),
                };
                DownloadSubject::Prepared {
                    context_id: target.context().map(str::to_string).unwrap_or_else(|| {
                        format!("track:{}", uris.first().map_or("", String::as_str))
                    }),
                    title: Some(title),
                }
            }
            Self::PlaylistEntries(_) => {
                DownloadSubject::for_media_uris("playlist", Some("Playlist"), &uris)
            }
            Self::Targets { .. } => DownloadSubject::for_media_uris("collections", None, &uris),
            Self::MediaUris(_) | Self::Queue { .. } => {
                DownloadSubject::for_media_uris("playlist-media", None, &uris)
            }
            Self::Selection(_) => DownloadSubject::for_media_uris("track-selection", None, &uris),
        };
        Ok((uris, subject))
    }
}

async fn prepare_live_collection(
    database: &library::Database,
    operations: &rufin_core::runtime::source::SourceHandle,
    media_uri: &str,
) -> Result<PlayRequest, String> {
    operations
        .prepare_collection(media_uri.to_string())
        .recv()
        .await
        .map_err(|error| error.to_string())??;
    let cancellation = library::ReadCancellation::new();
    let scope = match library::source_entity_parts(media_uri)
        .map(|(_, kind, _)| kind)
        .as_deref()
    {
        Some("album") => database
            .album_row_by_media_uri(media_uri, &cancellation)
            .await
            .map_err(|error| error.to_string())?
            .map(|row| library::SongScope::Album(row.album_key)),
        Some("artist") => database
            .artist_row_by_media_uri(media_uri, &cancellation)
            .await
            .map_err(|error| error.to_string())?
            .map(|row| library::SongScope::Artist {
                key: row.artist_key,
                album_artist: false,
            }),
        _ => None,
    }
    .ok_or_else(|| "Collection is no longer available".to_string())?;
    let kind = match &scope {
        library::SongScope::Album(_) => "album",
        _ => "artist",
    };
    Ok(collection_play_request(scope).with_context(format!("{kind}:{media_uri}").into()))
}

#[cfg(test)]
mod tests {
    use super::*;
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
