use std::path::PathBuf;

use library::{
    AlbumKey, ArtistKey, Catalog, FolderKey, GenreKey, HomeEntryInput, HomeEntryKind,
    QueueRepeatMode, QueueTransferRow, SourceKey, TrackKey,
};
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

pub struct Fixture {
    pub _directory: tempfile::TempDir,
    pub path: PathBuf,
    pub catalog: Catalog,
    pub source: SourceKey,
    pub tracks: Vec<TrackKey>,
    pub track_uris: Vec<String>,
    pub albums: Vec<AlbumKey>,
    pub album_uris: Vec<String>,
    pub artists: Vec<ArtistKey>,
    pub artist_uris: Vec<String>,
    pub genre: GenreKey,
    pub folder: FolderKey,
}

pub async fn persist_queue(
    catalog: &Catalog,
    occurrences: &[QueueTransferRow],
    current: Option<usize>,
    progress_millis: i64,
    repeat_mode: QueueRepeatMode,
    shuffled: bool,
) {
    let captured = catalog
        .queue_entries(
            library::QueueLoad::items(
                occurrences
                    .iter()
                    .map(|row| (row.item.clone(), row.provenance.clone()))
                    .collect(),
            ),
            None,
        )
        .await
        .expect("capture Queue metadata");
    let state = library::QueueRestore {
        entries: captured
            .entries
            .into_iter()
            .zip(occurrences)
            .map(|(mut entry, row)| {
                entry.received_from_connect = row.received_from_connect;
                entry
            })
            .collect(),
        order: (0..occurrences.len() as u32).collect(),
        current_index: current,
        progress_millis,
        repeat_mode,
        shuffled,
        ..Default::default()
    };
    catalog
        .save_queue(&state)
        .await
        .expect("persist Queue records");
}

pub async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("temporary Library directory");
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.expect("open Library");
    sqlx::query("DELETE FROM smart_playlists")
        .execute(&mut connection(&path).await)
        .await
        .expect("start fixture with no Smart Playlists");
    let source = catalog
        .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
        .await
        .unwrap()
        .source;
    let mut observation = catalog.observe("source", None).await.unwrap();
    observation.start_observation().await.unwrap();
    let mut batch = library::CatalogBatch::default();
    for (id, title, artist, sort, date) in [
        ("album-a", "Album A", "Artist A", "album a", "2024-01-02"),
        ("album-b", "Album B", "Artist B", "album b", "2023-01-02"),
    ] {
        batch.albums.push(library::AlbumObservation {
            object_id: id.into(),
            metadata: library::AlbumMetadata {
                title: title.into(),
                artist: artist.into(),
                sort_text: sort.into(),
                year: Some(2024),
                release_date: Some(date.into()),
                date_added: Some(date.into()),
                musicbrainz_release_group_id: Some(
                    if id == "album-a" {
                        "group-a"
                    } else {
                        "group-b"
                    }
                    .into(),
                ),
                is_compilation: Some(id == "album-a"),
                album_artists: vec![library::ArtistCredit {
                    object_id: if id == "album-a" {
                        "artist-a"
                    } else {
                        "artist-b"
                    }
                    .into(),
                    name: artist.into(),
                    ..Default::default()
                }],
                genres: vec![library::TagReference {
                    object_id: "genre".into(),
                    name: "Rock".into(),
                    ..Default::default()
                }],
                release_types: vec![if id == "album-a" { "Album" } else { "EP" }.into()],
                ..Default::default()
            },
            user_data: library::NativeUserState {
                favorite: Some(false),
                ..Default::default()
            },
            ..Default::default()
        });
    }
    for (id, name) in [("artist-a", "Artist A"), ("artist-b", "Artist B")] {
        batch.artists.push(library::ArtistObservation {
            object_id: id.into(),
            metadata: library::ArtistMetadata {
                name: name.into(),
                sort_text: Some(name.to_lowercase()),
                ..Default::default()
            },
            user_data: library::NativeUserState {
                favorite: Some(false),
                ..Default::default()
            },
            ..Default::default()
        });
    }
    batch.tags.push(library::TagObservation {
        kind: library::TagKind::Genre,
        object_id: "genre".into(),
        metadata: library::TagMetadata {
            name: "Rock".into(),
            sort_text: Some("rock".into()),
        },
        artwork_binding: None,
    });
    batch.tags.push(library::TagObservation {
        kind: library::TagKind::Mood,
        object_id: "mood".into(),
        metadata: library::TagMetadata {
            name: "Energetic".into(),
            sort_text: Some("energetic".into()),
        },
        artwork_binding: None,
    });
    batch.folders.push(library::FolderObservation {
        object_id: "folder".into(),
        metadata: library::FolderMetadata {
            name: Some("Music".into()),
            sort_text: Some("music".into()),
        },
        ..Default::default()
    });
    for (index, album, artist, title, favorite) in [
        (0, "album-a", "artist-a", "Alpha", false),
        (1, "album-a", "artist-a", "Beta", true),
        (2, "album-b", "artist-b", "Gamma", false),
        (3, "album-b", "artist-b", "Delta", false),
    ] {
        batch.tracks.push(library::TrackObservation {
            object_id: format!("track-{index}"),
            album_object_id: Some(album.into()),
            metadata: library::TrackMetadata {
                title: title.into(),
                album: if album == "album-a" {
                    "Album A"
                } else {
                    "Album B"
                }
                .into(),
                artist: if artist == "artist-a" {
                    "Artist A"
                } else {
                    "Artist B"
                }
                .into(),
                sort_text: title.to_lowercase(),
                duration_millis: 180000 + index * 1000,
                disc_number: 1,
                track_number: index + 1,
                year: Some(2020 + index),
                date_added: Some("2024-01-02".into()),
                source_format: Some("FLAC".into()),
                comment: Some("Note".into()),
                bpm: Some(100 + index),
                artists: vec![library::ArtistCredit {
                    object_id: artist.into(),
                    name: if artist == "artist-a" {
                        "Artist A"
                    } else {
                        "Artist B"
                    }
                    .into(),
                    ..Default::default()
                }],
                genres: vec![library::TagReference {
                    object_id: "genre".into(),
                    name: "Rock".into(),
                    ..Default::default()
                }],
                moods: vec![library::TagReference {
                    object_id: "mood".into(),
                    name: "Energetic".into(),
                    sort_text: Some("energetic".into()),
                }],
                ..Default::default()
            },
            user_data: library::NativeUserState {
                favorite: Some(favorite),
                rating: (Some(Some(5 + index))).map(|rating| rating.map(|value| value * 10)),
                ..Default::default()
            },
            loudness_analysis_key: [index as u8 + 1; 32],
            folders: Some(vec!["folder".into()]),
            ..Default::default()
        });
    }
    observation.apply_catalog(&batch, true).await.unwrap();

    observation
        .replace_source_home_section(
            "featured",
            None,
            &[HomeEntryInput {
                position: 0,
                kind: HomeEntryKind::Track,
                entity_object_id: "track-0".into(),
                title: "Featured Track".into(),
                subtitle: "Featured".into(),
            }],
        )
        .await
        .unwrap();

    observation
        .complete_source_observation(
            &["track", "album", "artist", "genre", "mood", "folder"],
            None,
        )
        .await
        .unwrap();
    assert!(observation.changed());
    drop(observation);
    let mut connection = connection(&path).await;
    let tracks =
        sqlx::query_scalar::<_, TrackKey>("SELECT track_key FROM tracks ORDER BY object_id")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    let track_uris = sqlx::query_scalar("SELECT media_uri FROM tracks ORDER BY object_id")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    let albums =
        sqlx::query_scalar::<_, AlbumKey>("SELECT album_key FROM albums ORDER BY object_id")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    let album_uris = sqlx::query_scalar("SELECT media_uri FROM albums ORDER BY object_id")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    let artists =
        sqlx::query_scalar::<_, ArtistKey>("SELECT artist_key FROM artists ORDER BY object_id")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    let artist_uris = sqlx::query_scalar("SELECT media_uri FROM artists ORDER BY object_id")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    let genre = sqlx::query_scalar("SELECT tag_key FROM tags WHERE kind=0")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let folder = sqlx::query_scalar("SELECT folder_key FROM folders")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    Fixture {
        _directory: directory,
        path,
        catalog,
        source,
        tracks,
        track_uris,
        albums,
        album_uris,
        artists,
        artist_uris,
        genre,
        folder,
    }
}

/// What clients refresh for after a test's committed Writes: the catalog rows of a source,
/// its playlists, its plays, its Home rows or favorites and ratings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Shown(u8);

impl Shown {
    pub const CATALOG: Self = Self(1);
    pub const PLAYLIST: Self = Self(1 << 1);
    pub const STATS: Self = Self(1 << 2);
    pub const HOME: Self = Self(1 << 3);
    pub const STATE: Self = Self(1 << 4);

    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The parts of one Change, by the source they belong to.
    pub fn of(change: &library::Change) -> Vec<(Option<library::SourceKey>, Self)> {
        use library::Scope;
        change
            .scopes
            .iter()
            .filter_map(|scope| match scope {
                Scope::Tracks { source, .. } => Some((*source, Self::CATALOG)),
                Scope::Collections { source } | Scope::Source { key: source, .. } => {
                    Some((Some(*source), Self::CATALOG))
                }
                Scope::Playlist { .. }
                | Scope::Playlists
                | Scope::Smart { .. }
                | Scope::Folders => Some((None, Self::PLAYLIST)),
                Scope::Listened { .. } => Some((None, Self::STATS)),
                Scope::Home { source } => Some((Some(*source), Self::HOME)),
                Scope::State { .. } => Some((None, Self::STATE)),
                Scope::Reload => Some((None, Self::CATALOG)),
                _ => None,
            })
            .collect()
    }
}

impl std::ops::BitOr for Shown {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for Shown {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

/// The Changes a test's Catalog commits.
pub struct Changes(tokio::sync::broadcast::Receiver<std::sync::Arc<library::Change>>);

impl Changes {
    pub fn of(catalog: &Catalog) -> Self {
        Self(catalog.changes())
    }

    /// What every Write committed since the last call changed.
    pub fn facts(&mut self) -> Shown {
        let mut shown = Shown::default();
        while let Ok(change) = self.0.try_recv() {
            for (_, part) in Shown::of(&change) {
                shown |= part;
            }
        }
        shown
    }
}

/// Whether clients refresh for these changes.
pub fn shown(facts: Shown) -> bool {
    !facts.is_empty()
}

/// Rows written straight to the Store reach the Catalog when it opens again.
pub async fn reopen(catalog: Catalog, path: &std::path::Path) -> Catalog {
    catalog.close().await.expect("close Library");
    Catalog::open(path).await.expect("reopen Library")
}

pub async fn connection(path: &std::path::Path) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false),
    )
    .await
    .expect("open fixture connection")
}

/// Capture complete membership with a bounded metadata projection.
pub async fn resolve_queue(
    catalog: &Catalog,
    input: library::QueueLoad,
    anchor_index: usize,
) -> library::QueueRestore {
    let page = catalog.queue_entries(input, None).await.unwrap();
    let start = anchor_index.saturating_sub(10);
    let rows = catalog
        .queue_rows_for_entries(
            &page
                .entries
                .iter()
                .cloned()
                .enumerate()
                .skip(start)
                .take(100)
                .collect::<Vec<_>>(),
            &library::ReadCancellation::new(),
        )
        .await
        .unwrap();
    library::QueueRestore {
        current_index: (!page.entries.is_empty()).then_some(anchor_index),
        order: (0..page.entries.len() as u32).collect(),
        entries: page.entries.into(),
        rows,
        ..Default::default()
    }
}
