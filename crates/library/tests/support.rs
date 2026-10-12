use std::path::PathBuf;

use library::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, HomeEntryInput, HomeEntryKind, MoodKey,
    QueueRepeatMode, QueueRow, Scan, SourceKey, TrackKey,
};
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

pub struct Fixture {
    pub _directory: tempfile::TempDir,
    pub path: PathBuf,
    pub database: Database,
    pub source: SourceKey,
    pub tracks: Vec<TrackKey>,
    pub track_uris: Vec<String>,
    pub albums: Vec<AlbumKey>,
    pub album_uris: Vec<String>,
    pub artists: Vec<ArtistKey>,
    pub artist_uris: Vec<String>,
    pub genre: GenreKey,
    pub mood: MoodKey,
    pub folder: FolderKey,
}

#[allow(clippy::too_many_arguments)]
pub async fn persist_queue(
    database: &Database,
    source: SourceKey,
    occurrences: &[QueueRow],
    current: Option<usize>,
    progress_millis: i64,
    repeat_mode: QueueRepeatMode,
    shuffled: bool,
) {
    let _ = source;
    let captured = database
        .read_queue(library::QueueReadRequest {
            anchor: None,
            context_title: None,
            input: library::QueueLoad::Items(
                occurrences
                    .iter()
                    .map(|row| (row.item.clone(), row.provenance.clone()))
                    .collect(),
            ),
            anchor_index: current.unwrap_or(0),
            random_start: None,
            shuffled: None,
        })
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
    database
        .save_queue(&state)
        .await
        .expect("persist Queue records");
}

pub async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("temporary Library directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library");
    let mut scan = Scan::begin(&database, "source", "Source", "source", None)
        .await
        .expect("begin fixture scan");
    for (id, title, artist, sort, date) in [
        ("album-a", "Album A", "Artist A", "album a", "2024-01-02"),
        ("album-b", "Album B", "Artist B", "album b", "2023-01-02"),
    ] {
        scan.write_album(&library::AlbumInput {
            object_id: id.to_owned(),
            title: title.to_owned(),
            display_artist: artist.to_owned(),
            sort_text: sort.to_owned(),
            year: Some(2024),
            release_date: Some(date.to_owned()),
            date_added: Some(date.to_owned()),
            musicbrainz_release_group_id: Some(
                (if id == "album-a" {
                    "group-a"
                } else {
                    "group-b"
                })
                .to_owned(),
            ),
            is_compilation: Some(id == "album-a"),
            favorite: Some(false),
            ..Default::default()
        })
        .await
        .expect("stage Album");
    }
    for (id, name) in [("artist-a", "Artist A"), ("artist-b", "Artist B")] {
        scan.write_artist(&library::ArtistInput {
            object_id: id.to_owned(),
            name: name.to_owned(),
            sort_text: Some(name.to_lowercase()),
            favorite: Some(false),
            ..Default::default()
        })
        .await
        .expect("stage Artist");
    }
    scan.write_genre(&library::GenreInput {
        object_id: "genre".into(),
        name: "Rock".into(),
        sort_text: Some("rock".into()),
        ..Default::default()
    })
    .await
    .expect("stage Genre");
    scan.write_mood(&library::MoodInput {
        object_id: "mood".into(),
        name: "Energetic".into(),
        sort_text: "energetic".into(),
    })
    .await
    .expect("stage Mood");
    scan.write_folder(&library::FolderInput {
        object_id: "folder".into(),
        name: Some("Music".into()),
        sort_text: Some("music".into()),
        ..Default::default()
    })
    .await
    .expect("stage Folder");
    for (index, album, artist, title, favorite) in [
        (0, "album-a", "artist-a", "Alpha", false),
        (1, "album-a", "artist-a", "Beta", true),
        (2, "album-b", "artist-b", "Gamma", false),
        (3, "album-b", "artist-b", "Delta", false),
    ] {
        let id = format!("track-{index}");
        scan.write_track(&library::TrackInput {
            object_id: id.to_owned(),
            album_object_id: Some(album.to_owned()),
            title: title.to_owned(),
            display_album: (if album == "album-a" {
                "Album A"
            } else {
                "Album B"
            })
            .to_owned(),
            display_artist: (if artist == "artist-a" {
                "Artist A"
            } else {
                "Artist B"
            })
            .to_owned(),
            sort_text: title.to_lowercase(),
            duration_millis: 180_000 + index * 1_000,
            disc_number: 1,
            track_number: index + 1,
            year: Some(2020 + index),
            date_added: Some("2024-01-02".into()),
            source_format: Some("FLAC".into()),
            comment: Some("Note".into()),
            bpm: Some(100 + index),
            favorite: Some(favorite),
            rating: Some(5 + index),
            loudness_analysis_key: [index as u8 + 1; 32],
            ..Default::default()
        })
        .await
        .expect("stage Track");
        scan.write_track_relations(&[(&id, artist)], &[(&id, "genre")], &[(&id, "mood")])
            .await
            .expect("stage Track relations");
        scan.write_track_folders(&[library::ScanLink::new(&id, "folder", 0)])
            .await
            .expect("stage Track Folder");
    }
    scan.write_album_relations(
        &[("album-a", "artist-a"), ("album-b", "artist-b")],
        &[("album-a", "genre"), ("album-b", "genre")],
        &[("album-a", "Album"), ("album-b", "EP")],
    )
    .await
    .expect("stage Album relations");
    scan.replace_home_section(
        "featured",
        None,
        &[HomeEntryInput {
            position: 0,
            kind: HomeEntryKind::Track,
            entity_object_id: "track-0".to_string(),
            title: "Featured Track".to_string(),
            subtitle: "Featured".to_string(),
        }],
    )
    .await
    .expect("stage provider Home entry");
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    scan.finish().await.expect("publish fixture");
    let publication = updates.try_recv().unwrap();
    assert!(publication.metadata);
    let mut connection = connection(&path).await;
    let tracks =
        sqlx::query_scalar::<_, TrackKey>("SELECT track_key FROM tracks ORDER BY object_id")
            .fetch_all(&mut connection)
            .await
            .expect("read Track keys");
    let track_uris =
        sqlx::query_scalar::<_, String>("SELECT media_uri FROM tracks ORDER BY object_id")
            .fetch_all(&mut connection)
            .await
            .expect("read Track URIs");
    let albums =
        sqlx::query_scalar::<_, AlbumKey>("SELECT album_key FROM albums ORDER BY object_id")
            .fetch_all(&mut connection)
            .await
            .expect("read Album keys");
    let album_uris =
        sqlx::query_scalar::<_, String>("SELECT media_uri FROM albums ORDER BY object_id")
            .fetch_all(&mut connection)
            .await
            .expect("read Album URIs");
    let artists =
        sqlx::query_scalar::<_, ArtistKey>("SELECT artist_key FROM artists ORDER BY object_id")
            .fetch_all(&mut connection)
            .await
            .expect("read Artist keys");
    let artist_uris =
        sqlx::query_scalar::<_, String>("SELECT media_uri FROM artists ORDER BY object_id")
            .fetch_all(&mut connection)
            .await
            .expect("read Artist URIs");
    let genre = sqlx::query_scalar("SELECT genre_key FROM genres")
        .fetch_one(&mut connection)
        .await
        .expect("read Genre key");
    let mood = sqlx::query_scalar("SELECT mood_key FROM moods")
        .fetch_one(&mut connection)
        .await
        .expect("read Mood key");
    let folder = sqlx::query_scalar("SELECT folder_key FROM folders")
        .fetch_one(&mut connection)
        .await
        .expect("read Folder key");
    Fixture {
        _directory: directory,
        path,
        database,
        source: publication.source,
        tracks,
        track_uris,
        albums,
        album_uris,
        artists,
        artist_uris,
        genre,
        mood,
        folder,
    }
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
    database: &Database,
    input: library::QueueLoad,
    anchor_index: usize,
) -> library::QueueRestore {
    let mut page = database
        .read_queue(library::QueueReadRequest {
            anchor: None,
            context_title: None,
            input: input,
            anchor_index,
            random_start: None,
            shuffled: None,
        })
        .await
        .unwrap();
    let start = page.current_index.saturating_sub(10);
    page.rows = database
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
        current_index: (!page.entries.is_empty()).then_some(page.current_index),
        order: (0..page.entries.len() as u32).collect(),
        entries: page.entries.into(),
        rows: page.rows,
        ..Default::default()
    }
}
