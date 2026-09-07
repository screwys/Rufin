use crate::{
    artwork::{ArtworkTile, THUMB_COVER_SIZE},
    selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot},
};
use adw::prelude::*;
use artwork::ArtworkBinding;
use downloads::DownloadSubject;
use gtk::glib;
use rufin_core::playback::PlaybackTarget;
use std::{cell::RefCell, rc::Rc};
const PLAYLIST_DRAG_ICON_WIDTH: i32 = 180;
const PLAYLIST_DRAG_ICON_COVER_SIZE: i32 = 36;
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
    pub fn capture_target(
        selected: Option<&rufin_core::runtime::SelectedLibrary>,
        target: PlaybackTarget,
    ) -> Self {
        Self::Target {
            source_key: selected.as_ref().map(|selected| selected.source_key),
            folder: selected
                .as_ref()
                .and_then(|selected| selected.music_folder_key),
            target,
        }
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
    use crate::sparse_model::SparseObjectItem;
    // Sparse placeholders already carry collection keys. A large selection needs no row hydration.
    macro_rules! selected {
        ($row:ty, $key:ty, $field:ident, $target:expr, $matches:expr) => {{
            if let Some(row) =
                crate::sparse_model::object_item::<$row, _>(object.clone(), Clone::clone)
            {
                Some(($target(row.$field), $matches(&row)))
            } else {
                let item = object.downcast_ref::<SparseObjectItem>()?;
                if item.is_ready() {
                    None
                } else {
                    item.value::<$key>().map(|key| ($target(key), false))
                }
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

impl MediaDragSource {
    pub async fn queue_input(&self) -> Result<library::QueueInput, String> {
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

    pub async fn resolve(
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
                let subject = download_subject(&target);
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
                let subject = download_subject(&target);
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
    operations: &rufin_core::runtime::source::SourceHandle,
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

pub fn download_subject(target: &PlaybackTarget) -> DownloadSubject {
    let title = match target {
        PlaybackTarget::Track(_) => localization::tr("Track"),
        PlaybackTarget::Album(_) | PlaybackTarget::AlbumKey(_) => localization::tr("Album"),
        PlaybackTarget::Artist(_)
        | PlaybackTarget::AlbumArtist(_)
        | PlaybackTarget::ArtistKey(..) => localization::tr("Artist"),
        PlaybackTarget::Genre(_) => localization::tr("Genre"),
        PlaybackTarget::Mood(_) => localization::tr("Mood"),
        PlaybackTarget::Playlist(_) => localization::tr("Playlist"),
        PlaybackTarget::SmartPlaylist(_) => localization::tr("Smart Playlist"),
        PlaybackTarget::Contextual { target, .. } => return download_subject(target),
    };
    DownloadSubject::Prepared {
        context_id: target.context_id(),
        title: Some(title),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collection_drag_retains_unhydrated_selected_keys() {
        use crate::sparse_model::SparseObjectModel;
        macro_rules! check {
            ($key:ty, $row:ty, $dragged:expr, $expected:expr) => {{
                let key = <$key>::from_raw(1001);
                let model = SparseObjectModel::new::<$key, $row>(vec![key], 64);
                let object = model.item(0).expect("selected placeholder");
                assert_eq!(
                    selected_collection_target(&object, &$dragged),
                    Some(($expected(key), false))
                );
            }};
        }
        check!(
            library::AlbumKey,
            library::AlbumRow,
            PlaybackTarget::Album("album:dragged".into()),
            PlaybackTarget::AlbumKey
        );
        check!(
            library::ArtistKey,
            library::ArtistRow,
            PlaybackTarget::Artist("artist:dragged".into()),
            |key| PlaybackTarget::ArtistKey(key, false)
        );
        check!(
            library::GenreKey,
            library::GenreRow,
            PlaybackTarget::Genre(library::GenreKey::from_raw(2)),
            PlaybackTarget::Genre
        );
        check!(
            library::MoodKey,
            library::MoodRow,
            PlaybackTarget::Mood(library::MoodKey::from_raw(2)),
            PlaybackTarget::Mood
        );
        check!(
            library::PlaylistKey,
            library::PlaylistRow,
            PlaybackTarget::Playlist(library::PlaylistKey::from_raw(2)),
            PlaybackTarget::Playlist
        );
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
