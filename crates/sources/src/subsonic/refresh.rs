use std::time::Duration;

use library::Freshness;

use super::*;
use crate::remote_json as json;
use crate::source::{SourceReadProgress, SourceReadStage};
use serde_json::Value;

const ALBUM_REQUEST_SIZE: usize = 500;
const TRACK_REQUEST_SIZE: usize = 500;
const FRESHNESS_VERSION: u32 = 3;
const METADATA_SCAN_POLL_INTERVAL: Duration = Duration::from_secs(1);
const METADATA_SCAN_MAX_POLLS: usize = 120;

#[derive(Clone, Debug)]
struct ScanStatus {
    scanning: bool,
    count: i64,
    folder_count: Option<i64>,
    last_scan: Option<String>,
    error: Option<String>,
}

impl ScanStatus {
    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            scanning: json::boolean(&value["scanning"])?,
            count: json::field(value, "count").unwrap_or_default(),
            folder_count: json::field(value, "folderCount"),
            last_scan: json::field(value, "lastScan"),
            error: json::field(value, "error"),
        })
    }
}

impl SubsonicSource {
    async fn acquire_favorites(
        &self,
        observation: &mut library::Observation<'_>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let body = self.get_json("getStarred2", &[]).await?;
        let starred = body
            .get("starred2")
            .filter(|value| value.is_object())
            .ok_or_else(|| {
                SourceError::Other("Subsonic favorites response is unavailable".into())
            })?;
        let ids = |field, kind| {
            json::items(&starred[field])
                .iter()
                .filter_map(|item| json::id(&item["id"]))
                .map(|id| self.id(kind, &id))
                .collect::<Vec<_>>()
        };

        check_cancelled(cancelled)?;
        for (field, kind) in [("song", "track"), ("album", "album"), ("artist", "artist")] {
            observation
                .replace_source_favorites(kind, &ids(field, kind))
                .await?;
        }

        Ok(())
    }

    pub(crate) async fn acquire_user_collections(
        &self,
        observation: &mut library::Observation<'_>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let stamp = observation.observed;

        let stamp = Some(match stamp {
            Some(stamp) => stamp,
            None => observation.start_observation().await?,
        });
        if let Err(error) = self.acquire_favorites(observation, cancelled).await {
            crate::source::optional_collection_error(error)?;
        }
        check_cancelled(cancelled)?;
        progress(stage(SourceReadStage::Playlists, 0));
        match self
            .acquire_playlists(observation, progress, cancelled)
            .await
        {
            Ok(()) => {
                if stamp.is_some() {
                    check_cancelled(cancelled)?;
                    observation
                        .complete_source_observation(&["playlist"], None)
                        .await?;
                }
            }
            Err(error) => {
                crate::source::optional_collection_error(error)?;
            }
        }
        crate::source::acquire_home(observation, cancelled, |section| self.home_section(section))
            .await?;
        check_cancelled(cancelled)?;
        Ok(())
    }

    pub(crate) async fn acquire_playlist_snapshot(
        &self,
        observation: &mut library::Observation<'_>,
        playlist_id: &str,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = observation.source_id;

        let snapshot = self.read_playlist(source_id, playlist_id).await?;
        let mut batch = library::CatalogBatch::default();
        batch.playlists.push(snapshot.playlist.header.clone());
        check_cancelled(cancelled)?;
        observation.apply_catalog(&batch, true).await?;
        observation
            .write_source_playlist_page(
                &snapshot.playlist.header.object_id,
                0,
                &snapshot.entries,
                true,
            )
            .await?;
        observation
            .write_source_playlist_freshness(
                &snapshot.playlist.header.object_id,
                snapshot.playlist.revision.as_deref(),
                snapshot.playlist.valid_until,
            )
            .await?;

        Ok(())
    }

    pub(crate) async fn home_section(
        &self,
        section: crate::SourceHomeSection,
    ) -> SourceResult<Vec<library::HomeEntryInput>> {
        let (list_type, extra) = match section {
            crate::SourceHomeSection::MostPlayed => ("frequent", Vec::new()),
            crate::SourceHomeSection::NewlyAdded => ("newest", Vec::new()),
            crate::SourceHomeSection::RecentlyPlayed => ("recent", Vec::new()),
            crate::SourceHomeSection::RecentlyReleased => (
                "byYear",
                vec![
                    ("fromYear", current_year().to_string()),
                    ("toYear", "0".to_string()),
                ],
            ),
        };
        let mut query = vec![("type", list_type.to_string()), ("size", "24".to_string())];
        query.extend(extra);
        let body: Value = self.get_json("getAlbumList2", &query).await?;
        json::items(&body["albumList2"]["album"])
            .iter()
            .take(24)
            .filter_map(|album| Some((self.id("album", &json::id(&album["id"])?), album)))
            .enumerate()
            .map(|(position, (object_id, album))| {
                Ok(library::HomeEntryInput {
                    position: position as i64,
                    kind: library::HomeEntryKind::Album,
                    entity_object_id: object_id,
                    title: album_title(album),
                    subtitle: display_artist(album),
                })
            })
            .collect()
    }
    pub(crate) async fn freshness(&self) -> SourceResult<Option<Freshness>> {
        let body = self.get_json("getScanStatus", &[]).await?;
        let Some(marker) = ScanStatus::from_json(&body["scanStatus"]).and_then(completed_freshness)
        else {
            return Ok(None);
        };
        Ok(Some(Freshness::new(marker)?))
    }

    pub(crate) async fn acquire_catalog(
        &self,
        observation: &mut library::Observation<'_>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<bool> {
        let source_id = observation.source_id;
        let stamp = observation.observed;

        check_cancelled(cancelled)?;
        let mut complete = true;
        let music_folders = match self.read_music_folders().await {
            Ok(folders) => folders,
            Err(error) => {
                complete = false;
                crate::source::optional_collection_error(error)?;
                Vec::new()
            }
        };
        let mut batch = library::CatalogBatch::default();
        batch.folders.extend_from_slice(&music_folders);
        check_cancelled(cancelled)?;
        observation.apply_catalog(&batch, true).await?;
        check_cancelled(cancelled)?;
        if self.has_navidrome_library() {
            self.acquire_navidrome_library(observation, progress, cancelled)
                .await?;
        } else {
            for (entity, read_stage) in [
                ("album", SourceReadStage::Albums),
                ("track", SourceReadStage::Tracks),
                ("artist", SourceReadStage::Artists),
            ] {
                progress(stage(read_stage, 0));
                let result = async {
                    if entity == "track" {
                        return self
                            .acquire_tracks(observation, &music_folders, progress, cancelled)
                            .await;
                    }
                    let mut offset = 0_usize;
                    loop {
                        check_cancelled(cancelled)?;
                        let body: Value = if entity == "album" {
                            self.get_json(
                                "getAlbumList2",
                                &[
                                    ("type", "alphabeticalByName".into()),
                                    ("size", ALBUM_REQUEST_SIZE.to_string()),
                                    ("offset", offset.to_string()),
                                ],
                            )
                            .await?
                        } else {
                            self.get_json(
                                "search3",
                                &[
                                    ("query", String::new()),
                                    ("artistCount", ALBUM_REQUEST_SIZE.to_string()),
                                    ("artistOffset", offset.to_string()),
                                    ("albumCount", "0".into()),
                                    ("albumOffset", "0".into()),
                                    ("songCount", "0".into()),
                                    ("songOffset", "0".into()),
                                ],
                            )
                            .await?
                        };
                        let page = json::items(if entity == "album" {
                            &body["albumList2"]["album"]
                        } else {
                            &body["searchResult3"]["artist"]
                        });
                        if page.is_empty() {
                            break;
                        }
                        offset = offset.checked_add(page.len()).ok_or_else(|| {
                            SourceError::Other(format!("OpenSubsonic {entity} offset overflowed"))
                        })?;
                        let mut batch = library::CatalogBatch::default();
                        if entity == "album" {
                            batch.albums = page
                                .iter()
                                .filter_map(|value| {
                                    album_from_json(self, source_id, value).transpose()
                                })
                                .collect::<library::LibraryResult<_>>()?;
                        } else {
                            batch.artists = page
                                .iter()
                                .filter_map(|value| {
                                    artist_from_json(self, source_id, value).transpose()
                                })
                                .collect::<library::LibraryResult<_>>()?;
                        }
                        check_cancelled(cancelled)?;
                        observation.apply_catalog(&batch, true).await?;
                        progress(stage(read_stage, offset));
                        if page.len() < ALBUM_REQUEST_SIZE {
                            break;
                        }
                    }
                    if stamp.is_some() {
                        check_cancelled(cancelled)?;
                        observation
                            .complete_source_observation(&[entity], None)
                            .await?;
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    complete = false;
                    crate::source::optional_collection_error(error)?;
                }
            }
        }

        check_cancelled(cancelled)?;
        progress(stage(SourceReadStage::Genres, 0));
        match self.read_genres().await {
            Ok(genres) => {
                check_cancelled(cancelled)?;
                observation
                    .apply_catalog(
                        &library::CatalogBatch {
                            tags: genres,
                            ..Default::default()
                        },
                        true,
                    )
                    .await?;
                if stamp.is_some() {
                    check_cancelled(cancelled)?;
                    observation
                        .complete_source_observation(&["genre"], None)
                        .await?;
                }
            }
            Err(error) => crate::source::optional_collection_error(error)?,
        }

        check_cancelled(cancelled)?;
        self.acquire_user_collections(observation, progress, cancelled)
            .await?;

        check_cancelled(cancelled)?;

        progress(stage(SourceReadStage::Finalizing, 0));
        Ok(complete)
    }

    pub(super) async fn acquire_tracks(
        &self,
        observation: &mut library::Observation<'_>,
        music_folders: &[library::FolderObservation],
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = observation.source_id;
        let stamp = observation.observed;

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
                    extra.push((
                        "musicFolderId",
                        raw_item_id(folder.object_id.as_str()).to_string(),
                    ));
                }
                let body: Value = self.get_json("search3", &extra).await?;
                let page = json::items(&body["searchResult3"]["song"]);
                if page.is_empty() {
                    break;
                }
                offset = offset.checked_add(page.len()).ok_or_else(|| {
                    SourceError::Other("OpenSubsonic track offset overflowed".to_string())
                })?;
                let mut tracks = Vec::with_capacity(page.len());
                for song in page {
                    let Some(track) = track_from_json(self, source_id, song)? else {
                        continue;
                    };
                    tracks.push(track);
                }
                drop(body);
                let mut batch = library::CatalogBatch::default();
                emitted += tracks.len();
                if let Some(folder) = folder {
                    batch
                        .folders
                        .extend_from_slice(&[library::FolderObservation {
                            object_id: folder.object_id.clone(),
                            tracks: tracks.iter().map(|track| track.object_id.clone()).collect(),
                            ..Default::default()
                        }]);
                }
                batch.tracks = tracks;
                check_cancelled(cancelled)?;
                observation.apply_catalog(&batch, true).await?;
                progress(stage(SourceReadStage::Tracks, emitted));
            }
            if let Some(folder) = folder
                && stamp.is_some()
            {
                check_cancelled(cancelled)?;
                observation
                    .complete_source_folder_membership(&folder.object_id)
                    .await?;
            }
        }
        if stamp.is_some() {
            check_cancelled(cancelled)?;
            observation
                .complete_source_observation(&["track"], None)
                .await?;
        }
        Ok(())
    }

    async fn read_music_folders(&self) -> SourceResult<Vec<library::FolderObservation>> {
        let body: Value = self.get_json("getMusicFolders", &[]).await?;
        Ok(json::items(&body["musicFolders"]["musicFolder"])
            .iter()
            .filter_map(|folder| {
                let name = json::field::<String>(folder, "name")
                    .unwrap_or_else(|| "Untitled Folder".to_string());
                Some(library::FolderObservation {
                    metadata: library::FolderMetadata {
                        sort_text: Some(name.to_lowercase()),
                        name: Some(name),
                        ..Default::default()
                    },
                    object_id: self.id("music-folder", &json::id(&folder["id"])?),
                    ..Default::default()
                })
            })
            .collect())
    }

    async fn read_genres(&self) -> SourceResult<Vec<library::TagObservation>> {
        let body: Value = self.get_json("getGenres", &[]).await?;
        Ok(json::items(&body["genres"]["genre"])
            .iter()
            .filter_map(|genre| {
                let name = json::field::<String>(genre, "value")
                    .or_else(|| json::field(genre, "name"))
                    .filter(|name| !name.trim().is_empty())?;
                Some(library::TagObservation {
                    kind: library::TagKind::Genre,
                    object_id: self.id("genre", &name),
                    metadata: library::TagMetadata {
                        sort_text: Some(name.to_lowercase()),
                        name,
                    },
                    artwork_binding: None,
                })
            })
            .collect())
    }

    async fn acquire_playlists(
        &self,
        observation: &mut library::Observation<'_>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let catalog = observation.catalog;
        let source_id = observation.source_id;

        let body: Value = self.get_json("getPlaylists", &[]).await?;
        let playlists = json::items(&body["playlists"]["playlist"]);
        let total = playlists.len();
        for (position, playlist) in playlists.iter().enumerate() {
            check_cancelled(cancelled)?;
            let Some(playlist) = playlist_from_json(self, source_id, playlist)? else {
                continue;
            };
            let mut batch = library::CatalogBatch {
                reference: true,
                ..Default::default()
            };
            batch.playlists.push(playlist.header.clone());
            check_cancelled(cancelled)?;
            observation.apply_catalog(&batch, true).await?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let retained = if let Some(revision) = playlist.revision.as_deref()
                && playlist.valid_until.is_some_and(|deadline| deadline > now)
            {
                catalog
                    .source_playlist_is_fresh(
                        source_id,
                        &playlist.header.object_id,
                        Some(revision),
                        now,
                    )
                    .await?
            } else {
                false
            };
            if !retained {
                self.acquire_playlist_snapshot(observation, &playlist.header.object_id, cancelled)
                    .await?;
            }
            progress(SourceReadProgress {
                stage: SourceReadStage::Playlists,
                completed: position + 1,
                total: Some(total),
            });
        }
        Ok(())
    }

    pub(crate) async fn start_metadata_scan_and_wait(
        &self,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let body = self.get_json("getScanStatus", &[]).await?;
        self.wait_metadata_scan(&body, cancelled).await?;
        check_cancelled(cancelled)?;
        let body = self.get_json("startScan", &[]).await?;
        scan_finished(self.wait_metadata_scan(&body, cancelled).await?)
    }

    async fn wait_metadata_scan(
        &self,
        body: &Value,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<ScanStatus> {
        let mut status = ScanStatus::from_json(&body["scanStatus"]);
        for poll in 0..=METADATA_SCAN_MAX_POLLS {
            check_cancelled(cancelled)?;
            if let Some(status) = status.take().filter(|status| !status.scanning) {
                return Ok(status);
            }
            if poll == METADATA_SCAN_MAX_POLLS {
                break;
            }
            tokio::time::sleep(METADATA_SCAN_POLL_INTERVAL).await;
            let body = self.get_json("getScanStatus", &[]).await?;
            status = ScanStatus::from_json(&body["scanStatus"]);
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
    // Counts alone cannot detect a retag or a same-size replacement.
    (!status.scanning
        && status
            .last_scan
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty()))
    .then(|| freshness(status))
}

fn stage(stage: SourceReadStage, completed: usize) -> SourceReadProgress {
    SourceReadProgress {
        stage,
        completed,
        total: None,
    }
}

pub(super) fn check_cancelled(cancelled: &(dyn Fn() -> bool + Send + Sync)) -> SourceResult<()> {
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
        for last_scan in [None, Some(String::new())] {
            let mut incomplete = status(40, "2026-08-27T00:00:00Z");
            incomplete.last_scan = last_scan;
            assert_eq!(completed_freshness(incomplete), None);
        }
    }

    #[tokio::test]
    async fn playlist_expiry_controls_reuse_without_hiding_header_changes_or_removals() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let server = MockServer::start().await;
        let state = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let response_state = state.clone();
        Mock::given(method("GET")).and(path("/rest/getPlaylists.view"))
            .respond_with(move |_: &wiremock::Request| {
                let state = response_state.load(Ordering::Relaxed);
                let mut playlist = serde_json::json!({"id":"playlist","name":if state==1 {"Renamed"} else {"Playlist"},
                    "changed":if state==4 {"revision-2"} else {"revision-1"},"validUntil":"2099-01-01T00:00:00Z"});
                if state==2 { playlist["validUntil"] = "2000-01-01T00:00:00Z".into(); }
                if state==0 { playlist["coverArt"] = "cover".into(); }
                if state==3 || state==6 { playlist.as_object_mut().unwrap().remove("validUntil"); }
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"subsonic-response":{"status":"ok","playlists":{
                    "playlist":if state==5 {vec![]} else {vec![playlist]}
                }}}))
            }).mount(&server).await;
        let response_state = state.clone();
        let response_reads = reads.clone();
        Mock::given(method("GET")).and(path("/rest/getPlaylist.view"))
            .respond_with(move |_: &wiremock::Request| {
                response_reads.fetch_add(1,Ordering::Relaxed);
                let state=response_state.load(Ordering::Relaxed);
                if state==6 { return ResponseTemplate::new(500); }
                let mut playlist=serde_json::json!({"id":"playlist","name":"Playlist","changed":if state==4 {"revision-2"} else {"revision-1"},
                    "validUntil":"2099-01-01T00:00:00Z","entry":if state==4 {vec![]} else {vec![serde_json::json!({"id":"track"})]}});
                if state==2 { playlist["validUntil"]="2000-01-01T00:00:00Z".into(); }
                if state==0 { playlist["coverArt"] = "cover".into(); }
                if state==3 { playlist.as_object_mut().unwrap().remove("validUntil"); }
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"subsonic-response":{"status":"ok","playlist":playlist}}))
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
            super::SubsonicCredential::LegacyPassword("password".into()).serialize(),
        )
        .unwrap();
        let source_owner = crate::Source::new(
            library::SourceId::new("source"),
            crate::Implementation::OpenSubsonic(source),
        );
        let crate::Implementation::OpenSubsonic(source) = &source_owner.implementation else {
            unreachable!()
        };
        let directory = tempfile::tempdir().unwrap();
        let catalog = library::Catalog::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        catalog
            .reconcile_source(&library::SourceId::new("source"), "source", "Source")
            .await
            .unwrap();

        let cancellation = library::ReadCancellation::new();
        for (next, expected_reads) in [(0, 1), (1, 1), (2, 2), (3, 3), (6, 4), (4, 5), (5, 5)] {
            state.store(next, Ordering::Relaxed);
            let catalog_source_id = ("source").to_string();
            let mut observation = catalog.observe(&catalog_source_id, None).await.unwrap();
            let result = {
                let result = source
                    .acquire_user_collections(&mut observation, &|_| {}, &|| false)
                    .await;
                result
            };
            result.unwrap();
            assert_eq!(reads.load(Ordering::Relaxed), expected_reads);
            let rows = crate::listed_playlists(
                &catalog,
                library::ListRead {
                    playlist_scope: Default::default(),
                    kind: library::ListKind::Playlists,
                    source: (Some(
                        {
                            catalog.loaded().await;
                            catalog.source_key(&crate::SourceId::new("source"))
                        }
                        .unwrap(),
                    ))
                    .into(),
                    sort: library::LibraryField::RowIndex,
                    folder: None,
                    filter: ("").to_string(),
                    descending: false,
                    favorites_only: false,
                    downloaded_only: false,
                    activity: false,
                },
            )
            .await;
            if next == 5 {
                assert!(rows.is_empty());
                continue;
            }
            assert_eq!(rows.len(), 1);
            let playlist = rows[0].playlist_key;
            assert_eq!(rows[0].name, if next == 1 { "Renamed" } else { "Playlist" });
            assert_eq!(
                rows[0]
                    .artwork_binding
                    .as_deref()
                    .map(library::authored_playlist_artwork)
                    .transpose()
                    .unwrap()
                    .flatten()
                    .is_some(),
                next <= 1
            );
            assert_eq!(
                catalog
                    .selected_track_uris(
                        &library::MemberRead::new(library::Collection::Playlist(playlist))
                            .selection(),
                        None,
                        &cancellation
                    )
                    .await
                    .unwrap()
                    .len(),
                if next == 4 { 0 } else { 1 }
            );
        }
    }

    #[tokio::test]
    async fn favorites_and_playlists_refresh_without_replacing_catalog_metadata() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let server = MockServer::start().await;
        let state = Arc::new(AtomicUsize::new(0));
        let response_state = state.clone();
        Mock::given(method("GET")).and(path("/rest/getStarred2.view"))
            .respond_with(move |_: &wiremock::Request| {
                match response_state.load(Ordering::Relaxed) {
                    2 => ResponseTemplate::new(500),
                    1 => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "subsonic-response":{"status":"ok","starred2":{}}
                    })),
                    _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "subsonic-response":{"status":"ok","starred2":{
                            "song":[{"id":"track"}],"album":[{"id":"album"}],"artist":[{"id":"artist"}]
                        }}
                    })),
                }
            }).mount(&server).await;
        let response_state = state.clone();
        Mock::given(method("GET"))
            .and(path("/rest/getPlaylists.view"))
            .respond_with(move |_: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "subsonic-response":{"status":"ok","playlists":{"playlist":
                        if response_state.load(Ordering::Relaxed) == 1 { serde_json::json!([]) }
                        else { serde_json::json!([{"id":"playlist","name":"Playlist"}]) }
                    }}
                }))
            })
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/rest/getPlaylist.view"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response":{"status":"ok","playlist":{"id":"playlist","name":"Playlist","entry":[{"id":"track"}]}}
            }))).mount(&server).await;
        let source = SubsonicSource::open(
            SubsonicFlavor::Subsonic,
            SubsonicSourceConfig {
                base_url: server.uri(),
                username: "listener".into(),
                trust_invalid_cert: false,
                navidrome_library_version: 0,
                authentication: SubsonicAuthentication::LegacyPassword,
            },
            super::SubsonicCredential::LegacyPassword("password".into()).serialize(),
        )
        .unwrap();
        let source_owner = crate::Source::new(
            library::SourceId::new("source"),
            crate::Implementation::OpenSubsonic(source),
        );
        let crate::Implementation::OpenSubsonic(source) = &source_owner.implementation else {
            unreachable!()
        };
        let directory = tempfile::tempdir().unwrap();
        let catalog = library::Catalog::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let mut updates = catalog.changes();
        let marker = library::Freshness::new(b"unchanged catalog".to_vec()).unwrap();
        catalog
            .reconcile_source(&library::SourceId::new("source"), "source", "Source")
            .await
            .unwrap();
        let catalog_source_id = ("source").to_string();
        let mut observation = catalog.observe(&catalog_source_id, None).await.unwrap();
        let stamp = Some(observation.start_observation().await.unwrap());
        (async {
            let mut batch = library::CatalogBatch::default();
            let result = batch.albums.extend_from_slice(&[(super::album_from_json(
                &source,
                "source",
                &serde_json::json!({"id":"album","name":"Album","userRating":4}),
            )
            .unwrap()
            .unwrap())
            .clone()]);
            observation.apply_catalog(&batch, true).await?;
            Ok::<_, library::LibraryError>(result)
        })
        .await
        .unwrap();
        (async {
            let mut batch = library::CatalogBatch::default();
            let result = batch.artists.extend_from_slice(&[(super::artist_from_json(
                &source,
                "source",
                &serde_json::json!({"id":"artist","name":"Artist"}),
            )
            .unwrap()
            .unwrap())
            .clone()]);
            observation.apply_catalog(&batch, true).await?;
            Ok::<_, library::LibraryError>(result)
        })
        .await
        .unwrap();
        (async { let mut batch = library::CatalogBatch::default(); let result = batch.tracks.extend_from_slice(&[(super::track_from_json(&source, "source", &serde_json::json!({"id":"track","title":"Track","albumId":"album","comment":"Keep","duration":42,"userRating":4})).unwrap().unwrap()).clone()]); observation.apply_catalog(&batch, true).await?; Ok::<_,library::LibraryError>(result) }).await.unwrap();
        {
            let result = source.acquire_favorites(&mut observation, &|| false).await;
            result
        }
        .unwrap();
        {
            let result = source
                .acquire_playlist_snapshot(&mut observation, "subsonic:playlist:playlist", &|| {
                    false
                })
                .await;
            result
        }
        .unwrap();
        (async {
            if stamp.is_some() {
                observation
                    .complete_source_observation(
                        &[
                            "track", "album", "artist", "genre", "mood", "folder", "playlist",
                        ],
                        (Some(marker.clone())).as_ref(),
                    )
                    .await
                    .unwrap();
            }
            if stamp.is_some() {
                observation
                    .complete_source_observation(&[], Some(&marker))
                    .await
                    .unwrap();
            }
            Ok::<_, library::LibraryError>(())
        })
        .await
        .unwrap();
        let publication = crate::published(&mut updates).expect("catalog update").1;
        assert!(publication.intersects(sources::Shown::CATALOG));
        for (next, expected) in [(2, true), (1, false), (0, true)] {
            state.store(next, Ordering::Relaxed);
            let catalog_source_id = ("source").to_string();
            let mut observation = catalog.observe(&catalog_source_id, None).await.unwrap();
            let result = {
                let result = source
                    .acquire_user_collections(&mut observation, &|_| {}, &|| false)
                    .await;
                result
            };
            result.unwrap();
            for (kind, id) in [("track", "track"), ("album", "album"), ("artist", "artist")] {
                let uri = library::source_entity_uri(
                    &crate::SourceId::new("source"),
                    kind,
                    &format!("subsonic:{kind}:{id}"),
                );
                let target = match kind {
                    "track" => library::FavoriteTarget::Track(uri),
                    "album" => library::FavoriteTarget::Album(uri),
                    _ => library::FavoriteTarget::Artist(uri),
                };
                assert_eq!(catalog.favorite(&target).await.unwrap(), expected);
            }
            let cancellation = library::ReadCancellation::new();
            let uri = library::source_entity_uri(
                &crate::SourceId::new("source"),
                "track",
                "subsonic:track:track",
            );
            let row = catalog
                .track(&(&uri), &cancellation)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.metadata.comment.as_deref(), Some("Keep"));
            assert!(
                catalog
                    .accept_freshness("source", &marker, &cancellation)
                    .await
                    .unwrap()
            );
            assert_eq!(
                crate::listed_playlists(
                    &catalog,
                    library::ListRead {
                        playlist_scope: Default::default(),
                        kind: library::ListKind::Playlists,
                        source: (Some(
                            {
                                catalog.loaded().await;
                                catalog.source_key(&library::SourceId::new(&catalog_source_id))
                            }
                            .unwrap()
                        ))
                        .into(),
                        sort: library::LibraryField::Title,
                        folder: None,
                        filter: ("").to_string(),
                        descending: false,
                        favorites_only: false,
                        downloaded_only: false,
                        activity: false
                    }
                )
                .await
                .len() as i64,
                i64::from(next != 1)
            );
        }
        crate::published(&mut updates);
        let catalog_source_id = ("source").to_string();
        let mut observation = catalog.observe(&catalog_source_id, None).await.unwrap();
        let result = {
            let result = source
                .acquire_user_collections(&mut observation, &|_| {}, &|| false)
                .await;
            result
        };
        result.unwrap();
        assert!(!observation.changed());
        assert!(
            crate::published(&mut updates).is_none(),
            "unchanged native data stays quiet"
        );
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
                    let songs = if query.get("songCount").is_some_and(|count| count == "500") {
                        match query.get("songOffset").map(|value| value.as_ref()) {
                            Some("0") => {
                                let mut page = vec![serde_json::json!({"title":"No identity"}); 499];
                                page.push(serde_json::json!({
                                    "id":"track", "title":"Track", "album":"Album", "albumId":"album",
                                    "artist":"Artist", "duration":42, "isDir":"false", "bpm":{},
                                    "artists":[{"id":"artist","name":"Artist"}, {"name":"No identity"}],
                                    "genres":[{"name":"Rock"}, {"name":false}]
                                }));
                                serde_json::Value::Array(page)
                            }
                            Some("500") => serde_json::json!([{"id":2,"title":"Next page","albumId":"album"}]),
                            _ => serde_json::json!([]),
                        }
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
            let source_owner = crate::Source::new(
                library::SourceId::new("source"),
                crate::Implementation::OpenSubsonic(source),
            );
            let crate::Implementation::OpenSubsonic(source) = &source_owner.implementation else {
                unreachable!()
            };
            let directory = tempfile::tempdir().unwrap();
            let catalog = library::Catalog::open(directory.path().join("library.sqlite"))
                .await
                .unwrap();
            let mut updates = catalog.changes();
            let marker = library::Freshness::new(b"server-observation".to_vec()).unwrap();
            if folders_available {
                catalog
                    .reconcile_source(
                        &library::SourceId::new("source"),
                        "nextcloud music",
                        "Nextcloud Music",
                    )
                    .await
                    .unwrap();
                let catalog_source_id = ("source").to_string();
                let mut observation = catalog.observe(&catalog_source_id, None).await.unwrap();
                observation.start_observation().await.unwrap();
                let batch=library::CatalogBatch {
                    albums:vec![super::album_from_json(source,"source",&serde_json::json!({"id":"album","name":"Album","artist":"Artist"})).unwrap().unwrap()],
                    tracks:vec![super::track_from_json(source,"source",&serde_json::json!({"id":"removed","title":"Vanished","albumId":"album"})).unwrap().unwrap()],
                    tags:vec![library::TagObservation {kind:library::TagKind::Genre,object_id:"cached-genre".into(),metadata:library::TagMetadata {name:"Cached".into(),sort_text:Some("cached".into())},artwork_binding:None}],
                    playlists:vec![library::PlaylistObservation {object_id:"cached-playlist".into(),name:"Cached".into(),sort_text:"cached".into(),..Default::default()}],..Default::default()
                };
                observation.apply_catalog(&batch, true).await.unwrap();
                observation
                    .write_source_playlist_page(
                        "cached-playlist",
                        0,
                        &[library::PlaylistEntryObservation {
                            object_id: "occurrence".into(),
                            track_id: "subsonic:track:removed".into(),
                        }],
                        true,
                    )
                    .await
                    .unwrap();
                observation
                    .replace_source_home_section(
                        "recently-played",
                        None,
                        &[library::HomeEntryInput {
                            position: 0,
                            kind: library::HomeEntryKind::Album,
                            entity_object_id: "subsonic:album:album".into(),
                            title: "Album".into(),
                            subtitle: "Artist".into(),
                        }],
                    )
                    .await
                    .unwrap();
                crate::published(&mut updates)
                    .expect("initial catalog update")
                    .1;
            }
            let progress = std::sync::Mutex::new(Vec::new());
            catalog
                .reconcile_source(
                    &library::SourceId::new("source"),
                    "nextcloud music",
                    "Nextcloud Music",
                )
                .await
                .unwrap();
            let catalog_source_id = ("source").to_string();
            let mut observation = catalog.observe(&catalog_source_id, None).await.unwrap();
            let stamp = Some(observation.start_observation().await.unwrap());
            let complete = {
                let result = source
                    .acquire_catalog(
                        &mut observation,
                        &|value| {
                            progress.lock().unwrap().push(value);
                        },
                        &|| false,
                    )
                    .await;
                result
            }
            .unwrap();
            (async {
                if stamp.is_some() {
                    observation
                        .complete_source_observation(&[], Some(&marker).filter(|_| complete))
                        .await?;
                }
                Ok::<_, library::LibraryError>(())
            })
            .await
            .unwrap();
            let publication = crate::published(&mut updates).expect("catalog update").1;
            assert!(publication.intersects(sources::Shown::CATALOG));
            let cancellation = library::ReadCancellation::new();
            let home = catalog
                .home_page(
                    {
                        catalog.loaded().await;
                        catalog.source_key(&library::SourceId::new(&catalog_source_id))
                    }
                    .unwrap(),
                    None,
                    0,
                    0,
                    &library::HomeBlockKind::all(),
                    &cancellation,
                )
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
                Some(if folders_available { 4 } else { 2 })
            );
            for (id, title) in [("track", "Track"), ("2", "Next page")] {
                let uri = library::source_entity_uri(
                    &crate::SourceId::new("source"),
                    "track",
                    &format!("subsonic:track:{id}"),
                );
                let track = catalog
                    .track(&(&uri), &cancellation)
                    .await
                    .unwrap()
                    .expect("usable track is published");
                assert_eq!(track.metadata.title, title);
            }
            assert_eq!(
                catalog
                    .accept_freshness("source", &marker, &cancellation)
                    .await
                    .unwrap(),
                folders_available
            );
            if folders_available {
                let removed = library::source_entity_uri(
                    &crate::SourceId::new("source"),
                    "track",
                    "subsonic:track:removed",
                );
                assert!(
                    !catalog
                        .selected_track_uris(
                            &library::MemberRead::new(library::Collection::Source(
                                {
                                    catalog.loaded().await;
                                    catalog.source_key(&library::SourceId::new(&catalog_source_id))
                                }
                                .unwrap()
                            ))
                            .selection(),
                            None,
                            &cancellation
                        )
                        .await
                        .unwrap()
                        .contains(&removed)
                );
                assert!(
                    catalog
                        .catalog_keys(
                            library::CatalogKind::Genre,
                            Some(
                                {
                                    catalog.loaded().await;
                                    catalog.source_key(&library::SourceId::new(&catalog_source_id))
                                }
                                .unwrap()
                            ),
                            &[("cached-genre").to_owned()],
                            &cancellation
                        )
                        .await
                        .map(|v| v.into_iter().next().map(library::GenreKey::from_raw))
                        .unwrap()
                        .is_some()
                );
                let playlist = library::PlaylistKey::from_raw(
                    catalog
                        .catalog_keys(
                            library::CatalogKind::Playlist,
                            {
                                catalog.loaded().await;
                                catalog.source_key(&library::SourceId::new(&catalog_source_id))
                            },
                            &["cached-playlist".into()],
                            &cancellation,
                        )
                        .await
                        .unwrap()[0],
                );
                assert_eq!(
                    catalog
                        .selected_track_uris(
                            &library::MemberRead::new(library::Collection::Playlist(playlist))
                                .selection(),
                            None,
                            &cancellation,
                        )
                        .await
                        .unwrap(),
                    vec![removed]
                );
                assert_eq!(home.recently_played.albums.len(), 1);
                for id in ["one", "two"] {
                    let folder = catalog
                        .catalog_keys(
                            library::CatalogKind::Folder,
                            Some(
                                {
                                    catalog.loaded().await;
                                    catalog.source_key(&library::SourceId::new(&catalog_source_id))
                                }
                                .unwrap(),
                            ),
                            &[(&source.id("music-folder", id)).to_string()],
                            &cancellation,
                        )
                        .await
                        .map(|keys| keys.into_iter().next().map(library::FolderKey::from_raw))
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        catalog
                            .home_page(
                                {
                                    catalog.loaded().await;
                                    catalog.source_key(&library::SourceId::new(&catalog_source_id))
                                }
                                .unwrap(),
                                Some(folder),
                                0,
                                0,
                                &library::HomeBlockKind::all(),
                                &cancellation
                            )
                            .await
                            .unwrap()
                            .explore
                            .len(),
                        1
                    );
                }
            }
            // getScanStatus and optional collection endpoints are deliberately absent.
            // Automatic refresh must still acquire the catalog and keep no-ops quiet.
            progress.lock().unwrap().clear();
            let source = source_owner;
            source
                .acquire_catalog(
                    &catalog,
                    &|value| progress.lock().unwrap().push(value),
                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    false,
                )
                .await
                .unwrap();
            assert!(
                crate::published(&mut updates).is_none(),
                "unchanged automatic acquisition stays quiet"
            );
            assert!(
                progress
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|value| value.stage == crate::SourceReadStage::Tracks)
            );
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
        assert_eq!(entries[0].title, "New Album");
    }
}
