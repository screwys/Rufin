use std::io::{BufRead, Write};
use std::path::Path;

use futures_util::TryStreamExt;
use sqlx::Connection;

use crate::{
    Database, LibraryError, LibraryResult, PlaylistEntryWrite, PlaylistIdentity, PlaylistKey,
    ReadCancellation,
};

#[derive(Debug)]
pub struct PlaylistImportReport {
    pub playlist: PlaylistKey,
    pub imported: u64,
    pub skipped: u64,
}

impl Database {
    pub async fn import_playlist_m3u(
        &self,
        input: impl BufRead,
        file: &Path,
        recognize: impl Fn(&str) -> Option<String>,
    ) -> LibraryResult<PlaylistImportReport> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let report = import_playlist_m3u_on(&mut transaction, input, file, recognize).await?;
        transaction.commit().await?;
        Ok(report)
    }

    pub async fn export_playlist_m3u(
        &self,
        playlist: PlaylistKey,
        file: &Path,
        mut output: impl Write,
    ) -> LibraryResult<u64> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        export_playlist_m3u_on(&mut connection, playlist, file, &mut output).await
    }
}

pub(crate) async fn import_playlist_m3u_on(
    connection: &mut sqlx::SqliteConnection,
    input: impl BufRead,
    file: &Path,
    recognize: impl Fn(&str) -> Option<String>,
) -> LibraryResult<PlaylistImportReport> {
    let object_id: String =
        sqlx::query_scalar("SELECT 'rufin:playlist:' || lower(hex(randomblob(16)))")
            .fetch_one(&mut *connection)
            .await?;
    let mut identity = PlaylistIdentity {
        source_id: None,
        object_id,
        name: Some(
            file.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        position: 0,
    };
    let mut playlist = None;
    let mut imported = 0;
    let mut skipped = 0;
    let mut info = None;
    let mut preserved: Option<PlaylistEntryWrite> = None;
    for line in input.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                skipped += 1;
                info = None;
                preserved = None;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let line = line.trim_start_matches('\u{feff}').trim();
        if let Some(value) = line.strip_prefix("#RUFIN-PLAYLIST:1:") {
            if playlist.is_none() {
                if let Ok(value) = serde_json::from_str::<PlaylistIdentity>(value) {
                    if value.name.is_some() && !value.object_id.is_empty() && value.position >= 0 {
                        let native = sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM main.playlists playlist JOIN main.source_ids source USING(source_key) WHERE source.object_id=?1 AND playlist.object_id=?2 AND playlist.name IS NULL) OR EXISTS(SELECT 1 FROM catalog.native_playlists playlist JOIN catalog.sources source USING(source_key) WHERE source.object_id=?1 AND playlist.object_id=?2)")
                            .bind(&value.source_id).bind(&value.object_id).fetch_one(&mut *connection).await?;
                        if !native {
                            identity = value;
                        }
                    }
                }
            }
        } else if let Some(value) = line.strip_prefix("#PLAYLIST:") {
            if playlist.is_none() {
                identity.name = Some(value.to_owned());
            }
        } else if let Some(value) = line.strip_prefix("#RUFIN-MEDIA:1:") {
            preserved = serde_json::from_str(value).ok();
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            info = Some(value.to_owned());
        } else if !line.is_empty() && !line.starts_with('#') {
            let locator = recognize(line).unwrap_or_else(|| line.to_owned());
            let Some(uri) = m3u_locator(&locator, file.parent().unwrap_or_else(|| Path::new(".")))
            else {
                skipped += 1;
                info = None;
                preserved = None;
                continue;
            };
            let key = match playlist {
                Some(key) => key,
                None => {
                    let key =
                        crate::playlists::write_playlist_identity(connection, &identity).await?;
                    sqlx::query("DELETE FROM main.playlist_entries WHERE playlist_key=?1")
                        .bind(key)
                        .execute(&mut *connection)
                        .await?;
                    playlist = Some(key);
                    key
                }
            };
            let mut entry = preserved
                .take()
                .filter(|entry| m3u_locator(&entry.media_uri, Path::new("/")).is_some())
                .unwrap_or_else(|| PlaylistEntryWrite {
                    object_id: format!("m3u:{imported}"),
                    media_uri: uri.clone(),
                    title: None,
                    artist: None,
                    album: None,
                    album_display_artist: None,
                    snapshot_at: 0,
                    duration_millis: None,
                    disc_number: None,
                    track_number: None,
                    year: None,
                    release_date: None,
                    source_format: None,
                    musicbrainz_recording_id: None,
                    musicbrainz_release_track_id: None,
                    position: imported as i64,
                });
            if crate::source_entity_parts(&entry.media_uri).is_none()
                && crate::cue_media_parts(&entry.media_uri).is_none()
            {
                entry.media_uri = uri;
            }
            if let Some(info) = info.take() {
                if let Some((duration, title)) = info.split_once(',') {
                    entry.duration_millis = entry.duration_millis.or_else(|| {
                        duration
                            .parse::<f64>()
                            .ok()
                            .filter(|value| value.is_finite() && *value >= 0.0)
                            .map(|value| (value * 1000.0) as i64)
                    });
                    if entry.title.is_none() {
                        if let Some((artist, title)) = title.split_once(" - ") {
                            entry.artist = Some(artist.to_owned());
                            entry.title = Some(title.to_owned());
                        } else {
                            entry.title = Some(title.to_owned());
                        }
                    }
                }
            }
            if entry.object_id.is_empty() {
                entry.object_id = format!("m3u:{imported}");
            }
            if sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM main.playlist_entries WHERE playlist_key=?1 AND object_id=?2)").bind(key).bind(&entry.object_id).fetch_one(&mut *connection).await? {
                    entry.object_id = sqlx::query_scalar("SELECT 'm3u:' || lower(hex(randomblob(16)))").fetch_one(&mut *connection).await?;
                }
            entry.position = imported as i64;
            crate::playlists::write_playlist_entry(connection, key, &entry).await?;
            imported += 1;
        }
    }
    let playlist = match playlist {
        Some(key) => key,
        None => {
            let key = crate::playlists::write_playlist_identity(connection, &identity).await?;
            sqlx::query("DELETE FROM main.playlist_entries WHERE playlist_key=?1")
                .bind(key)
                .execute(&mut *connection)
                .await?;
            key
        }
    };
    Ok(PlaylistImportReport {
        playlist,
        imported,
        skipped,
    })
}

pub(crate) async fn export_playlist_m3u_on(
    connection: &mut sqlx::SqliteConnection,
    playlist: PlaylistKey,
    file: &Path,
    mut output: impl Write,
) -> LibraryResult<u64> {
    let mut identity = sqlx::query_as::<_, PlaylistIdentity>(if playlist.raw() >= 0 {
        "SELECT source.object_id source_id,playlist.object_id,playlist.name,playlist.position FROM main.playlists playlist LEFT JOIN main.source_ids source USING(source_key) WHERE playlist.playlist_key=?1"
    } else {
        "SELECT source.object_id source_id,playlist.object_id,playlist.name,playlist.position FROM playlists playlist LEFT JOIN catalog.sources source USING(source_key) WHERE playlist.playlist_key=?1"
    })
            .bind(playlist).fetch_one(&mut *connection).await?;
    writeln!(
        output,
        "#EXTM3U\n#PLAYLIST:{}",
        single_line(identity.name.as_deref().unwrap_or_default())
    )?;
    if playlist.raw() < 0 {
        identity.name = None;
    }
    write!(output, "#RUFIN-PLAYLIST:1:")?;
    serde_json::to_writer(&mut output, &identity)?;
    writeln!(output)?;
    let mut entries = sqlx::query_as::<_, PlaylistEntryWrite>("SELECT object_id,media_uri,title,artist,album,album_display_artist,snapshot_at,duration_millis,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,position FROM playlist_entries WHERE playlist_key=?1 ORDER BY position")
            .bind(playlist).fetch(&mut *connection);
    let mut count = 0;
    while let Some(entry) = entries.try_next().await? {
        if write_m3u_entry(&mut output, file, &entry)? {
            count += 1;
        }
    }
    Ok(count)
}

pub(crate) fn write_m3u_entry(
    output: &mut impl Write,
    file: &Path,
    entry: &PlaylistEntryWrite,
) -> LibraryResult<bool> {
    if entry.media_uri.starts_with("local:")
        || m3u_locator(&entry.media_uri, Path::new("/")).is_none()
    {
        return Ok(false);
    }
    write!(output, "#RUFIN-MEDIA:1:")?;
    serde_json::to_writer(&mut *output, entry)?;
    writeln!(output)?;
    let display = match (&entry.artist, &entry.title) {
        (Some(artist), Some(title)) => format!("{artist} - {title}"),
        (_, title) => title.clone().unwrap_or_default(),
    };
    writeln!(
        output,
        "#EXTINF:{},{}",
        entry
            .duration_millis
            .map(|value| value as f64 / 1000.0)
            .unwrap_or(-1.0),
        single_line(&display)
    )?;
    let locator = crate::file_media_path(&entry.media_uri)
        .and_then(|path| {
            path.strip_prefix(file.parent().unwrap_or_else(|| Path::new(".")))
                .ok()
                .map(|path| path.to_string_lossy().into_owned())
        })
        .filter(|path| !path.starts_with('#') && !path.contains(['\r', '\n']))
        .unwrap_or_else(|| entry.media_uri.clone());
    writeln!(output, "{}", single_line(&locator))?;
    Ok(true)
}

fn single_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

fn m3u_locator(value: &str, base: &Path) -> Option<String> {
    if Path::new(value).is_absolute() {
        return url::Url::from_file_path(value)
            .ok()
            .map(|uri| uri.to_string());
    }
    if crate::source_entity_parts(value).is_some() {
        return Some(value.to_owned());
    }
    if let Some((_, backing, _, _)) = crate::cue_media_parts(value) {
        return crate::file_media_path(&backing).map(|_| value.to_owned());
    }
    if let Ok(uri) = url::Url::parse(value) {
        if !uri.username().is_empty()
            || uri.password().is_some()
            || uri.query_pairs().any(|(key, _)| {
                matches!(
                    key.to_ascii_lowercase().as_str(),
                    "api_key"
                        | "apikey"
                        | "access_token"
                        | "token"
                        | "password"
                        | "authorization"
                        | "x-emby-token"
                ) || (uri.path().contains("/rest/") && matches!(key.as_ref(), "p" | "t" | "s"))
            })
        {
            return None;
        }
        return crate::normalize_direct_media_uri(value).or_else(|| Some(uri.to_string()));
    }
    if value.contains(':') && !Path::new(value).is_absolute() {
        return None;
    }
    let path = Path::new(value);
    url::Url::from_file_path(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    })
    .ok()
    .map(|uri| uri.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn smart_export_streams_large_direct_http_results_without_current() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let file = directory.path().join("large.m3u8");
        {
            let mut input = std::io::BufWriter::new(std::fs::File::create(&file).unwrap());
            for index in 0..10001 {
                writeln!(
                    input,
                    "#EXTINF:12,Song {index}\nhttps://example.test/{index}"
                )
                .unwrap();
            }
        }
        database
            .import_playlist_m3u(
                std::io::BufReader::new(std::fs::File::open(&file).unwrap()),
                &file,
                |_| None,
            )
            .await
            .unwrap();
        let key = database
            .create_smart_playlist("All", &crate::SmartPlaylistDefinition::default())
            .await
            .unwrap();
        let mut output = tempfile::tempfile().unwrap();
        assert_eq!(
            database
                .export_smart_playlist_m3u(key, None, None, 0, &file, &mut output)
                .await
                .unwrap(),
            10001
        );
    }

    #[tokio::test]
    async fn native_export_imports_as_an_authored_copy_without_combining_entries() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        {
            let mut writer = database.writer().await.unwrap();
            let connection = writer.as_mut().unwrap();
            sqlx::raw_sql("INSERT INTO catalog.sources(source_key,object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES(5,'source','Source','source',zeroblob(32),zeroblob(32));
              INSERT INTO catalog.native_playlists(playlist_key,source_key,object_id,name,normalized_name,sort_text) VALUES(8,5,'native','Native','native','native');
              INSERT INTO catalog.native_playlist_entries(playlist_key,object_id,media_uri,title,position) VALUES(8,'one','https://example.test/song','Song',0),(8,'two','https://example.test/song','Song',1);")
              .execute(connection).await.unwrap();
        }
        let file = directory.path().join("native.m3u8");
        let mut output = Vec::new();
        database
            .export_playlist_m3u(PlaylistKey::from_raw(-8), &file, &mut output)
            .await
            .unwrap();
        let copy = database
            .import_playlist_m3u(std::io::Cursor::new(output), &file, |_| None)
            .await
            .unwrap();
        assert_eq!(copy.imported, 2);
        assert_eq!(
            database
                .playlist_owner(copy.playlist, &crate::ReadCancellation::new())
                .await
                .unwrap(),
            Some((None, None))
        );
        let mut copied = Vec::new();
        assert_eq!(
            database
                .export_playlist_m3u(copy.playlist, &file, &mut copied)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            String::from_utf8(copied)
                .unwrap()
                .matches("\nhttps://example.test/song\n")
                .count(),
            2
        );
        let mut native = Vec::new();
        assert_eq!(
            database
                .export_playlist_m3u(PlaylistKey::from_raw(-8), &file, &mut native)
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn ordinary_m3u_preserves_unicode_duplicates_missing_paths_and_exact_reimport() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let file = directory.path().join("mix.m3u8");
        let report = database.import_playlist_m3u(std::io::Cursor::new("#EXTM3U\n#PLAYLIST:Björk\n#EXTINF:12.5,Björk - Jóga\nmissing.flac\nmissing.flac\nhttps://example.test/song\nhttps://user:secret@example.test/song\nhttps:// bad\n"), &file, |_| None).await.unwrap();
        assert_eq!((report.imported, report.skipped), (3, 2));
        let mut output = Vec::new();
        assert_eq!(
            database
                .export_playlist_m3u(report.playlist, &file, &mut output)
                .await
                .unwrap(),
            3
        );
        let text = String::from_utf8(output.clone()).unwrap();
        assert!(text.contains("#PLAYLIST:Björk"));
        assert!(text.contains("\nmissing.flac\n"));
        assert!(!text.contains("secret"));
        let again = database
            .import_playlist_m3u(std::io::Cursor::new(output), &file, |_| None)
            .await
            .unwrap();
        assert_eq!(again.playlist, report.playlist);
        assert_eq!(again.imported, 3);
    }
}
