//! Streams Jellyfin catalog pages into bounded Library changes.

use std::collections::HashSet;

use super::*;
use crate::source::{SourceReadProgress, SourceReadStage};

impl JellyfinEmbySource {
    pub(crate) async fn home_section(
        &self,
        section: crate::SourceHomeSection,
    ) -> SourceResult<Vec<library::HomeEntryInput>> {
        let (item_type, sort_by, kind) = match section {
            crate::SourceHomeSection::MostPlayed => {
                ("Audio", "PlayCount,SortName", library::HomeEntryKind::Track)
            }
            crate::SourceHomeSection::NewlyAdded => (
                "MusicAlbum",
                "DateCreated,SortName",
                library::HomeEntryKind::Album,
            ),
            crate::SourceHomeSection::RecentlyPlayed => (
                "Audio",
                "DatePlayed,SortName",
                library::HomeEntryKind::Track,
            ),
            crate::SourceHomeSection::RecentlyReleased => (
                "MusicAlbum",
                "ProductionYear,PremiereDate,SortName",
                library::HomeEntryKind::Album,
            ),
        };
        let page = self
            .item_page_sorted(item_type, 0, 24, sort_by, "Descending")
            .await?;
        Ok(items(&page["Items"])
            .iter()
            .take(24)
            .enumerate()
            .filter_map(|(position, item)| home_entry_from_item(self.kind, item, position, kind))
            .collect())
    }
    pub(crate) async fn acquire_items(
        &self,
        catalog: &mut library::Catalog<'_>,
        upserts: Vec<String>,
        mut removals: Vec<String>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = catalog.source_id;

        for raw_id in upserts {
            let mut url = self.item_url(&raw_id)?;
            url.query_pairs_mut()
                .append_pair("UserId", &self.user_id)
                .append_pair("Fields", MIXED_ITEM_FIELDS);
            let item = match self.get_json::<Value>(url).await {
                Ok(item) => item,
                Err(SourceError::NotFound) => {
                    removals.push(raw_id);
                    continue;
                }
                Err(error) => return Err(error),
            };
            if id(&item["Id"]).is_none() {
                continue;
            }
            let item_type = item["Type"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if item_type == "musicalbum" {
                self.acquire_collection(
                    catalog,
                    &crate::SourceCollection::Album(self.kind.object_id("album", &raw_id)),
                    Some(&item),
                    true,
                    cancelled,
                )
                .await?;
                continue;
            }
            let mut batch = library::CatalogBatch::default();
            let mut folders = Ok(None);
            let mut playlist_id = None;
            match item_type.as_str() {
                "audio" => {
                    let mut track = track_from_item(self.kind, source_id, &item)?;
                    if let Some(album_id) = id(&item["AlbumId"]) {
                        batch
                            .albums
                            .extend(self.read_album(source_id, &album_id).await?);
                    }
                    folders = self.item_folders(source_id, &raw_id).await;
                    if let Ok(Some(folders)) = &folders {
                        if let Some(track) = &mut track {
                            track.folders = Some(
                                folders
                                    .iter()
                                    .map(|folder| folder.object_id.clone())
                                    .collect(),
                            );
                        }
                        batch.folders.extend_from_slice(folders);
                    }
                    batch.tracks.extend(track);
                }
                "musicartist" => batch
                    .artists
                    .extend(artist_from_item(self.kind, source_id, &item)?),
                "musicgenre" => batch
                    .genres
                    .extend(genre_from_item(self.kind, source_id, &item)?),
                "playlist" => {
                    let Some(playlist) = playlist_from_item(self.kind, &item) else {
                        continue;
                    };
                    let artwork_binding = playlist
                        .image_ref
                        .as_ref()
                        .map(|image| crate::native_artwork_binding(source_id, image))
                        .transpose()?;
                    playlist_id = Some(playlist.id.clone());
                    batch.playlists.push(library::PlaylistObservation {
                        object_id: playlist.id,
                        name: playlist.name.clone(),
                        sort_text: playlist.name.to_lowercase(),
                        artwork_binding,
                        ..Default::default()
                    });
                }
                _ => continue,
            }
            check_cancelled(cancelled)?;
            catalog.apply_catalog(&batch, true).await?;
            folders?;
            if let Some(playlist_id) = playlist_id {
                self.acquire_playlist_entries(catalog, &playlist_id, cancelled)
                    .await?;
            }
        }

        for raw_id in removals {
            check_cancelled(cancelled)?;
            for kind in ["track", "album", "artist", "genre", "playlist"] {
                catalog
                    .remove_source_items(kind, &[self.kind.object_id(kind, &raw_id)])
                    .await?;
            }
        }

        Ok(())
    }

    pub(super) async fn item_folders(
        &self,
        source_id: &str,
        raw_item_id: &str,
    ) -> SourceResult<Option<Vec<library::FolderObservation>>> {
        let mut url = endpoint(&self.base_url, &format!("Items/{raw_item_id}/Ancestors"))?;
        url.query_pairs_mut().append_pair("UserId", &self.user_id);
        let ancestors = match self.get_json::<Vec<Value>>(url).await {
            Ok(ancestors) => ancestors,
            Err(error) => {
                crate::source::optional_collection_error(error)?;
                return Ok(None);
            }
        };
        Ok(Some(self.music_folders(source_id, &ancestors)?))
    }

    fn music_folders(
        &self,
        source_id: &str,
        values: &[Value],
    ) -> library::LibraryResult<Vec<library::FolderObservation>> {
        let mut folders = Vec::new();
        for folder in values.iter().filter(|item| {
            item["CollectionType"]
                .as_str()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("music"))
        }) {
            let Some(name) = field::<String>(&folder, "Name") else {
                continue;
            };
            let Some(raw_folder_id) = id(&folder["Id"]) else {
                continue;
            };
            let folder_id = self.kind.object_id("music-folder", &raw_folder_id);
            let artwork = primary_image_ref(self.kind, "music-folder", &raw_folder_id, &folder)
                .as_ref()
                .map(|image| crate::native_artwork_binding(source_id, image))
                .transpose()?;
            folders.push(library::FolderObservation {
                metadata: library::FolderMetadata {
                    name: Some(name.clone()),
                    sort_text: Some(name.to_lowercase()),
                    ..Default::default()
                },
                object_id: folder_id,
                artwork_binding: artwork,
                ..Default::default()
            });
        }
        Ok(folders)
    }

    pub(crate) async fn acquire_catalog(
        &self,
        catalog: &mut library::Catalog<'_>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = catalog.source_id;
        let stamp = catalog.observed;

        for (entity, stage_name, paths) in [
            ("album", SourceReadStage::Albums, &["MusicAlbum"][..]),
            ("track", SourceReadStage::Tracks, &["Audio"][..]),
            (
                "artist",
                SourceReadStage::Artists,
                &["Artists", "Artists/AlbumArtists"][..],
            ),
            ("genre", SourceReadStage::Genres, &["MusicGenres"][..]),
        ] {
            let mut complete = true;
            for path in paths {
                let mut pages = PageState::default();
                loop {
                    check_cancelled(cancelled)?;
                    let response = match entity {
                        "artist" => {
                            self.people_page(path, pages.offset(), COLLECTION_PAGE_SIZE)
                                .await
                        }
                        "genre" => {
                            self.music_genre_page(pages.offset(), COLLECTION_PAGE_SIZE)
                                .await
                        }
                        _ => {
                            self.item_page(path, pages.offset(), COLLECTION_PAGE_SIZE)
                                .await
                        }
                    }
                    .and_then(|page| {
                        let finished = pages.advance(
                            items(&page["Items"]).len(),
                            field(&page, "TotalRecordCount"),
                        )?;
                        Ok((page, finished))
                    });
                    check_cancelled(cancelled)?;
                    let (page, finished) = match response {
                        Ok(page) => page,
                        Err(error) if entity == "genre" => {
                            crate::source::optional_collection_error(error)?;
                            complete = false;
                            break;
                        }
                        Err(error) => return Err(error),
                    };
                    let mut batch = library::CatalogBatch::default();
                    for item in items(&page["Items"]) {
                        match entity {
                            "album" => batch
                                .albums
                                .extend(album_from_item(self.kind, source_id, item)?),
                            "track" => batch
                                .tracks
                                .extend(track_from_item(self.kind, source_id, item)?),
                            "artist" => batch
                                .artists
                                .extend(artist_from_item(self.kind, source_id, item)?),
                            "genre" => batch
                                .genres
                                .extend(genre_from_item(self.kind, source_id, item)?),
                            _ => unreachable!(),
                        }
                    }
                    check_cancelled(cancelled)?;
                    catalog.apply_catalog(&batch, true).await?;
                    progress(stage(stage_name, pages.offset(), pages.total()));
                    if finished {
                        break;
                    }
                }
            }
            if entity == "artist" {
                self.acquire_missing_artists(catalog, cancelled).await?;
            }
            if complete && let Some(stamp) = stamp {
                check_cancelled(cancelled)?;
                catalog
                    .complete_source_observation(stamp, &[entity], None)
                    .await?;
            }
            if entity == "track" {
                let folders = self.acquire_music_folders(catalog, cancelled).await?;
                for folder in folders {
                    self.acquire_music_folder_memberships(catalog, &folder, cancelled)
                        .await?;
                }
                if let Some(stamp) = stamp {
                    check_cancelled(cancelled)?;
                    catalog
                        .complete_source_observation(stamp, &["folder"], None)
                        .await?;
                }
            }
        }

        if let Err(error) = self.acquire_playlists(catalog, progress, cancelled).await {
            crate::source::optional_collection_error(error)?;
        }
        crate::source::acquire_home(catalog, cancelled, |section| self.home_section(section))
            .await?;

        progress(stage(SourceReadStage::Finalizing, 1, Some(1)));
        Ok(())
    }

    async fn acquire_missing_artists(
        &self,
        catalog: &mut library::Catalog<'_>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let database = catalog.database;
        let source_id = catalog.source_id;

        loop {
            check_cancelled(cancelled)?;
            let missing = database.unresolved_source_artist_ids(source_id).await?;
            if missing.is_empty() {
                break;
            }
            let mut batch = library::CatalogBatch::default();
            let result: SourceResult<()> = async {
                for id in missing {
                    check_cancelled(cancelled)?;
                    let mut url = self.item_url(raw_item_id(&id))?;
                    url.query_pairs_mut()
                        .append_pair("UserId", &self.user_id)
                        .append_pair("Fields", MIXED_ITEM_FIELDS);
                    let mut artist = self.get_json::<Value>(url).await?;
                    if crate::remote_json::id(&artist["Id"]).is_none() {
                        if let Some(fields) = artist.as_object_mut() {
                            fields.insert("Id".into(), Value::String(raw_item_id(&id).to_string()));
                        } else {
                            artist = serde_json::json!({"Id":raw_item_id(&id)});
                        }
                    }
                    if let Some(mapped) = artist_from_item(self.kind, source_id, &artist)? {
                        batch.artists.push(mapped);
                    }
                }
                Ok(())
            }
            .await;
            check_cancelled(cancelled)?;
            catalog.apply_catalog(&batch, true).await?;
            result?;
        }
        Ok(())
    }

    async fn acquire_music_folders(
        &self,
        catalog: &mut library::Catalog<'_>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<Vec<String>> {
        let source_id = catalog.source_id;

        check_cancelled(cancelled)?;
        let mut url = endpoint(&self.base_url, &format!("Users/{}/Views", self.user_id))?;
        url.query_pairs_mut()
            .append_pair("IncludeExternalContent", "false");
        let response = self.get_json::<Value>(url).await?;
        let batch = library::CatalogBatch {
            folders: self.music_folders(source_id, items(&response["Items"]))?,
            ..Default::default()
        };
        check_cancelled(cancelled)?;
        catalog.apply_catalog(&batch, true).await?;
        Ok(batch
            .folders
            .into_iter()
            .map(|folder| folder.object_id)
            .collect())
    }

    async fn acquire_music_folder_memberships(
        &self,
        catalog: &mut library::Catalog<'_>,
        folder_id: &str,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let stamp = catalog.observed;

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
            let page = self.get_json::<Value>(url).await?;
            let count = items(&page["Items"]).len();
            let finished = pages.advance(count, field(&page, "TotalRecordCount"))?;
            let mut batch = library::CatalogBatch::default();
            batch
                .folders
                .extend_from_slice(&[library::FolderObservation {
                    object_id: folder_id.to_owned(),
                    tracks: items(&page["Items"])
                        .iter()
                        .filter_map(|item| {
                            id(&item["Id"]).map(|raw| self.kind.object_id("track", &raw))
                        })
                        .collect(),
                    ..Default::default()
                }]);
            check_cancelled(cancelled)?;
            catalog.apply_catalog(&batch, true).await?;
            if finished {
                if let Some(stamp) = stamp {
                    check_cancelled(cancelled)?;
                    catalog
                        .complete_source_folder_membership(&folder_id, stamp)
                        .await?;
                }
                return Ok(());
            }
        }
    }

    async fn acquire_playlists(
        &self,
        catalog: &mut library::Catalog<'_>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = catalog.source_id;
        let stamp = catalog.observed;

        let mut pages = PageState::default();
        let mut seen = HashSet::new();
        loop {
            check_cancelled(cancelled)?;
            let page = self
                .item_page("Playlist", pages.offset(), COLLECTION_PAGE_SIZE)
                .await?;
            let count = items(&page["Items"]).len();
            let finished = pages.advance(count, field(&page, "TotalRecordCount"))?;
            for item in items(&page["Items"]) {
                let Some(playlist) = playlist_from_item(self.kind, item) else {
                    continue;
                };
                if !seen.insert(playlist.id.clone()) {
                    continue;
                }
                let artwork = playlist
                    .image_ref
                    .as_ref()
                    .map(|image| crate::native_artwork_binding(source_id, image))
                    .transpose()?;
                let mut batch = library::CatalogBatch::default();
                batch.playlists.push(library::PlaylistObservation {
                    object_id: (&playlist.id).to_string(),
                    name: (&playlist.name).to_string(),
                    sort_text: (&playlist.name.to_lowercase()).to_string(),
                    artwork_binding: (artwork.as_deref()).map(|value| value.to_vec()),
                    ..Default::default()
                });
                check_cancelled(cancelled)?;
                catalog.apply_catalog(&batch, true).await?;
                self.acquire_playlist_entries(catalog, &playlist.id, cancelled)
                    .await?;
            }
            progress(stage(
                SourceReadStage::Playlists,
                pages.offset(),
                pages.total(),
            ));
            if finished {
                if let Some(stamp) = stamp {
                    check_cancelled(cancelled)?;
                    catalog
                        .complete_source_observation(stamp, &["playlist"], None)
                        .await?;
                }
                return Ok(());
            }
        }
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
            if self
                .total
                .or(total)
                .is_some_and(|total| self.offset < total)
            {
                return Err(SourceError::Other(
                    "Jellyfin omitted a catalog page".to_string(),
                ));
            }
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

    use crate::jellyfin_emby::{JellyfinEmbySource, JellyfinEmbySourceConfig};
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
    async fn live_updates_refresh_newly_added_and_retain_it_on_failure() {
        for kind in [crate::ServerKind::Jellyfin, crate::ServerKind::Emby] {
            let server = MockServer::start().await;
            let source = JellyfinEmbySource::open(
                JellyfinEmbySourceConfig {
                    emby_connect: false,
                    kind,
                    base_url: server.uri(),
                    server_id: Some("server".into()),
                    user_id: "user".into(),
                    username: "listener".into(),
                    trust_invalid_cert: false,
                    use_instant_mix: false,
                },
                "token".into(),
                "device".into(),
            )
            .unwrap();
            let source = crate::Source::new(
                crate::SourceId::new("source"),
                crate::source::Implementation::JellyfinEmby(source),
            );
            let directory = tempfile::tempdir().unwrap();
            let database = library::Database::open(directory.path().join("library.sqlite"))
                .await
                .unwrap();
            let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
            database.observe_catalog(move |_, update| {
                let _ = sender.send(update);
                std::future::ready(())
            });
            database
                .reconcile_source(&library::SourceId::new("source"), "source", "Source")
                .await
                .unwrap();

            let cancellation = library::ReadCancellation::new();
            for (id, status, empty, identical, expected) in [
                ("old", 200, false, false, vec!["old"]),
                ("new", 200, false, false, vec!["new"]),
                ("new", 200, false, true, vec!["new"]),
                ("third", 503, false, false, vec!["new"]),
                ("third", 200, true, false, vec!["new", "old", "third"]),
            ] {
                server.reset().await;
                let item = serde_json::json!({"Id":id,"Name":id,"Type":"MusicAlbum"});
                Mock::given(method("GET"))
                    .and(path(match kind {
                        crate::ServerKind::Jellyfin => format!("/Items/{id}"),
                        crate::ServerKind::Emby => format!("/emby/Users/user/Items/{id}"),
                    }))
                    .respond_with(ResponseTemplate::new(200).set_body_json(item.clone()))
                    .expect(1)
                    .mount(&server)
                    .await;
                Mock::given(method("GET")).and(path(match kind {
                    crate::ServerKind::Jellyfin => "/Items", crate::ServerKind::Emby => "/emby/Items",
                })).and(query_param("ParentId", id)).and(query_param("IncludeItemTypes", "Audio"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"Items":[{"Id":format!("{id}-track"),"Name":"Track","Type":"Audio","AlbumId":id,"Album":id}]})))
                    .mount(&server).await;
                let entries = if empty { vec![] } else { vec![item] };
                Mock::given(method("GET"))
                    .and(path(match kind {
                        crate::ServerKind::Jellyfin => "/Items",
                        crate::ServerKind::Emby => "/emby/Items",
                    }))
                    .and(query_param("SortBy", "DateCreated,SortName"))
                    .and(query_param("Limit", "24"))
                    .respond_with(ResponseTemplate::new(status).set_body_json(
                        serde_json::json!({"Items":entries,"TotalRecordCount":entries.len()}),
                    ))
                    .expect(1)
                    .mount(&server)
                    .await;
                source
                    .apply_items(&database, vec![id.into()], vec![])
                    .await
                    .unwrap();
                if identical {
                    assert!(updates.try_recv().is_err());
                } else {
                    assert!(updates.try_recv().expect("catalog update").changed());
                }
                let home = database
                    .home_page(
                        database
                            .source_identity_key(&library::SourceId::new("source"))
                            .await
                            .unwrap()
                            .unwrap(),
                        None,
                        0,
                        0,
                        &[library::HomeBlockKind::NewlyAdded],
                        &cancellation,
                    )
                    .await
                    .unwrap();
                assert!(
                    home.newly_added
                        .albums
                        .iter()
                        .all(|album| album.album.summary.track_count == 1)
                );
                let mut titles = home
                    .newly_added
                    .albums
                    .iter()
                    .map(|row| row.title.as_str())
                    .collect::<Vec<_>>();
                titles.sort_unstable();
                assert_eq!(titles, expected);
                server.verify().await;
            }
        }
    }

    #[tokio::test]
    async fn catalog_sync_keeps_optional_facts_and_advances_past_unusable_entries() {
        let server = MockServer::start().await;
        for endpoint in ["/Users/user/Views", "/Artists", "/Artists/AlbumArtists"] {
            Mock::given(method("GET"))
                .and(path(endpoint))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(if endpoint == "/Users/user/Views" {
                            serde_json::json!({"Items":[],"TotalRecordCount":0})
                        } else {
                            serde_json::json!({"Items":[{"Id":"listed","Name":"Artist","UserData":{"IsFavorite":false}}],"TotalRecordCount":1})
                        }),
                )
                .mount(&server)
                .await;
        }
        for (kind, item) in [
            (
                "MusicAlbum",
                serde_json::json!({"Id":"album","Name":"Album","Type":"MusicAlbum"}),
            ),
            (
                "Audio",
                serde_json::json!({"Id":"kept","Name":"Track","Type":"Audio","AlbumId":"album","Album":"Album",
                    "ProductionYear":{}, "RunTimeTicks":"broken", "UserData":{"Rating":{},"IsFavorite":true,"PlayCount":7},
                    "ImageTags":{"Primary":"cover","Broken":[]}, "ProviderIds":{"MusicBrainzRecording":"recording","Other":{}},
                    "NormalizationGain":-4.25, "AlbumNormalizationGain":-3.5,
                    "ArtistItems":[{"Id":"credited","Name":"Artist"},{"Id":"listed","Name":"Artist"},{"Name":42},null]}),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path("/Items"))
                .and(query_param("IncludeItemTypes", kind))
                .and(query_param("Limit", "500"))
                .and(query_param("StartIndex", "0"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(if kind == "Audio" {
                            serde_json::json!({"Items":[item,{"Name":"No identity"}],"TotalRecordCount":3})
                        } else { serde_json::json!({"Items":[item],"TotalRecordCount":1}) }),
                )
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/Items"))
            .and(query_param("IncludeItemTypes", "Audio"))
            .and(query_param("StartIndex", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items":[{"Id":"next","Name":"Next Track","Type":"Audio","AlbumId":"album"}],"TotalRecordCount":3
            })))
            .expect(2)
            .mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/Items/credited"))
            .and(query_param("UserId", "user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Name":"Artist","UserData":{"IsFavorite":true}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let source = JellyfinEmbySource::open(
            JellyfinEmbySourceConfig {
                emby_connect: false,
                kind: crate::ServerKind::Jellyfin,
                base_url: server.uri(),
                server_id: Some("server".into()),
                user_id: "user".into(),
                username: "listener".into(),
                trust_invalid_cert: false,
                use_instant_mix: false,
            },
            "token".into(),
            "device".into(),
        )
        .unwrap();
        let owner = crate::Source::new(
            crate::SourceId::new("source"),
            crate::source::Implementation::JellyfinEmby(source),
        );
        let crate::source::Implementation::JellyfinEmby(source) = &owner.implementation else {
            unreachable!()
        };

        let root = tempfile::tempdir().unwrap();
        let database = library::Database::open(root.path().join("library.sqlite"))
            .await
            .unwrap();
        let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |_, update| {
            let _ = sender.send(update);
            std::future::ready(())
        });
        database
            .reconcile_source(&library::SourceId::new("source"), "jellyfin", "Jellyfin")
            .await
            .unwrap();
        let catalog_source_id = ("source").to_string();
        let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
        catalog.start_observation().await.unwrap();
        (async {
            let mut batch = library::CatalogBatch::default();
            let result = batch.albums.extend_from_slice(&[(super::album_from_item(
                crate::ServerKind::Jellyfin,
                &catalog_source_id,
                &serde_json::from_value(
                    serde_json::json!({"Id":"album","Name":"Album","Type":"MusicAlbum"}),
                )
                .unwrap(),
            )
            .unwrap()
            .unwrap())
            .clone()]);
            catalog.apply_catalog(&batch, true).await?;
            Ok::<_, library::LibraryError>(result)
        })
        .await
        .unwrap();
        (async { let mut batch=library::CatalogBatch::default(); let result = batch.tracks.extend_from_slice(&[(super::track_from_item(crate::ServerKind::Jellyfin, &catalog_source_id, &serde_json::from_value(serde_json::json!({"Id":"removed","Name":"Removed","Type":"Audio","AlbumId":"album"})).unwrap()).unwrap().unwrap()).clone()]); catalog.apply_catalog(&batch, true).await?; Ok::<_,library::LibraryError>(result) }).await.unwrap();
        (async {
            let mut batch = library::CatalogBatch::default();
            let result = batch
                .genres
                .extend_from_slice(&[(library::GenreObservation {
                    object_id: "cached-genre".into(),
                    metadata: library::TagMetadata {
                        name: "Cached".into(),
                        sort_text: Some("cached".into()),
                    },
                    ..Default::default()
                })
                .clone()]);
            catalog.apply_catalog(&batch, true).await?;
            Ok::<_, library::LibraryError>(result)
        })
        .await
        .unwrap();
        (async {
            let mut batch = library::CatalogBatch::default();
            let result = batch.playlists.push(library::PlaylistObservation {
                object_id: ("cached-playlist").to_string(),
                name: ("Cached").to_string(),
                sort_text: ("cached").to_string(),
                ..Default::default()
            });
            catalog.apply_catalog(&batch, true).await?;
            Ok::<_, library::LibraryError>(result)
        })
        .await
        .unwrap();
        catalog
            .write_source_playlist_page(
                "cached-playlist",
                0,
                &[library::PlaylistEntryObservation {
                    object_id: "occurrence".into(),
                    track_id: "jellyfin:track:removed".into(),
                }],
                true,
            )
            .await
            .unwrap();
        (async {
            let batch = library::CatalogBatch::default();
            catalog
                .replace_source_home_section(
                    "recently-played",
                    None,
                    &[library::HomeEntryInput {
                        position: 0,
                        kind: library::HomeEntryKind::Album,
                        entity_object_id: "jellyfin:album:album".into(),
                        title: "Album".into(),
                        subtitle: String::new(),
                    }],
                )
                .await?;
            catalog.apply_catalog(&batch, true).await?;
            Ok::<_, library::LibraryError>(())
        })
        .await
        .unwrap();
        {
            database
                .publish_catalog_update(&catalog_source_id, catalog.changes().clone())
                .await;
            Ok::<_, crate::SourceError>(())
        }
        .unwrap();
        updates.try_recv().expect("initial catalog update");
        database
            .reconcile_source(&library::SourceId::new("source"), "jellyfin", "Jellyfin")
            .await
            .unwrap();
        let catalog_source_id = ("source").to_string();
        let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
        catalog.start_observation().await.unwrap();

        source
            .acquire_catalog(&mut catalog, &|_| {}, &|| false)
            .await
            .unwrap();
        {
            database
                .publish_catalog_update(&catalog_source_id, catalog.changes().clone())
                .await;
            Ok::<_, crate::SourceError>(())
        }
        .unwrap();
        let publication = updates.try_recv().expect("catalog update");
        assert!(publication.metadata);
        let cancellation = library::ReadCancellation::new();
        for object in ["kept", "next"] {
            let uri = library::source_entity_uri(
                &crate::SourceId::new("source"),
                "track",
                &format!("jellyfin:track:{object}"),
            );
            let track = database
                .track(&(&uri), &cancellation)
                .await
                .unwrap()
                .unwrap();
            if object == "kept" {
                assert!(track.state.favorite);
                assert_eq!(track.state.play_count, 7);
                assert_eq!(track.state.rating, None);
                assert_eq!(
                    track.metadata.musicbrainz_recording_id.as_deref(),
                    Some("recording")
                );
                assert!(track.artwork_binding.is_some());
                assert_eq!(track.links.artists.len(), 2);
                let (track_gain, album_gain) = database
                    .playback_loudness(&uri, &cancellation)
                    .await
                    .unwrap();
                assert_eq!(track_gain.unwrap().replay_gain_db, Some(-4.25));
                assert_eq!(album_gain.unwrap().replay_gain_db, Some(-3.5));
            }
        }
        for (id, favorite) in [("credited", true), ("listed", false)] {
            let uri = library::source_entity_uri(
                &crate::SourceId::new("source"),
                "artist",
                &format!("jellyfin:artist:{id}"),
            );
            let artist = database
                .artist(&(&uri), false, false, &cancellation)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(artist.state.favorite, favorite);
        }
        let removed = library::source_entity_uri(
            &crate::SourceId::new("source"),
            "track",
            "jellyfin:track:removed",
        );
        assert!(
            database
                .tracks_by_uris(&([(&removed).to_owned()]), &cancellation)
                .await
                .map(|rows| { rows.into_iter().next() })
                .unwrap()
                .is_some()
        );
        let catalog = (library::MemberRead::new(library::Collection::Source(
            database
                .source_identity_key(&library::SourceId::new(&catalog_source_id))
                .await
                .unwrap()
                .unwrap(),
        )))
        .page(&database, library::RouteSeedWindow::top(), &cancellation)
        .await
        .map(|page| {
            page.rows
                .into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .unwrap();
        assert_eq!(catalog.len(), 2);
        assert!(catalog.iter().all(|track| track.media_uri != removed));
        assert!(
            database
                .catalog_keys(
                    library::CatalogKind::Genre,
                    Some(
                        database
                            .source_identity_key(&library::SourceId::new(&catalog_source_id))
                            .await
                            .unwrap()
                            .unwrap()
                    ),
                    &["cached-genre".into()],
                    &cancellation
                )
                .await
                .unwrap()
                .into_iter()
                .next()
                .is_some()
        );
        assert!(
            database
                .playlist_key_by_object(
                    database
                        .source_identity_key(&library::SourceId::new(&catalog_source_id))
                        .await
                        .unwrap()
                        .unwrap(),
                    "cached-playlist",
                    &cancellation
                )
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            database
                .home_page(
                    database
                        .source_identity_key(&library::SourceId::new(&catalog_source_id))
                        .await
                        .unwrap()
                        .unwrap(),
                    None,
                    0,
                    0,
                    &library::HomeBlockKind::all(),
                    &cancellation
                )
                .await
                .unwrap()
                .recently_played
                .albums
                .len(),
            1
        );
        database
            .reconcile_source(&library::SourceId::new("source"), "jellyfin", "Jellyfin")
            .await
            .unwrap();
        let catalog_source_id = ("source").to_string();
        let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
        catalog.start_observation().await.unwrap();

        source
            .acquire_catalog(&mut catalog, &|_| {}, &|| false)
            .await
            .unwrap();
        {
            database
                .publish_catalog_update(&catalog_source_id, catalog.changes().clone())
                .await;
            Ok::<_, crate::SourceError>(())
        }
        .unwrap();
        assert!(!catalog.changes().changed());
        assert!(
            updates.try_recv().is_err(),
            "unchanged native data stays quiet"
        );
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
        let source = JellyfinEmbySource::open(
            JellyfinEmbySourceConfig {
                emby_connect: false,
                kind: crate::ServerKind::Jellyfin,
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
        let source = JellyfinEmbySource::open(
            JellyfinEmbySourceConfig {
                emby_connect: false,
                kind: crate::ServerKind::Jellyfin,
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
        let owner = crate::Source::new(
            crate::SourceId::new("jellyfin:test"),
            crate::source::Implementation::JellyfinEmby(source),
        );
        let crate::source::Implementation::JellyfinEmby(source) = &owner.implementation else {
            unreachable!()
        };

        let directory = tempfile::tempdir().expect("Library directory");
        let database = library::Database::open(directory.path().join("library.sqlite3"))
            .await
            .expect("Library database");
        let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |_, update| {
            let _ = sender.send(update);
            std::future::ready(())
        });
        database
            .reconcile_source(
                &library::SourceId::new("jellyfin:test"),
                "jellyfin",
                "Jellyfin",
            )
            .await
            .unwrap();
        let catalog_source_id = ("jellyfin:test").to_string();
        let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
        catalog.start_observation().await.unwrap();

        source
            .acquire_playlists(&mut catalog, &|_| {}, &|| false)
            .await
            .expect("stage overlapping Playlist pages");

        {
            database
                .publish_catalog_update(&catalog_source_id, catalog.changes().clone())
                .await;
            Ok::<_, crate::SourceError>(())
        }
        .expect("publish Playlists");
        let update = updates.try_recv().expect("catalog update");
        assert!(update.playlists);
    }
}

fn stage(stage: SourceReadStage, completed: usize, total: Option<usize>) -> SourceReadProgress {
    SourceReadProgress {
        stage,
        completed,
        total,
    }
}

pub(super) fn check_cancelled(cancelled: &(dyn Fn() -> bool + Send + Sync)) -> SourceResult<()> {
    if cancelled() {
        Err(SourceError::Cancelled)
    } else {
        Ok(())
    }
}
