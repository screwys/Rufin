use std::time::Duration;

use library::{Freshness, Scan};

use super::*;
use crate::source::{SourceReadProgress, SourceReadStage};

const ALBUM_REQUEST_SIZE: usize = 500;
const TRACK_REQUEST_SIZE: usize = 500;
const FRESHNESS_VERSION: u32 = 2;
const METADATA_SCAN_POLL_INTERVAL: Duration = Duration::from_secs(1);
const METADATA_SCAN_MAX_POLLS: usize = 120;

fn incomplete_collection(scan: &mut Scan, error: SourceError) -> SourceResult<()> {
    if matches!(error, SourceError::Cancelled | SourceError::Library(_)) {
        return Err(error);
    }
    tracing::warn!(%error, "OpenSubsonic collection unavailable; continuing catalog acquisition without removals");
    scan.incomplete();
    Ok(())
}

#[derive(Clone, Copy)]
struct ScanWait {
    interval: Duration,
    max_polls: usize,
}

impl SubsonicSource {
    pub(crate) async fn stage_playlist_snapshot(
        &self,
        scan: &mut Scan,
        playlist_id: &str,
    ) -> SourceResult<()> {
        let snapshot = self.read_playlist(playlist_id).await?;
        let artwork = snapshot
            .playlist
            .image_ref
            .as_ref()
            .map(|image| crate::native_artwork_binding(scan.source_id(), image))
            .transpose()?;
        scan.begin_batch().await?;
        scan.write_playlist(
            &snapshot.playlist.id,
            &snapshot.playlist.name,
            &snapshot.playlist.name.to_lowercase(),
            &snapshot.playlist.name.to_lowercase(),
            artwork.as_deref(),
        )
        .await?;
        for (position, entry) in snapshot.entries.iter().enumerate() {
            scan.write_playlist_entry(
                &snapshot.playlist.id,
                &entry.occurrence_id,
                &entry.track_id,
                position as i64,
            )
            .await?;
        }
        scan.finish_batch().await?;
        Ok(())
    }

    pub(crate) async fn home_section(
        &self,
        section: crate::SourceHomeSection,
    ) -> SourceResult<Vec<library::HomeEntryInput>> {
        let (section_id, list_type, extra) = match section {
            crate::SourceHomeSection::MostPlayed => ("most-played", "frequent", Vec::new()),
            crate::SourceHomeSection::NewlyAdded => ("newly-added", "newest", Vec::new()),
            crate::SourceHomeSection::RecentlyPlayed => ("recently-played", "recent", Vec::new()),
            crate::SourceHomeSection::RecentlyReleased => (
                "recently-released",
                "byYear",
                vec![
                    ("fromYear", current_year().to_string()),
                    ("toYear", "0".to_string()),
                ],
            ),
        };
        let mut query = vec![("type", list_type.to_string()), ("size", "24".to_string())];
        query.extend(extra);
        let body: AlbumListBody = self.get_json("getAlbumList2", &query).await?;
        body.album_list
            .album
            .into_iter()
            .enumerate()
            .map(|(position, dto)| {
                let album = album_from_dto(self, dto);
                Ok(library::HomeEntryInput {
                    section_id: section_id.to_string(),
                    position: position as i64,
                    kind: library::HomeEntryKind::Album,
                    entity_object_id: album.id,
                    title: album.title,
                    subtitle: album.artist,
                })
            })
            .collect()
    }
    pub(crate) async fn freshness(&self) -> SourceResult<Option<Freshness>> {
        let body: ScanStatusBody = self.get_json("getScanStatus", &[]).await?;
        let Some(marker) = completed_freshness(body.scan_status) else {
            return Ok(None);
        };
        Ok(Some(Freshness::new(marker)?))
    }

    pub(crate) async fn stage_catalog(
        &self,
        scan: &mut Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        check_cancelled(cancelled)?;
        let music_folders = match self.read_music_folders().await {
            Ok(folders) => folders,
            Err(error) => {
                incomplete_collection(scan, error)?;
                Vec::new()
            }
        };
        scan.begin_batch().await?;
        for folder in &music_folders {
            scan.write_folder(
                &folder.id,
                &folder.name,
                &folder.name.to_lowercase(),
                &folder.name.to_lowercase(),
                None,
            )
            .await?;
        }
        scan.finish_batch().await?;
        check_cancelled(cancelled)?;
        if self.has_navidrome_library() {
            self.stage_navidrome_library(scan, progress, cancelled)
                .await?;
        } else {
            progress(stage(SourceReadStage::Albums, 0));
            if let Err(error) = self.emit_albums(scan, progress, cancelled).await {
                incomplete_collection(scan, error)?;
            }
            progress(stage(SourceReadStage::Tracks, 0));
            if let Err(error) = self
                .emit_tracks(&music_folders, scan, progress, cancelled)
                .await
            {
                incomplete_collection(scan, error)?;
            }
            check_cancelled(cancelled)?;
            progress(stage(SourceReadStage::Artists, 0));
            if let Err(error) = self.stage_artists(scan, progress, cancelled).await {
                incomplete_collection(scan, error)?;
            }
        }

        check_cancelled(cancelled)?;
        progress(stage(SourceReadStage::Genres, 0));
        let genres = match self.read_genres().await {
            Ok(genres) => genres,
            Err(error) => {
                crate::source::optional_collection_error(error)?;
                scan.retain_genres().await?;
                Vec::new()
            }
        };
        scan.begin_batch().await?;
        for genre in genres {
            stage_genre(scan, genre).await?;
        }
        scan.finish_batch().await?;

        check_cancelled(cancelled)?;
        progress(stage(SourceReadStage::Playlists, 0));
        if let Err(error) = self.emit_playlists(scan, progress, cancelled).await {
            crate::source::optional_collection_error(error)?;
            scan.retain_playlists().await?;
        }
        self.stage_home(scan).await?;

        check_cancelled(cancelled)?;
        progress(stage(SourceReadStage::Finalizing, 0));
        Ok(())
    }

    pub(super) async fn emit_albums(
        &self,
        scan: &mut Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let mut offset = 0_usize;
        loop {
            check_cancelled(cancelled)?;
            let body: AlbumListBody = self
                .get_json(
                    "getAlbumList2",
                    &[
                        ("type", "alphabeticalByName".to_string()),
                        ("size", ALBUM_REQUEST_SIZE.to_string()),
                        ("offset", offset.to_string()),
                    ],
                )
                .await?;
            let page = body.album_list.album;
            if page.is_empty() {
                return Ok(());
            }
            let page_len = page.len();
            offset = offset.checked_add(page.len()).ok_or_else(|| {
                SourceError::Other("OpenSubsonic album offset overflowed".to_string())
            })?;
            scan.begin_batch().await?;
            for album in page {
                stage_album(scan, album_from_dto(self, album)).await?;
            }
            scan.finish_batch().await?;
            progress(stage(SourceReadStage::Albums, offset));
            if page_len < ALBUM_REQUEST_SIZE {
                return Ok(());
            }
        }
    }

    pub(super) async fn emit_tracks(
        &self,
        music_folders: &[MusicFolder],
        scan: &mut Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let scopes = if music_folders.is_empty() {
            vec![None]
        } else {
            music_folders.iter().map(Some).collect()
        };
        let mut emitted = 0;
        for folder in scopes {
            let mut offset = 0_usize;
            loop {
                check_cancelled(cancelled)?;
                let mut extra = vec![
                    ("query", String::new()),
                    ("artistCount", "0".to_string()),
                    ("artistOffset", "0".to_string()),
                    ("albumCount", "0".to_string()),
                    ("albumOffset", "0".to_string()),
                    ("songCount", TRACK_REQUEST_SIZE.to_string()),
                    ("songOffset", offset.to_string()),
                ];
                if let Some(folder) = folder {
                    extra.push(("musicFolderId", raw_item_id(folder.id.as_str()).to_string()));
                }
                let body: SearchBody = self.get_json("search3", &extra).await?;
                let page = body
                    .search_result
                    .and_then(|result| result.song)
                    .unwrap_or_default();
                if page.is_empty() {
                    break;
                }
                offset = offset.checked_add(page.len()).ok_or_else(|| {
                    SourceError::Other("OpenSubsonic track offset overflowed".to_string())
                })?;
                scan.begin_batch().await?;
                let mut folder_links = Vec::new();
                for song in page {
                    let track = track_from_dto(self, song);
                    if let Some(folder) = folder {
                        folder_links.push((track.id.clone(), folder.id.clone()));
                    }
                    if stage_track(scan, track).await? {
                        emitted += 1;
                    }
                }
                scan.write_track_folders(
                    &folder_links
                        .iter()
                        .map(|(track, folder)| library::ScanLink::new(track, folder, 0))
                        .collect::<Vec<_>>(),
                )
                .await?;
                scan.finish_batch().await?;
                progress(stage(SourceReadStage::Tracks, emitted));
            }
        }
        Ok(())
    }

    async fn read_music_folders(&self) -> SourceResult<Vec<MusicFolder>> {
        let body: MusicFoldersBody = self.get_json("getMusicFolders", &[]).await?;
        Ok(body
            .music_folders
            .music_folder
            .into_iter()
            .map(|folder| MusicFolder {
                id: String::from(self.id("music-folder", &folder.id.0)),
                name: folder.name,
                image_ref: None,
            })
            .collect())
    }

    async fn read_genres(&self) -> SourceResult<Vec<Genre>> {
        let body: GenresBody = self.get_json("getGenres", &[]).await?;
        Ok(body
            .genres
            .genre
            .into_iter()
            .map(|genre| genre_from_dto(self, genre))
            .collect())
    }

    async fn emit_playlists(
        &self,
        scan: &mut Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let body: PlaylistsBody = self.get_json("getPlaylists", &[]).await?;
        let playlists = body
            .playlists
            .map(|playlists| playlists.playlist)
            .unwrap_or_default();
        let total = playlists.len();
        for (position, playlist) in playlists.into_iter().enumerate() {
            check_cancelled(cancelled)?;
            let id = String::from(self.id("playlist", &raw_id_string(&playlist.id)));
            self.stage_playlist_snapshot(scan, &id).await?;
            progress(SourceReadProgress {
                stage: SourceReadStage::Playlists,
                completed: position + 1,
                total: Some(total),
            });
        }
        Ok(())
    }

    async fn stage_artists(
        &self,
        scan: &mut Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let mut offset = 0_usize;
        loop {
            check_cancelled(cancelled)?;
            let body: SearchBody = self
                .get_json(
                    "search3",
                    &[
                        ("query", String::new()),
                        ("artistCount", ALBUM_REQUEST_SIZE.to_string()),
                        ("artistOffset", offset.to_string()),
                        ("albumCount", "0".to_string()),
                        ("albumOffset", "0".to_string()),
                        ("songCount", "0".to_string()),
                        ("songOffset", "0".to_string()),
                    ],
                )
                .await?;
            let page = body
                .search_result
                .and_then(|result| result.artist)
                .unwrap_or_default();
            if page.is_empty() {
                return Ok(());
            }
            offset += page.len();
            let finished = page.len() < ALBUM_REQUEST_SIZE;
            scan.begin_batch().await?;
            for artist in page {
                stage_artist(scan, artist_from_dto(self, artist)).await?;
            }
            scan.finish_batch().await?;
            progress(stage(SourceReadStage::Artists, offset));
            if finished {
                return Ok(());
            }
        }
    }

    async fn stage_home(&self, scan: &mut Scan) -> SourceResult<()> {
        for section in [
            crate::SourceHomeSection::MostPlayed,
            crate::SourceHomeSection::NewlyAdded,
            crate::SourceHomeSection::RecentlyPlayed,
            crate::SourceHomeSection::RecentlyReleased,
        ] {
            let entries = match self.home_section(section).await {
                Ok(entries) => entries,
                Err(error) => {
                    crate::source::optional_collection_error(error)?;
                    scan.retain_home_section(section.id()).await?;
                    continue;
                }
            };
            scan.begin_batch().await?;
            for entry in entries {
                scan.write_home_entry(&entry).await?;
            }
            scan.finish_batch().await?;
        }
        Ok(())
    }

    pub(crate) async fn require_metadata_scan_idle(&self) -> SourceResult<()> {
        let body: ScanStatusBody = self.get_json("getScanStatus", &[]).await?;
        if body.scan_status.scanning {
            return Err(SourceError::Other(
                "The server is already scanning its library. Wait for it to finish before editing metadata."
                    .to_string(),
            ));
        }
        Ok(())
    }

    pub(crate) async fn start_metadata_scan_and_wait(&self) -> SourceResult<()> {
        self.start_metadata_scan_with_wait(ScanWait {
            interval: METADATA_SCAN_POLL_INTERVAL,
            max_polls: METADATA_SCAN_MAX_POLLS,
        })
        .await
    }

    async fn start_metadata_scan_with_wait(&self, wait: ScanWait) -> SourceResult<()> {
        let body: ScanStatusBody = self.get_json("startScan", &[]).await?;
        let mut status = body.scan_status;
        for poll in 0..=wait.max_polls {
            if !status.scanning {
                return scan_finished(status);
            }
            if poll == wait.max_polls {
                break;
            }
            if !wait.interval.is_zero() {
                tokio::time::sleep(wait.interval).await;
            }
            let body: ScanStatusBody = self.get_json("getScanStatus", &[]).await?;
            status = body.scan_status;
        }
        Err(SourceError::Other(
            "The server library scan did not finish within two minutes.".to_string(),
        ))
    }
}

fn scan_finished(status: ScanStatus) -> SourceResult<()> {
    if let Some(error) = status.error.filter(|error| !error.trim().is_empty()) {
        Err(SourceError::Other(format!(
            "The server library scan failed: {error}"
        )))
    } else {
        Ok(())
    }
}

fn freshness(status: ScanStatus) -> Vec<u8> {
    let mut marker = FRESHNESS_VERSION.to_le_bytes().to_vec();
    marker.extend_from_slice(&status.count.to_le_bytes());
    marker.extend_from_slice(&status.folder_count.unwrap_or_default().to_le_bytes());
    if let Some(last_scan) = status.last_scan {
        marker.extend_from_slice(&(last_scan.len() as u64).to_le_bytes());
        marker.extend_from_slice(last_scan.as_bytes());
    } else {
        marker.extend_from_slice(&0_u64.to_le_bytes());
    }
    marker
}

fn completed_freshness(status: ScanStatus) -> Option<Vec<u8>> {
    (!status.scanning).then(|| freshness(status))
}

fn stage(stage: SourceReadStage, completed: usize) -> SourceReadProgress {
    SourceReadProgress {
        stage,
        completed,
        total: None,
    }
}

fn check_cancelled(cancelled: &(dyn Fn() -> bool + Send + Sync)) -> SourceResult<()> {
    if cancelled() {
        Err(SourceError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ScanStatus, completed_freshness};
    use crate::subsonic::{
        SubsonicAuthentication, SubsonicFlavor, SubsonicSource, SubsonicSourceConfig,
    };
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn status(count: i64, last_scan: &str) -> ScanStatus {
        ScanStatus {
            scanning: false,
            count,
            folder_count: Some(3),
            last_scan: Some(last_scan.to_string()),
            error: None,
        }
    }

    #[test]
    fn completed_scan_marker_changes_only_with_provider_freshness() {
        let accepted = completed_freshness(status(40, "2026-08-27T00:00:00Z"));
        assert_eq!(
            completed_freshness(status(40, "2026-08-27T00:00:00Z")),
            accepted
        );
        assert_ne!(
            completed_freshness(status(41, "2026-08-27T00:00:00Z")),
            accepted
        );
        assert_ne!(
            completed_freshness(status(40, "2026-08-27T00:01:00Z")),
            accepted
        );
        let mut scanning = status(41, "2026-08-27T00:01:00Z");
        scanning.scanning = true;
        assert_eq!(completed_freshness(scanning), None);
    }

    #[tokio::test]
    async fn catalog_and_home_publish_when_optional_collections_are_unavailable() {
        for folders_available in [false, true] {
            let server = MockServer::start().await;
            if folders_available {
                Mock::given(method("GET"))
                    .and(path("/rest/getMusicFolders.view"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "subsonic-response": { "status": "ok", "musicFolders": { "musicFolder": [
                            { "id": "one", "name": "One" }, { "id": "two", "name": "Two" }
                        ] } }
                    })))
                    .mount(&server)
                    .await;
            }
            Mock::given(method("GET"))
                .and(path("/rest/getAlbumList2.view"))
                .and(query_param("type", "alphabeticalByName"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "subsonic-response": { "status": "ok", "albumList2": { "album": [
                        { "id": "album", "name": "Album", "artist": "Artist" }
                    ] } }
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET")).and(path("/rest/search3.view"))
                .respond_with(|request: &wiremock::Request| {
                    let query = request.url.query_pairs().collect::<std::collections::HashMap<_, _>>();
                    let songs = if query.get("songCount").is_some_and(|count| count == "500")
                        && query.get("songOffset").is_some_and(|offset| offset == "0") {
                        serde_json::json!([{ "id": "track", "title": "Track", "album": "Album", "albumId": "album", "artist": "Artist", "duration": 42 }])
                    } else { serde_json::json!([]) };
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "subsonic-response": { "status": "ok", "searchResult3": { "song": songs, "artist": [] } }
                    }))
                }).mount(&server).await;
            let source = SubsonicSource::open(
                SubsonicFlavor::Subsonic,
                SubsonicSourceConfig {
                    base_url: server.uri(),
                    username: "listener".into(),
                    trust_invalid_cert: false,
                    navidrome_library_version: 0,
                    authentication: SubsonicAuthentication::LegacyPassword,
                },
                super::SubsonicCredential::LegacyPassword("api-password".into()).serialize(),
            )
            .unwrap();
            let directory = tempfile::tempdir().unwrap();
            let database = library::Database::open(directory.path().join("library.sqlite"))
                .await
                .unwrap();
            let marker = library::Freshness::new(b"server-observation".to_vec()).unwrap();
            if folders_available {
                let mut previous = library::Scan::begin(
                    &database,
                    "source",
                    "Nextcloud Music",
                    "nextcloud music",
                    None,
                )
                .await
                .unwrap();
                super::stage_album(
                    &mut previous,
                    super::album_from_dto(
                        &source,
                        serde_json::from_value(
                            serde_json::json!({"id":"album","name":"Album","artist":"Artist"}),
                        )
                        .unwrap(),
                    ),
                )
                .await
                .unwrap();
                super::stage_track(&mut previous, super::track_from_dto(&source, serde_json::from_value(serde_json::json!({"id":"removed","title":"Vanished","albumId":"album"})).unwrap())).await.unwrap();
                previous
                    .write_genre("cached-genre", "Cached", "cached", "cached", None)
                    .await
                    .unwrap();
                previous
                    .write_playlist("cached-playlist", "Cached", "cached", "cached", None)
                    .await
                    .unwrap();
                previous
                    .write_playlist_entry(
                        "cached-playlist",
                        "occurrence",
                        "subsonic:track:removed",
                        0,
                    )
                    .await
                    .unwrap();
                previous
                    .write_home_entry(&library::HomeEntryInput {
                        section_id: "recently-played".into(),
                        position: 0,
                        kind: library::HomeEntryKind::Album,
                        entity_object_id: "subsonic:album:album".into(),
                        title: "Album".into(),
                        subtitle: "Artist".into(),
                    })
                    .await
                    .unwrap();
                previous.finish().await.unwrap();
            }
            let progress = std::sync::Mutex::new(Vec::new());
            let mut scan = library::Scan::begin(
                &database,
                "source",
                "Nextcloud Music",
                "nextcloud music",
                Some(marker.clone()),
            )
            .await
            .unwrap();
            source
                .stage_catalog(
                    &mut scan,
                    &|value| {
                        progress.lock().unwrap().push(value);
                    },
                    &|| false,
                )
                .await
                .unwrap();
            let library::ScanOutcome::Changed(publication) = scan.finish().await.unwrap() else {
                panic!("first catalog must publish");
            };
            let cancellation = library::ReadCancellation::new();
            let home = database
                .home_page(publication.source, None, 0, 0, &cancellation)
                .await
                .unwrap();
            assert_eq!(home.newly_added.albums.len(), 1);
            assert_eq!(home.explore.len(), 1);
            assert_eq!(
                progress
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|progress| progress.stage == crate::SourceReadStage::Tracks)
                    .map(|progress| progress.completed)
                    .max(),
                Some(1)
            );
            assert_eq!(
                library::Scan::accept_freshness(&database, "source", &marker, &cancellation)
                    .await
                    .unwrap()
                    .is_some(),
                folders_available
            );
            if folders_available {
                assert!(
                    database
                        .track_row_by_uri(
                            &library::source_entity_uri(
                                &crate::SourceId::new("source"),
                                "track",
                                "subsonic:track:removed"
                            ),
                            &cancellation
                        )
                        .await
                        .unwrap()
                        .is_none()
                );
                assert!(
                    database
                        .genre_key_by_object(publication.source, "cached-genre", &cancellation)
                        .await
                        .unwrap()
                        .is_some()
                );
                assert!(
                    database
                        .playlist_key_by_object(
                            publication.source,
                            "cached-playlist",
                            &cancellation
                        )
                        .await
                        .unwrap()
                        .is_some()
                );
                assert_eq!(home.recently_played.albums.len(), 1);
                for id in ["one", "two"] {
                    let folder = database
                        .folder_key_by_object(
                            publication.source,
                            &source.id("music-folder", id),
                            &cancellation,
                        )
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        database
                            .home_page(publication.source, Some(folder), 0, 0, &cancellation)
                            .await
                            .unwrap()
                            .explore
                            .len(),
                        1
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn newly_added_home_keeps_the_opensubsonic_provider_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/getAlbumList2.view"))
            .and(query_param("type", "newest"))
            .and(query_param("size", "24"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response": {
                    "status": "ok",
                    "albumList2": {
                        "album": [{ "id": "album-one", "name": "New Album" }]
                    }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let source = SubsonicSource::open(
            SubsonicFlavor::Subsonic,
            SubsonicSourceConfig {
                base_url: server.uri(),
                username: "listener".to_string(),
                trust_invalid_cert: false,
                navidrome_library_version: 0,
                authentication: SubsonicAuthentication::Password,
            },
            "salt:token".to_string(),
        )
        .expect("OpenSubsonic source");

        let entries = source
            .home_section(crate::SourceHomeSection::NewlyAdded)
            .await
            .expect("OpenSubsonic Newly Added");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].section_id, "newly-added");
        assert_eq!(entries[0].title, "New Album");
    }
}
