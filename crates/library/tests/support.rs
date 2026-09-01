use std::path::PathBuf;

use library::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, HomeEntryInput, HomeEntryKind, MoodKey,
    QueueCompactOccurrence, QueueRepeatMode, Scan, ScanOutcome, SourceKey, TrackKey,
};
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

pub struct Fixture {
    pub _directory: tempfile::TempDir,
    pub path: PathBuf,
    pub database: Database,
    pub source: SourceKey,
    pub tracks: Vec<TrackKey>,
    pub albums: Vec<AlbumKey>,
    pub artists: Vec<ArtistKey>,
    pub genre: GenreKey,
    pub mood: MoodKey,
    pub folder: FolderKey,
}

#[allow(clippy::too_many_arguments)]
pub async fn persist_queue(
    database: &Database,
    source: SourceKey,
    occurrences: &[QueueCompactOccurrence],
    current: Option<&str>,
    prepared: Option<&str>,
    progress_millis: i64,
    repeat_mode: QueueRepeatMode,
    shuffled: bool,
) {
    database
        .persist_compact_queue(
            source,
            occurrences.len(),
            |offset, limit| {
                let page = occurrences
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .cloned()
                    .collect();
                async move { Ok(page) }
            },
            current,
            prepared,
            progress_millis,
            repeat_mode,
            shuffled,
        )
        .await
        .expect("persist compact Queue");
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
        scan.write_album(
            id,
            title,
            sort,
            artist,
            sort,
            Some(2024),
            Some(date),
            Some(date),
            None,
            Some(if id == "album-a" {
                "group-a"
            } else {
                "group-b"
            }),
            Some(id == "album-a"),
            None,
            false,
            None,
            None,
        )
        .await
        .expect("stage Album");
    }
    for (id, name) in [("artist-a", "Artist A"), ("artist-b", "Artist B")] {
        scan.write_artist(
            id,
            name,
            &name.to_lowercase(),
            &name.to_lowercase(),
            None,
            None,
            false,
            None,
        )
        .await
        .expect("stage Artist");
    }
    scan.write_genre("genre", "Rock", "rock", "rock", None)
        .await
        .expect("stage Genre");
    scan.write_mood("mood", "Energetic", "energetic", "energetic")
        .await
        .expect("stage Mood");
    scan.write_folder("folder", "Music", "music", "music", None)
        .await
        .expect("stage Folder");
    for (index, album, artist, title, favorite) in [
        (0, "album-a", "artist-a", "Alpha", false),
        (1, "album-a", "artist-a", "Beta", true),
        (2, "album-b", "artist-b", "Gamma", false),
        (3, "album-b", "artist-b", "Delta", false),
    ] {
        let id = format!("track-{index}");
        scan.write_track(
            &id,
            Some(album),
            title,
            &format!(
                "{} {} {} note",
                title.to_lowercase(),
                if album == "album-a" {
                    "album a"
                } else {
                    "album b"
                },
                if artist == "artist-a" {
                    "artist a"
                } else {
                    "artist b"
                }
            ),
            if album == "album-a" {
                "Album A"
            } else {
                "Album B"
            },
            if artist == "artist-a" {
                "Artist A"
            } else {
                "Artist B"
            },
            &title.to_lowercase(),
            180_000 + index * 1_000,
            1,
            index + 1,
            Some(2020 + index),
            None,
            Some("2024-01-02"),
            Some("file:///track.flac"),
            Some("FLAC"),
            Some("Note"),
            Some(100 + index),
            None,
            None,
            None,
            None,
            None,
            None,
            favorite,
            Some(5 + index),
            None,
            None,
            None,
            None,
            None,
            [index as u8 + 1; 32],
        )
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
    scan.write_home_entry(&HomeEntryInput {
        section_id: "featured".to_string(),
        position: 0,
        kind: HomeEntryKind::Track,
        entity_object_id: "track-0".to_string(),
        title: "Featured Track".to_string(),
        subtitle: "Featured".to_string(),
        artwork_binding: None,
    })
    .await
    .expect("stage provider Home entry");
    let ScanOutcome::Changed(publication) = scan.finish().await.expect("publish fixture") else {
        panic!("fixture must publish");
    };
    let mut connection = connection(&path).await;
    let tracks =
        sqlx::query_scalar::<_, TrackKey>("SELECT track_key FROM tracks ORDER BY object_id")
            .fetch_all(&mut connection)
            .await
            .expect("read Track keys");
    let albums =
        sqlx::query_scalar::<_, AlbumKey>("SELECT album_key FROM albums ORDER BY object_id")
            .fetch_all(&mut connection)
            .await
            .expect("read Album keys");
    let artists =
        sqlx::query_scalar::<_, ArtistKey>("SELECT artist_key FROM artists ORDER BY object_id")
            .fetch_all(&mut connection)
            .await
            .expect("read Artist keys");
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
        albums,
        artists,
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
