//! Streams Jellyfin catalog pages directly into Library Scan staging.

use std::collections::HashSet;

use library::Scan;

use super::*;
use crate::source::{SourceReadProgress, SourceReadStage};

impl JellyfinSource {
    pub(crate) async fn home_section(
        &self,
        section: crate::SourceHomeSection,
    ) -> SourceResult<Vec<library::HomeEntryInput>> {
        let (section_id, item_type, sort_by, kind) = match section {
            crate::SourceHomeSection::MostPlayed => (
                "most-played",
                "Audio",
                "PlayCount,SortName",
                library::HomeEntryKind::Track,
            ),
            crate::SourceHomeSection::NewlyAdded => (
                "newly-added",
                "MusicAlbum",
                "DateCreated,SortName",
                library::HomeEntryKind::Album,
            ),
            crate::SourceHomeSection::RecentlyPlayed => (
                "recently-played",
                "Audio",
                "DatePlayed,SortName",
                library::HomeEntryKind::Track,
            ),
            crate::SourceHomeSection::RecentlyReleased => (
                "recently-released",
                "MusicAlbum",
                "ProductionYear,PremiereDate,SortName",
                library::HomeEntryKind::Album,
            ),
        };
        let page = self
            .item_page_sorted(item_type, 0, 24, sort_by, "Descending")
            .await?;
        page.items
            .into_iter()
            .enumerate()
            .map(|(position, item)| {
                let (entity_object_id, title, subtitle, artwork_binding) = match kind {
                    library::HomeEntryKind::Track => {
                        let track = track_from_item(item);
                        (
                            track.id,
                            track.title,
                            track.artist,
                            track
                                .image_ref
                                .as_ref()
                                .map(serde_json::to_vec)
                                .transpose()?,
                        )
                    }
                    library::HomeEntryKind::Album => {
                        let album = album_from_item(item);
                        (
                            album.id,
                            album.title,
                            album.artist,
                            album
                                .image_ref
                                .as_ref()
                                .map(serde_json::to_vec)
                                .transpose()?,
                        )
                    }
                    _ => unreachable!(),
                };
                Ok(library::HomeEntryInput {
                    section_id: section_id.to_string(),
                    position: position as i64,
                    kind,
                    entity_object_id,
                    title,
                    subtitle,
                    artwork_binding,
                })
            })
            .collect()
    }
    pub(crate) async fn apply_live_items(
        &self,
        database: &library::Database,
        source_id: &str,
        upserts: Vec<String>,
        removals: Vec<String>,
    ) -> SourceResult<library::ScanOutcome> {
        let mut scan = Scan::begin_items(database, source_id).await?;
        for raw_id in upserts {
            let mut url = endpoint(&self.base_url, &format!("Items/{raw_id}"))?;
            url.query_pairs_mut()
                .append_pair("UserId", &self.user_id)
                .append_pair("Fields", MIXED_ITEM_FIELDS);
            let item = self.get_json::<JellyfinItem>(url).await?;
            let item_type = item.item_type.clone().unwrap_or_default();
            if item_type.eq_ignore_ascii_case("Audio") {
                if let Some(album_id) = item.album_id.clone() {
                    let mut album_url = endpoint(&self.base_url, &format!("Items/{album_id}"))?;
                    album_url
                        .query_pairs_mut()
                        .append_pair("UserId", &self.user_id)
                        .append_pair("Fields", ALBUM_FIELDS);
                    let album = self.get_json::<JellyfinItem>(album_url).await?;
                    scan.begin_batch().await?;
                    stage_album(&mut scan, album_from_item(album)).await?;
                    scan.finish_batch().await?;
                }
                scan.begin_batch().await?;
                stage_track(&mut scan, track_from_item(item)).await?;
                scan.finish_batch().await?;
                self.stage_live_track_folders(&mut scan, &raw_id).await?;
            } else if item_type.eq_ignore_ascii_case("MusicAlbum") {
                scan.begin_batch().await?;
                stage_album(&mut scan, album_from_item(item)).await?;
                scan.finish_batch().await?;
            } else if item_type.eq_ignore_ascii_case("MusicArtist") {
                scan.begin_batch().await?;
                stage_artist(&mut scan, artist_from_item(item)).await?;
                scan.finish_batch().await?;
            } else if item_type.eq_ignore_ascii_case("MusicGenre") {
                scan.begin_batch().await?;
                stage_genre(&mut scan, genre_from_item(item)).await?;
                scan.finish_batch().await?;
            } else if item_type.eq_ignore_ascii_case("Playlist") {
                let playlist = playlist_from_item(item);
                let artwork = playlist
                    .image_ref
                    .as_ref()
                    .map(serde_json::to_vec)
                    .transpose()?;
                scan.begin_batch().await?;
                scan.write_playlist(
                    &playlist.id,
                    &playlist.name,
                    &playlist.name.to_lowercase(),
                    &playlist.name.to_lowercase(),
                    artwork.as_deref(),
                )
                .await?;
                scan.finish_batch().await?;
                self.stage_playlist_entries(&mut scan, &playlist.id).await?;
            }
        }
        scan.begin_batch().await?;
        for raw_id in removals {
            scan.remove_track(&jellyfin_id("track", &raw_id)).await?;
            scan.remove_album(&jellyfin_id("album", &raw_id)).await?;
            scan.remove_artist(&jellyfin_id("artist", &raw_id)).await?;
            scan.remove_genre(&jellyfin_id("genre", &raw_id)).await?;
            scan.remove_playlist(&jellyfin_id("playlist", &raw_id))
                .await?;
        }
        scan.finish_batch().await?;
        Ok(scan.finish().await?)
    }

    async fn stage_live_track_folders(
        &self,
        scan: &mut Scan,
        raw_track_id: &str,
    ) -> SourceResult<()> {
        let mut url = endpoint(&self.base_url, &format!("Items/{raw_track_id}/Ancestors"))?;
        url.query_pairs_mut().append_pair("UserId", &self.user_id);
        let ancestors = self.get_json::<Vec<JellyfinItem>>(url).await?;
        scan.begin_batch().await?;
        let mut folders = Vec::new();
        for (position, folder) in ancestors
            .into_iter()
            .filter(|item| {
                item.collection_type
                    .as_deref()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("music"))
            })
            .enumerate()
        {
            let Some(name) = folder.name else { continue };
            let folder_id = jellyfin_id("music-folder", &folder.id);
            let artwork = primary_image_ref("music-folder", &folder.id, &folder.image_tags)
                .as_ref()
                .map(serde_json::to_vec)
                .transpose()?;
            scan.write_folder(
                &folder_id,
                &name,
                &name.to_lowercase(),
                &name.to_lowercase(),
                artwork.as_deref(),
            )
            .await?;
            folders.push((folder_id, position as i64));
        }
        let track_id = jellyfin_id("track", raw_track_id);
        scan.write_track_folders(
            &folders
                .iter()
                .map(|(folder_id, position)| {
                    library::ScanLink::new(&track_id, folder_id, *position)
                })
                .collect::<Vec<_>>(),
        )
        .await?;
        scan.finish_batch().await?;
        Ok(())
    }

    pub(crate) async fn stage_catalog(
        &self,
        scan: &mut Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let mut pages = PageState::default();
        loop {
            check_cancelled(cancelled)?;
            let page = self
                .item_page("MusicAlbum", pages.offset(), COLLECTION_PAGE_SIZE)
                .await?;
            let count = page.items.len();
            let finished = pages.advance(count, page.total_record_count)?;
            scan.begin_batch().await?;
            for item in page.items {
                stage_album(scan, album_from_item(item)).await?;
            }
            scan.finish_batch().await?;
            progress(stage(
                SourceReadStage::Albums,
                pages.offset(),
                pages.total(),
            ));
            if finished {
                break;
            }
        }

        let folders = self.stage_music_folders(scan, cancelled).await?;
        for (folder_position, folder_id) in folders.iter().enumerate() {
            self.stage_music_folder_memberships(scan, folder_id, folder_position as i64, cancelled)
                .await?;
        }

        let mut pages = PageState::default();
        loop {
            check_cancelled(cancelled)?;
            let page = self
                .item_page("Audio", pages.offset(), COLLECTION_PAGE_SIZE)
                .await?;
            let count = page.items.len();
            let finished = pages.advance(count, page.total_record_count)?;
            scan.begin_batch().await?;
            for item in page.items {
                stage_track(scan, track_from_item(item)).await?;
            }
            scan.finish_batch().await?;
            progress(stage(
                SourceReadStage::Tracks,
                pages.offset(),
                pages.total(),
            ));
            if finished {
                break;
            }
        }

        for path in ["Artists", "Artists/AlbumArtists"] {
            let mut pages = PageState::default();
            loop {
                check_cancelled(cancelled)?;
                let page = self
                    .people_page(path, pages.offset(), COLLECTION_PAGE_SIZE)
                    .await?;
                let count = page.items.len();
                let finished = pages.advance(count, page.total_record_count)?;
                scan.begin_batch().await?;
                for item in page.items {
                    stage_artist(scan, artist_from_item(item)).await?;
                }
                scan.finish_batch().await?;
                progress(stage(
                    SourceReadStage::Artists,
                    pages.offset(),
                    pages.total(),
                ));
                if finished {
                    break;
                }
            }
        }

        let mut pages = PageState::default();
        loop {
            check_cancelled(cancelled)?;
            let page = self
                .music_genre_page(pages.offset(), COLLECTION_PAGE_SIZE)
                .await?;
            let count = page.items.len();
            let finished = pages.advance(count, page.total_record_count)?;
            scan.begin_batch().await?;
            for item in page.items {
                stage_genre(scan, genre_from_item(item)).await?;
            }
            scan.finish_batch().await?;
            progress(stage(
                SourceReadStage::Genres,
                pages.offset(),
                pages.total(),
            ));
            if finished {
                break;
            }
        }

        self.stage_playlists(scan, progress, cancelled).await?;
        self.stage_home(scan).await?;
        progress(stage(SourceReadStage::Finalizing, 1, Some(1)));
        Ok(())
    }

    async fn stage_music_folders(
        &self,
        scan: &mut Scan,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<Vec<String>> {
        check_cancelled(cancelled)?;
        let mut url = endpoint(&self.base_url, &format!("Users/{}/Views", self.user_id))?;
        url.query_pairs_mut()
            .append_pair("IncludeExternalContent", "false");
        let response = self.get_json::<ItemQueryResult>(url).await?;
        let mut folders = Vec::new();
        scan.begin_batch().await?;
        for item in response.items.into_iter().filter(|item| {
            item.collection_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("music"))
        }) {
            let Some(name) = item.name else { continue };
            let id = jellyfin_id("music-folder", &item.id);
            let artwork = primary_image_ref("music-folder", &item.id, &item.image_tags)
                .as_ref()
                .map(serde_json::to_vec)
                .transpose()?;
            scan.write_folder(
                &id,
                &name,
                &name.to_lowercase(),
                &name.to_lowercase(),
                artwork.as_deref(),
            )
            .await?;
            folders.push(id);
        }
        scan.finish_batch().await?;
        Ok(folders)
    }

    async fn stage_music_folder_memberships(
        &self,
        scan: &mut Scan,
        folder_id: &str,
        folder_position: i64,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let raw_folder_id = raw_item_id(folder_id).to_string();
        let mut pages = PageState::default();
        loop {
            check_cancelled(cancelled)?;
            let mut url = endpoint(&self.base_url, "Items")?;
            url.query_pairs_mut()
                .append_pair("UserId", &self.user_id)
                .append_pair("ParentId", &raw_folder_id)
                .append_pair("Recursive", "true")
                .append_pair("IncludeItemTypes", "Audio")
                .append_pair("StartIndex", &pages.offset().to_string())
                .append_pair("Limit", &COLLECTION_PAGE_SIZE.to_string())
                .append_pair("SortBy", "SortName")
                .append_pair("SortOrder", "Ascending");
            let page = self.get_json::<ItemQueryResult>(url).await?;
            let count = page.items.len();
            let finished = pages.advance(count, page.total_record_count)?;
            scan.begin_batch().await?;
            let track_ids = page
                .items
                .into_iter()
                .map(|item| jellyfin_id("track", &item.id))
                .collect::<Vec<_>>();
            scan.write_track_folders(
                &track_ids
                    .iter()
                    .map(|track_id| library::ScanLink::new(track_id, folder_id, folder_position))
                    .collect::<Vec<_>>(),
            )
            .await?;
            scan.finish_batch().await?;
            if finished {
                return Ok(());
            }
        }
    }

    async fn stage_playlists(
        &self,
        scan: &mut Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let mut pages = PageState::default();
        let mut seen = HashSet::new();
        loop {
            check_cancelled(cancelled)?;
            let page = self
                .item_page("Playlist", pages.offset(), COLLECTION_PAGE_SIZE)
                .await?;
            let count = page.items.len();
            let finished = pages.advance(count, page.total_record_count)?;
            for item in page.items {
                let playlist = playlist_from_item(item);
                if !seen.insert(playlist.id.clone()) {
                    continue;
                }
                let artwork = playlist
                    .image_ref
                    .as_ref()
                    .map(serde_json::to_vec)
                    .transpose()?;
                scan.begin_batch().await?;
                scan.write_playlist(
                    &playlist.id,
                    &playlist.name,
                    &playlist.name.to_lowercase(),
                    &playlist.name.to_lowercase(),
                    artwork.as_deref(),
                )
                .await?;
                scan.finish_batch().await?;
                self.stage_playlist_entries(scan, &playlist.id).await?;
            }
            progress(stage(
                SourceReadStage::Playlists,
                pages.offset(),
                pages.total(),
            ));
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
            let entries = self.home_section(section).await?;
            scan.begin_batch().await?;
            for entry in entries {
                scan.write_home_entry(&entry).await?;
            }
            scan.finish_batch().await?;
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct PageState {
    offset: usize,
    total: Option<usize>,
}

impl PageState {
    pub(super) fn offset(&self) -> usize {
        self.offset
    }

    pub(super) fn total(&self) -> Option<usize> {
        self.total
    }

    pub(super) fn advance(&mut self, count: usize, total: Option<usize>) -> SourceResult<bool> {
        if count == 0 {
            return Ok(true);
        }
        self.offset = self
            .offset
            .checked_add(count)
            .ok_or_else(|| SourceError::Other("Jellyfin page offset overflowed".to_string()))?;
        if let Some(total) = total {
            self.total = Some(total);
            Ok(self.offset >= total)
        } else {
            Ok(count < COLLECTION_PAGE_SIZE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PageState;

    use crate::jellyfin::{JellyfinSource, JellyfinSourceConfig};
    use library::{Scan, ScanOutcome};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn playlist_pages_continue_when_the_reported_total_grows() {
        let mut pages = PageState::default();
        assert!(!pages.advance(1, Some(2)).expect("first page"));
        assert_eq!(pages.offset(), 1);
        assert!(!pages.advance(1, Some(3)).expect("grown total"));
        assert_eq!(pages.offset(), 2);
        assert!(pages.advance(1, Some(3)).expect("final page"));
    }

    #[tokio::test]
    async fn newly_added_home_keeps_the_jellyfin_provider_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items"))
            .and(query_param("IncludeItemTypes", "MusicAlbum"))
            .and(query_param("SortBy", "DateCreated,SortName"))
            .and(query_param("SortOrder", "Descending"))
            .and(query_param("StartIndex", "0"))
            .and(query_param("Limit", "24"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [{ "Id": "album-one", "Name": "New Album" }],
                "TotalRecordCount": 1
            })))
            .expect(1)
            .mount(&server)
            .await;
        let source = JellyfinSource::open(
            JellyfinSourceConfig {
                base_url: server.uri(),
                server_id: Some("server-one".to_string()),
                user_id: "user-one".to_string(),
                username: "listener".to_string(),
                trust_invalid_cert: false,
                use_instant_mix: false,
            },
            "secret-token".to_string(),
            "device-one".to_string(),
        )
        .expect("Jellyfin source");

        let entries = source
            .home_section(crate::SourceHomeSection::NewlyAdded)
            .await
            .expect("Jellyfin Newly Added");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].section_id, "newly-added");
        assert_eq!(entries[0].title, "New Album");
    }

    #[tokio::test]
    async fn overlapping_playlist_pages_stage_each_identity_once() {
        let server = MockServer::start().await;
        for offset in ["0", "1"] {
            Mock::given(method("GET"))
                .and(path("/Items"))
                .and(query_param("IncludeItemTypes", "Playlist"))
                .and(query_param("StartIndex", offset))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "Items": [{ "Id": "playlist-one", "Name": "Playlist One" }],
                    "TotalRecordCount": 2
                })))
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/Playlists/playlist-one/Items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [],
                "TotalRecordCount": 0
            })))
            .expect(1)
            .mount(&server)
            .await;
        let source = JellyfinSource::open(
            JellyfinSourceConfig {
                base_url: server.uri(),
                server_id: Some("server-one".to_string()),
                user_id: "user-one".to_string(),
                username: "listener".to_string(),
                trust_invalid_cert: false,
                use_instant_mix: false,
            },
            "secret-token".to_string(),
            "device-one".to_string(),
        )
        .expect("Jellyfin source");
        let directory = tempfile::tempdir().expect("Library directory");
        let database = library::Database::open(directory.path().join("library.sqlite3"))
            .await
            .expect("Library database");
        let mut scan = Scan::begin(&database, "jellyfin:test", "Jellyfin", "jellyfin", None)
            .await
            .expect("begin Scan");

        source
            .stage_playlists(&mut scan, &|_| {}, &|| false)
            .await
            .expect("stage overlapping Playlist pages");

        assert!(matches!(
            scan.finish().await.expect("publish Playlists"),
            ScanOutcome::Changed(_)
        ));
    }
}

fn stage(stage: SourceReadStage, completed: usize, total: Option<usize>) -> SourceReadProgress {
    SourceReadProgress {
        stage,
        completed,
        total,
    }
}

fn check_cancelled(cancelled: &(dyn Fn() -> bool + Send + Sync)) -> SourceResult<()> {
    if cancelled() {
        Err(SourceError::Cancelled)
    } else {
        Ok(())
    }
}
