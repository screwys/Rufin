use crate::widgets::{
    artwork::ArtworkTile,
    selection::{PlayTarget, PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot},
};
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::glib;
use rufin_core::collection_context::CollectionTarget;
use rufin_core::playback::PlayRequest;
use std::ops::Range;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
pub enum CollectionTargets {
    Ready(Vec<PlayTarget>),
    Parents(Vec<(library::ListRead, Vec<Range<usize>>)>),
}

impl CollectionTargets {
    pub fn count(&self) -> usize {
        match self {
            Self::Ready(targets) => targets.len(),
            Self::Parents(reads) => reads
                .iter()
                .flat_map(|(_, ranges)| ranges)
                .map(|range| range.len())
                .sum(),
        }
    }
    /// The playlists and smart playlists it names.
    pub async fn collections(
        &self,
        catalog: &library::Catalog,
    ) -> Result<Vec<library::Collection>, String> {
        use rufin_core::route::Route;
        match self {
            Self::Ready(targets) => targets
                .iter()
                .map(|target| match target {
                    PlayTarget::Route(Route::PlaylistDetail(key)) => {
                        Ok(library::Collection::Playlist(*key))
                    }
                    PlayTarget::Route(Route::SmartPlaylistDetail(key)) => {
                        Ok(library::Collection::Smart { key: *key, now: 0 })
                    }
                    _ => Err("Selection does not contain playlists".into()),
                })
                .collect(),
            Self::Parents(reads) => {
                let mut collections = Vec::new();
                for (read, ranges) in reads {
                    collections.extend(selected_parent_collections(catalog, read, ranges).await?);
                }
                Ok(collections)
            }
        }
    }
    /// What each selected row plays.
    pub async fn resolve(&self, catalog: &library::Catalog) -> Result<Vec<PlayTarget>, String> {
        match self {
            Self::Ready(targets) => Ok(targets.clone()),
            Self::Parents(reads) => {
                let mut targets = Vec::new();
                for (read, ranges) in reads {
                    let collections = selected_parent_collections(catalog, read, ranges).await?;
                    targets.extend(collections.iter().filter_map(|collection| {
                        Some(PlayTarget::Route(
                            CollectionTarget::of(catalog, collection)?.route(),
                        ))
                    }));
                }
                Ok(targets)
            }
        }
    }
    /// What playing every selected row queues, each as its page plays.
    pub async fn requests(
        &self,
        catalog: &library::Catalog,
        source: &rufin_core::runtime::SourceHandle,
    ) -> Result<Vec<PlayRequest>, String> {
        let mut requests = Vec::new();
        for target in self.resolve(catalog).await? {
            requests.extend(
                target
                    .request(source, rufin_core::playback::QueuePlacement::Now, false)
                    .await?,
            );
        }
        Ok(requests)
    }
}

async fn selected_parent_collections(
    catalog: &library::Catalog,
    read: &library::ListRead,
    ranges: &[Range<usize>],
) -> Result<Vec<library::Collection>, String> {
    let cancel = library::ReadCancellation::new();
    catalog
        .selected_collections(read, ranges, &cancel)
        .await
        .map_err(|error| error.to_string())
}

/// Lets a drag of a selected collection row carry the selected rows of `rows`' list.
pub fn install_parent_selection(rows: &crate::widgets::view_model::Rows<library::Row>) {
    let weak = rows.list_model().downgrade();
    rows.list_model()
        .set_collection_selection(Box::new(move |dragged, positions| {
            let rows = crate::widgets::view_model::Rows::<library::Row>::of(&weak.upgrade()?);
            let read = rows.view()?.list()?.clone();
            let PlayTarget::Route(dragged) = dragged else {
                return None;
            };
            let same = |row: &library::Row| {
                rufin_core::route::Route::of_row(row).as_ref() == Some(dragged)
            };
            let position = rows.ready_position(same)?;
            positions.contains(position).then(|| {
                CollectionTargets::Parents(vec![(
                    read,
                    crate::widgets::selection::selected_ranges(positions),
                )])
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
        artwork: std::rc::Weak<crate::widgets::artwork::ArtworkState>,
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
        artwork: &Rc<crate::widgets::artwork::ArtworkState>,
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
        target: PlayTarget,
    },
    Targets {
        targets: CollectionTargets,
    },
    Selection(TrackSelectionSnapshot),
    PlaylistEntries(PlaylistEntrySelectionSnapshot),
    MediaUris(std::sync::Arc<[String]>),
    Queue {
        selection: rufin_core::playback::QueueSelection,
        media_uris: std::sync::Arc<[String]>,
    },
}

impl MediaDragSource {
    pub fn capture_target(target: PlayTarget) -> Self {
        Self::Target { target }
    }

    pub fn selection(selection: TrackSelectionSnapshot) -> Self {
        Self::Selection(selection)
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
    selected_collection_targets(&widget, target)
        .map(|targets| MediaDragSource::Targets { targets })
        .unwrap_or(source)
}

pub fn selected_collection_targets(
    widget: &gtk::Widget,
    target: &PlayTarget,
) -> Option<CollectionTargets> {
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
    let selection = selection?;
    let positions = selection.selection();
    if positions.size() < 2 {
        return None;
    }
    selection
        .downcast_ref::<crate::widgets::selection::PositionSelectionModel>()
        .and_then(|selection| selection.collection_selection(target, &positions))
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
    /// The tracks it holds for `scope`; a collection that holds nothing adds none.
    pub async fn track_selection(
        &self,
        catalog: &library::Catalog,
        source: &rufin_core::runtime::SourceHandle,
        scope: rufin_core::view::SelectionScope,
    ) -> Result<library::MemberSelection, String> {
        let gone = || "Nothing here".to_string();
        match self {
            Self::MediaUris(uris)
            | Self::Queue {
                media_uris: uris, ..
            } => Ok(library::MemberSelection::Uris((uris.to_vec()).into())),
            Self::Selection(TrackSelectionSnapshot { input, .. })
            | Self::PlaylistEntries(PlaylistEntrySelectionSnapshot { input, .. }) => {
                Ok(input.clone())
            }
            Self::LiveCollection {
                media_uri,
                operations,
            } => prepare_live_collection(catalog, operations, media_uri)
                .await?
                .selection(source, scope)
                .await?
                .ok_or_else(gone),
            Self::Target { target } => target.selection(source, scope).await?.ok_or_else(gone),
            Self::Targets {
                targets: CollectionTargets::Ready(targets),
            } => {
                let mut selections = Vec::new();
                for target in targets {
                    selections.extend(target.selection(source, scope).await?);
                }
                Ok(library::MemberSelection::Concat(selections))
            }
            Self::Targets {
                targets: CollectionTargets::Parents(reads),
            } => Ok(library::MemberSelection::Concat(
                reads
                    .iter()
                    .map(|(read, ranges)| {
                        source.list_selection(read.clone(), ranges.clone(), scope)
                    })
                    .collect(),
            )),
        }
    }

    pub async fn playlist_input(
        &self,
        catalog: &library::Catalog,
        source: &rufin_core::runtime::SourceHandle,
    ) -> Result<(library::MemberSelection, String, Vec<String>), String> {
        let selection = self
            .track_selection(catalog, source, rufin_core::view::SelectionScope::Page)
            .await?;
        let title = rufin_core::downloads::selection_title(catalog, &selection)
            .unwrap_or_else(|| localization::tr("Selection"));
        let preview = catalog
            .selected_track_uris(&selection, Some(4), &library::ReadCancellation::new())
            .await
            .map_err(|error| error.to_string())?;
        Ok((selection, title, preview))
    }

    pub async fn play_request(
        &self,
        catalog: &library::Catalog,
        source: &rufin_core::runtime::SourceHandle,
    ) -> Result<PlayRequest, String> {
        let placement = rufin_core::playback::QueuePlacement::Now;
        let gone = || "Nothing to play here".to_string();
        Ok(match self {
            Self::LiveCollection {
                media_uri,
                operations,
            } => prepare_live_collection(catalog, operations, media_uri)
                .await?
                .request(source, placement, false)
                .await?
                .ok_or_else(gone)?,
            Self::Target { target } => target
                .request(source, placement, false)
                .await?
                .ok_or_else(gone)?,
            Self::Targets { targets } => {
                PlayRequest::group(targets.requests(catalog, source).await?, placement)
            }
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
}

/// The page a live search result opens, once its source has its collection.
async fn prepare_live_collection(
    catalog: &library::Catalog,
    operations: &rufin_core::runtime::source::SourceHandle,
    media_uri: &str,
) -> Result<PlayTarget, String> {
    operations
        .prepare_collection(media_uri.to_string())
        .recv()
        .await
        .map_err(|error| error.to_string())??;
    live_collection(catalog, media_uri)
}

/// The page a live search result opens.
fn live_collection(catalog: &library::Catalog, media_uri: &str) -> Result<PlayTarget, String> {
    let uri = media_uri.to_owned();
    Ok(PlayTarget::Route(match catalog.item(media_uri) {
        Some(library::Item::Album(_)) => rufin_core::route::Route::AlbumDetail(uri),
        Some(library::Item::Artist { .. }) => rufin_core::route::Route::ArtistDetail(uri),
        _ => return Err("Collection is no longer available".to_string()),
    }))
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
