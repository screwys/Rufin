use super::item::{ItemResponse, read_items};
use super::{PlexSource, plex_id};
use crate::policy::raw_item_id;
use crate::remote_json::{field, id, items};
use crate::source::{SourceReadProgress, SourceReadStage};
use crate::{SourceError, SourceHomeSection, SourceResult};
use library::{FolderObservation, HomeEntryInput, HomeEntryKind};
use serde_json::Value;

const PAGE: usize = 512;

impl PlexSource {
    pub(crate) async fn acquire_catalog(
        &self,
        catalog: &mut library::Catalog<'_>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let stamp = catalog.observed;

        let sections = self.get("/library/sections", &[]).await?;
        let home_prefixes = items(&sections["MediaContainer"]["Directory"])
            .iter()
            .filter(|section| section["type"].as_str() == Some("artist"))
            .filter_map(|section| id(&section["key"]))
            .map(|section| format!("plex:{section}:"))
            .collect::<Vec<_>>();
        for section in items(&sections["MediaContainer"]["Directory"]) {
            if section["type"].as_str() != Some("artist") {
                continue;
            }
            let Some(section_id) = id(&section["key"]) else {
                continue;
            };
            let title = section["title"].as_str().unwrap_or(&section_id);
            let mut section_record = FolderObservation {
                metadata: library::FolderMetadata {
                    name: Some(title.to_owned()),
                    ..Default::default()
                },
                object_id: plex_id("music-folder", &section_id),
                ..Default::default()
            };
            let mut batch = library::CatalogBatch::default();
            batch
                .folders
                .extend_from_slice(std::slice::from_ref(&section_record));
            if cancelled() {
                return Err(crate::SourceError::Cancelled);
            }
            catalog.apply_catalog(&batch, true).await?;
            for (kind, stage) in [
                ("8", SourceReadStage::Artists),
                ("9", SourceReadStage::Albums),
                ("10", SourceReadStage::Tracks),
            ] {
                let mut offset = 0;
                loop {
                    if cancelled() {
                        return Err(SourceError::Cancelled);
                    }
                    let page = self
                        .get(
                            &format!("/library/sections/{section_id}/all"),
                            &[
                                ("type", kind.into()),
                                ("includeGuids", "1".into()),
                                ("includeStreams", "1".into()),
                                ("X-Plex-Container-Start", offset.to_string()),
                                ("X-Plex-Container-Size", PAGE.to_string()),
                            ],
                        )
                        .await?;
                    let rows = items(&page["MediaContainer"]["Metadata"]);
                    let finished = page_finished(&page, offset, rows.len())?;
                    section_record.tracks = if kind == "10" {
                        rows.iter()
                            .filter_map(|item| id(&item["ratingKey"]))
                            .map(|raw| plex_id("track", &raw))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    self.acquire_metadata_page(catalog, rows, Some(&section_record), cancelled)
                        .await?;
                    offset += rows.len();
                    progress(SourceReadProgress {
                        stage,
                        completed: offset,
                        total: field(&page["MediaContainer"], "totalSize"),
                    });
                    if finished {
                        break;
                    }
                }
            }
            if let Some(stamp) = stamp {
                if cancelled() {
                    return Err(crate::SourceError::Cancelled);
                }
                catalog
                    .complete_source_folder_membership(&section_record.object_id, stamp)
                    .await?;
            }
            self.acquire_hubs(catalog, &section_id, cancelled).await?;
        }
        if let Some(stamp) = stamp {
            if cancelled() {
                return Err(crate::SourceError::Cancelled);
            }
            catalog
                .complete_source_observation(stamp, &["track", "album", "artist", "folder"], None)
                .await?;
        }
        if cancelled() {
            return Err(crate::SourceError::Cancelled);
        }
        catalog
            .complete_source_home_sections(&"plex:", &Vec::new(), &home_prefixes)
            .await?;
        self.acquire_playlists(catalog, cancelled).await?;
        crate::source::acquire_home(catalog, cancelled, |section| self.home_section(section))
            .await?;

        progress(SourceReadProgress {
            stage: SourceReadStage::Finalizing,
            completed: 1,
            total: Some(1),
        });
        Ok(())
    }

    pub(super) async fn acquire_metadata_page(
        &self,
        catalog: &mut library::Catalog<'_>,
        rows: &[Value],
        folder: Option<&FolderObservation>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = catalog.source_id;

        let ids = rows
            .iter()
            .filter(|item| item["type"] == "track")
            .filter_map(|item| id(&item["ratingKey"]))
            .collect::<Vec<_>>();
        let details = if ids.is_empty() {
            Value::Null
        } else {
            self.get(
                &format!("/library/metadata/{}", ids.join(",")),
                &[("includeStreams", "1".into()), ("includeGuids", "1".into())],
            )
            .await?
        };
        let details = items(&details["MediaContainer"]["Metadata"])
            .iter()
            .filter_map(|item| id(&item["ratingKey"]).map(|id| (id, item)))
            .collect::<std::collections::HashMap<_, _>>();
        let completed = rows
            .iter()
            .map(|item| {
                let mut native = item.clone();
                let mut response = if item["type"] == "track" {
                    ItemResponse::Summary
                } else {
                    ItemResponse::Complete
                };
                if let Some(detail) = id(&item["ratingKey"]).and_then(|id| details.get(&id))
                    && let (Some(native), Some(detail)) =
                        (native.as_object_mut(), detail.as_object())
                {
                    native.extend(detail.clone());
                    response = ItemResponse::Complete;
                }
                (native, response)
            })
            .collect::<Vec<_>>();
        for mut batch in read_items(
            source_id,
            completed.iter().map(|(item, response)| (item, *response)),
        )? {
            if let Some(folder) = folder {
                batch.folders.push(folder.clone());
            }
            if cancelled() {
                return Err(SourceError::Cancelled);
            }
            catalog.apply_catalog(&batch, true).await?;
        }
        Ok(())
    }

    async fn acquire_playlists(
        &self,
        catalog: &mut library::Catalog<'_>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = catalog.source_id;

        let mut offset = 0;
        loop {
            if cancelled() {
                return Err(SourceError::Cancelled);
            }
            let page = match self
                .get(
                    "/playlists",
                    &[
                        ("playlistType", "audio".into()),
                        ("X-Plex-Container-Start", offset.to_string()),
                        ("X-Plex-Container-Size", PAGE.to_string()),
                    ],
                )
                .await
            {
                Ok(page) => page,
                Err(error) => {
                    crate::source::optional_collection_error(error)?;

                    return Ok(());
                }
            };
            let rows = items(&page["MediaContainer"]["Metadata"]);
            let finished = page_finished(&page, offset, rows.len())?;
            let playlists = rows
                .iter()
                .filter(|item| !is_station(item))
                .collect::<Vec<_>>();

            {
                for batch in read_items(
                    source_id,
                    playlists.iter().map(|item| (*item, ItemResponse::Complete)),
                )? {
                    if cancelled() {
                        return Err(crate::SourceError::Cancelled);
                    }
                    catalog.apply_catalog(&batch, true).await?;
                }
            };

            for item in playlists {
                if let Some(raw) = id(&item["ratingKey"]) {
                    self.acquire_playlist_entries(catalog, &plex_id("playlist", &raw), cancelled)
                        .await?;
                }
            }
            offset += rows.len();
            if finished {
                return Ok(());
            }
        }
    }

    pub(crate) async fn acquire_playlist_entries(
        &self,
        catalog: &mut library::Catalog<'_>,
        playlist_id: &str,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = catalog.source_id;

        let mut offset = 0;
        loop {
            let page = self
                .get(
                    &format!("/playlists/{}/items", raw_item_id(playlist_id)),
                    &[
                        ("X-Plex-Container-Start", offset.to_string()),
                        ("X-Plex-Container-Size", PAGE.to_string()),
                    ],
                )
                .await?;
            let rows = items(&page["MediaContainer"]["Metadata"]);
            let finished = page_finished(&page, offset, rows.len())?;
            let mut entries = Vec::with_capacity(rows.len());
            let mut native = Vec::with_capacity(rows.len());
            for item in rows {
                if item["type"].as_str() != Some("track") {
                    continue;
                }
                let (Some(raw), Some(entry)) =
                    (id(&item["ratingKey"]), id(&item["playlistItemID"]))
                else {
                    continue;
                };
                native.push((item, ItemResponse::Summary));
                entries.push(library::PlaylistEntryObservation {
                    object_id: entry,
                    track_id: plex_id("track", &raw),
                });
            }

            {
                for batch in read_items(source_id, native)? {
                    if cancelled() {
                        return Err(crate::SourceError::Cancelled);
                    }
                    catalog.apply_catalog(&batch, false).await?;
                }
            };
            if cancelled() {
                return Err(crate::SourceError::Cancelled);
            }
            catalog
                .write_source_playlist_page(&playlist_id, offset, &entries, finished)
                .await?;
            offset += rows.len();
            if finished {
                return Ok(());
            }
        }
    }

    pub(crate) async fn acquire_items(
        &self,
        catalog: &mut library::Catalog<'_>,
        upserts: Vec<String>,
        mut removals: Vec<String>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = catalog.source_id;

        for raw in upserts {
            let item = match self.metadata(&raw).await {
                Ok(item) => item,
                Err(SourceError::NotFound) => {
                    removals.push(raw);
                    continue;
                }
                Err(error) => return Err(error),
            };
            if item["type"] == "album" {
                self.acquire_collection(
                    catalog,
                    &crate::SourceCollection::Album(plex_id("album", raw_item_id(&raw))),
                    Some(&item),
                    true,
                    cancelled,
                )
                .await?;
                continue;
            }
            let album = if item["type"] == "track" {
                if let Some(album) = id(&item["parentRatingKey"]) {
                    match self.metadata(&album).await {
                        Ok(album) => Some(album),
                        Err(error) => {
                            crate::source::optional_collection_error(error)?;
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };

            {
                for batch in read_items(
                    source_id,
                    album
                        .iter()
                        .chain(std::iter::once(&item))
                        .map(|item| (item, ItemResponse::Complete)),
                )? {
                    if cancelled() {
                        return Err(crate::SourceError::Cancelled);
                    }
                    catalog.apply_catalog(&batch, true).await?;
                }
            };

            if item["type"] == "playlist"
                && let Some(id) = id(&item["ratingKey"])
            {
                self.acquire_playlist_entries(catalog, &plex_id("playlist", &id), cancelled)
                    .await?;
            }
        }

        for raw in removals {
            if cancelled() {
                return Err(crate::SourceError::Cancelled);
            }
            for kind in ["track", "album", "artist", "playlist"] {
                catalog
                    .remove_source_items(kind, &[plex_id(kind, raw_item_id(&raw))])
                    .await?;
            }
        }

        Ok(())
    }

    pub(crate) async fn home_section(
        &self,
        section: SourceHomeSection,
    ) -> SourceResult<Vec<HomeEntryInput>> {
        let (kind, sort) = match section {
            SourceHomeSection::MostPlayed => ("10", "viewCount:desc"),
            SourceHomeSection::RecentlyPlayed => ("10", "lastViewedAt:desc"),
            SourceHomeSection::NewlyAdded => ("9", "addedAt:desc"),
            SourceHomeSection::RecentlyReleased => ("9", "originallyAvailableAt:desc"),
        };
        let page = self
            .get(
                "/library/all",
                &[
                    ("type", kind.into()),
                    ("sort", sort.into()),
                    ("X-Plex-Container-Start", "0".into()),
                    ("X-Plex-Container-Size", "24".into()),
                ],
            )
            .await?;
        Ok(items(&page["MediaContainer"]["Metadata"])
            .iter()
            .take(24)
            .enumerate()
            .filter_map(|(position, item)| home_entry(position, item))
            .collect())
    }

    async fn acquire_hubs(
        &self,
        catalog: &mut library::Catalog<'_>,
        section: &str,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = catalog.source_id;

        let page = match self
            .get(
                &format!("/hubs/sections/{section}"),
                &[
                    ("includeStations", "1".into()),
                    ("includeMyMixes", "1".into()),
                    ("count", "24".into()),
                ],
            )
            .await
        {
            Ok(page) => page,
            Err(error) => {
                crate::source::optional_collection_error(error)?;

                return Ok(());
            }
        };
        let mut acquire_hub = async |section_id: &str, title: Option<&str>, rows: &[Value]| {
            let mut entries = Vec::new();
            let mut native = Vec::new();
            for (position, item) in rows.iter().take(24).enumerate() {
                if let Some(entry) = home_entry(position, item) {
                    native.push((item, ItemResponse::Summary));
                    entries.push(entry);
                }
            }
            for batch in read_items(source_id, native)? {
                if cancelled() {
                    return Err(SourceError::Cancelled);
                }
                catalog.apply_catalog(&batch, false).await?;
            }
            if cancelled() {
                return Err(SourceError::Cancelled);
            }
            catalog
                .replace_source_home_section(section_id, title, &entries)
                .await?;
            Ok::<_, SourceError>(())
        };
        let mut sections = Vec::new();
        for hub in items(&page["MediaContainer"]["Hub"]) {
            let Some(identifier) = hub["hubIdentifier"].as_str() else {
                continue;
            };
            let section_id = format!("plex:{section}:{identifier}");
            sections.push(section_id.clone());
            acquire_hub(&section_id, hub["title"].as_str(), items(&hub["Metadata"])).await?;

            for station in items(&hub["Directory"])
                .iter()
                .chain(items(&hub["Metadata"]))
                .filter(|item| is_station(item))
                .take(12)
            {
                let Some(key) = station["key"].as_str() else {
                    continue;
                };
                let mix_section = format!("{section_id}:{}", crate::policy::stable_hash(key));
                sections.push(mix_section.clone());
                let candidates = match self.station_candidates(key, 24).await {
                    Ok(rows) => rows,
                    Err(error) => {
                        crate::source::optional_collection_error(error)?;

                        continue;
                    }
                };
                acquire_hub(&mix_section, station["title"].as_str(), &candidates).await?;
            }
        }
        if cancelled() {
            return Err(SourceError::Cancelled);
        }
        catalog
            .complete_source_home_sections(&format!("plex:{section}:"), &sections, &Vec::new())
            .await?;
        Ok(())
    }

    pub(super) async fn station_candidates(
        &self,
        key: &str,
        limit: usize,
    ) -> SourceResult<Vec<Value>> {
        let request = self
            .request(
                reqwest::Method::POST,
                "/playQueues",
                &[
                    ("type", "audio".into()),
                    (
                        "uri",
                        format!(
                            "server://{}/com.plexapp.plugins.library{key}",
                            self.config.server_id
                        ),
                    ),
                    ("window", limit.to_string()),
                    ("X-Plex-Container-Start", "0".into()),
                    ("X-Plex-Container-Size", limit.to_string()),
                ],
            )
            .await?;
        let response: Value =
            crate::remote_http::json(request, super::HTTP, super::RESPONSE).await?;
        Ok(items(&response["MediaContainer"]["Metadata"])
            .iter()
            .take(limit)
            .cloned()
            .collect())
    }
}

fn home_entry(position: usize, item: &Value) -> Option<HomeEntryInput> {
    let raw = id(&item["ratingKey"])?;
    let kind = match item["type"].as_str()? {
        "track" => HomeEntryKind::Track,
        "album" => HomeEntryKind::Album,
        _ => return None,
    };
    Some(HomeEntryInput {
        position: position as i64,
        kind,
        entity_object_id: plex_id(item["type"].as_str()?, &raw),
        title: item["title"].as_str().unwrap_or("Untitled").into(),
        subtitle: item["grandparentTitle"]
            .as_str()
            .or(item["parentTitle"].as_str())
            .unwrap_or_default()
            .into(),
    })
}

fn is_station(item: &Value) -> bool {
    crate::remote_json::boolean(&item["radio"])
        .or_else(|| field::<i64>(item, "radio").map(|v| v != 0))
        .unwrap_or(false)
        || item["type"].as_str() == Some("station")
}

pub(super) fn page_finished(page: &Value, offset: usize, count: usize) -> SourceResult<bool> {
    let total = field::<usize>(&page["MediaContainer"], "totalSize");
    if count == 0 && total.is_some_and(|total| offset < total) {
        return Err(SourceError::Other("Plex omitted a catalog page".into()));
    }
    Ok(total.map_or(count < PAGE, |total| offset + count >= total))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(crate) fn source(server: &MockServer) -> PlexSource {
        let login = super::super::PlexLogin::decode(&json!({"client_id":"test", "token":"token", 
            "account_id":"user", "profiles":{}, "resources":{"user":{"server":"token"}},"home_admin_subscription":false,"download_subscriptions":{}}).to_string()).unwrap();
        PlexSource::new(
            super::super::PlexSourceConfig {
                require_valid_certificate: false,
                version: 1,
                server_id: "server".into(),
                profile_id: "user".into(),
                base_url: server.uri(),
                address_override: None,
                local: true,
                relay: false,
                owned: true,
                trust_invalid_cert: false,
            },
            login,
        )
        .unwrap()
    }

    fn track() -> Value {
        json!({"type":"track","ratingKey":"track","title":"Song","parentRatingKey":"album","parentTitle":"Album","grandparentRatingKey":"artist","grandparentTitle":"Artist",
        "librarySectionID":"1","duration":123456,"addedAt":1704067200,"userRating":7,"viewCount":"12","lastViewedAt":1704067200,
        "year":{},"parentYear":2010,"index":[],"skipCount":-4,"Guid":[null,{"id":"mbid://recording"}],"Genre":[{"tag":"Rock"},{"tag":[]}],"Mood":[{"tag":"Calm"}],
        "Media":[{"container":"flac","Part":[{"file":"/music/song.flac","Stream":[{"streamType":2,"gain":-4.25,"peak":0.8,"albumGain":-3.5,"loudness":-18.0}]}]}],
        "thumb":"/library/metadata/album/thumb/1"})
    }

    #[tokio::test]
    async fn live_updates_refresh_newly_added_and_retain_it_on_failure() {
        let server = MockServer::start().await;
        let source = crate::Source::new(
            crate::SourceId::new("source"),
            crate::source::Implementation::Plex(source(&server)),
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
            let item = serde_json::json!({"ratingKey":id,"title":id,"type":"album"});
            Mock::given(method("GET"))
                .and(path(format!("/library/metadata/{id}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"MediaContainer":{"Metadata":[item.clone()]}}),
                ))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET")).and(path(format!("/library/metadata/{id}/children")))
                .respond_with(move |request: &wiremock::Request| {
                    let offset = request.url.query_pairs().find(|(key, _)| key == "X-Plex-Container-Start").unwrap().1;
                    ResponseTemplate::new(200).set_body_json(json!({"MediaContainer":{"totalSize":2,"Metadata":[{"ratingKey":format!("{id}-track-{offset}"),"title":"Track","type":"track","parentRatingKey":id,"parentTitle":id}]}}))
                }).expect(2)
                .mount(&server).await;
            for offset in 0..2 {
                let raw = format!("{id}-track-{offset}");
                Mock::given(method("GET"))
                    .and(path(format!("/library/metadata/{raw}")))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "MediaContainer":{"Metadata":[{"ratingKey":raw,"title":"Track",
                            "type":"track","parentRatingKey":id,"parentTitle":id}]}
                    })))
                    .expect(1)
                    .mount(&server)
                    .await;
            }
            let entries = if empty { vec![] } else { vec![item] };
            Mock::given(method("GET"))
                .and(path("/library/all"))
                .and(query_param("sort", "addedAt:desc"))
                .and(query_param("X-Plex-Container-Size", "24"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .set_body_json(serde_json::json!({"MediaContainer":{"Metadata":entries}})),
                )
                .expect(1)
                .mount(&server)
                .await;
            source
                .apply_items(&database, vec![id.into()], vec![])
                .await
                .unwrap();
            if identical {
                assert!(
                    updates.try_recv().is_err(),
                    "unchanged native data stays quiet"
                );
            } else {
                assert!(updates.try_recv().expect("catalog update").changed());
            }
            let home = database
                .home_page(
                    database
                        .source_identity_key(&crate::SourceId::new("source"))
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
                    .all(|album| album.album.summary.track_count == 2)
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

    #[tokio::test]
    async fn catalog_and_live_items_ingest_independent_facts_and_preserve_occurrences() {
        let server = MockServer::start().await;
        let source = source(&server);
        let source_owner = crate::Source::new(
            library::SourceId::new("plex-test"),
            crate::Implementation::Plex(source),
        );
        let crate::Implementation::Plex(source) = &source_owner.implementation else {
            unreachable!()
        };
        let valid_title = "İstanbul; 東京; العربية; Ёжик; Édith";
        let mut damaged_track = track();
        damaged_track["title"] = json!("__BAD_TITLE__");
        let mut damaged_detail =
            serde_json::to_vec(&json!({"MediaContainer":{"Metadata":[damaged_track]}})).unwrap();
        let start = damaged_detail
            .windows(13)
            .position(|value| value == b"__BAD_TITLE__")
            .unwrap();
        damaged_detail.splice(start..start + 13, b"Bad \xff title".iter().copied());
        let bulk_track = Mock::given(method("GET"))
            .and(path("/library/metadata/track"))
            .and(query_param("includeStreams", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(damaged_detail))
            .mount_as_scoped(&server)
            .await;
        Mock::given(method("GET")).and(path("/library/metadata/unanalyzed")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"MediaContainer":{"Metadata":[{"ratingKey":"unanalyzed","type":"track","title":valid_title,"Media":[{"Part":[{"Stream":[{"streamType":2,"gain":{},"peak":-1}]}]}]}]}}))).mount(&server).await;
        Mock::given(method("GET")).and(path("/library/sections")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"MediaContainer":{"Directory":[null,{"key":"1","type":"artist","title":"Music"}]}}))).mount(&server).await;
        for (kind, value) in [
            (
                "8",
                json!({"ratingKey":"artist","type":"artist","title":"Artist"}),
            ),
            (
                "9",
                json!({"ratingKey":"album","type":"album","title":"Album"}),
            ),
            ("10", track()),
        ] {
            let mut value = value;
            if kind == "10" {
                value.as_object_mut().unwrap().remove("Mood");
                value.as_object_mut().unwrap().remove("Media");
            }
            Mock::given(method("GET")).and(path("/library/sections/1/all")).and(query_param("type",kind)).and(query_param("X-Plex-Container-Start","0")).and(query_param("X-Plex-Container-Size","512"))
                .respond_with({
                    if kind == "10" { value["title"] = json!("__BAD_TITLE__"); }
                    let mut body = serde_json::to_vec(&json!({"MediaContainer":{"Metadata":[null,{"title":"No identity"},value],"totalSize":4}})).unwrap();
                    if kind == "10" {
                        let start = body.windows(13).position(|value| value == b"__BAD_TITLE__").unwrap();
                        body.splice(start..start + 13, b"Bad \xff title".iter().copied());
                    }
                    ResponseTemplate::new(200).set_body_bytes(body)
                }).mount(&server).await;
            Mock::given(method("GET")).and(path("/library/sections/1/all")).and(query_param("type",kind)).and(query_param("X-Plex-Container-Start","3"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"MediaContainer":{"Metadata":[if kind == "10" {json!({"ratingKey":"unanalyzed","type":"track","title":valid_title,"Media":[{"Part":[{"Stream":[{"streamType":2,"gain":{},"peak":-1}]}]}]})} else {json!({"ratingKey":[],"title":"Invalid identity"})}],"totalSize":4}}))).mount(&server).await;
        }
        let playlist =
            json!({"ratingKey":"list","type":"playlist","title":"Repeated","smart":true});
        let mut hubs = Some(Mock::given(method("GET")).and(path("/hubs/sections/1")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"MediaContainer":{"Hub":[
            {"hubIdentifier":"related","title":"Picked for You","Metadata":[track()]},
            {"hubIdentifier":"mixes","title":"Mixes","Metadata":[{"type":"playlist","radio":true,"title":"Your Evening Mix","key":"/library/metadata/artist/station/abc?type=10"}]}
        ]}}))).mount_as_scoped(&server).await);
        let stations = Mock::given(method("POST"))
            .and(path("/playQueues"))
            .and(query_param("type", "audio"))
            .and(query_param("window", "24"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"MediaContainer":{"Metadata":[track()]}})),
            )
            .mount_as_scoped(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/playlists"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"MediaContainer":{"Metadata":[playlist],"totalSize":1}})),
            )
            .mount(&server)
            .await;
        let mut first = track();
        first["playlistItemID"] = json!("entry-one");
        let mut second = track();
        second["playlistItemID"] = json!("entry-two");
        Mock::given(method("GET"))
            .and(path("/playlists/list/items"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"MediaContainer":{"Metadata":[first,second],"totalSize":2}}),
                ),
            )
            .mount(&server)
            .await;
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
            .reconcile_source(&library::SourceId::new("plex-test"), "plex", "Plex")
            .await
            .unwrap();
        let catalog_source_id = ("plex-test").to_string();
        let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
        catalog.start_observation().await.unwrap();
        {
            let result = source
                .acquire_catalog(&mut catalog, &|_| {}, &|| false)
                .await;
            result
        }
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
        let uri = library::source_entity_uri(
            &crate::SourceId::new("plex-test"),
            "track",
            "plex:track:track",
        );
        let row = database
            .track(&(&uri), &cancellation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.metadata.title, "Bad � title");
        let later_page = database
            .tracks_by_uris(
                &([library::source_entity_uri(
                    &crate::SourceId::new("plex-test"),
                    "track",
                    "plex:track:unanalyzed",
                )]),
                &cancellation,
            )
            .await
            .unwrap();
        assert_eq!(later_page[0].metadata.title, valid_title);
        assert_eq!(row.metadata.duration_millis, 123456);
        assert_eq!(row.metadata.year, Some(2010));
        assert_eq!(row.state.play_count, 12);
        assert_eq!(row.state.rating, Some(7));
        assert_eq!(
            row.metadata.musicbrainz_recording_id.as_deref(),
            Some("recording")
        );
        assert!(row.artwork_binding.is_some());
        assert_eq!(
            row.links
                .genres
                .iter()
                .map(|genre| genre.name.as_str())
                .collect::<Vec<_>>(),
            ["Rock"]
        );
        let folders = database
            .folder_page(
                database
                    .source_identity_key(&library::SourceId::new(&catalog_source_id))
                    .await
                    .unwrap()
                    .unwrap(),
                None,
                0,
                10,
                &cancellation,
            )
            .await
            .unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].metadata.name.as_deref(), Some("Music"));
        assert_eq!(folders[0].track_count, 2);
        let moods = library::ListRead {
            playlist_folder: None,
            kind: library::ListKind::Moods,
            source: Some(
                database
                    .source_identity_key(&library::SourceId::new(&catalog_source_id))
                    .await
                    .unwrap()
                    .unwrap(),
            ),
            sort: library::CatalogSort::Title,
            folder: Some(folders[0].folder_key),
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: false,
        }
        .moods(&database, 0, 10, &cancellation)
        .await
        .unwrap();
        assert_eq!(moods.len(), 1);
        assert_eq!(moods[0].metadata.name, "Calm");
        assert_eq!(moods[0].summary.track_count, 1);
        assert_eq!(
            database
                .playback_loudness(&uri, &cancellation)
                .await
                .unwrap()
                .0
                .unwrap()
                .replay_gain_db,
            Some(-4.25)
        );
        let playlist_key = database
            .playlist_key_by_object(
                database
                    .source_identity_key(&library::SourceId::new(&catalog_source_id))
                    .await
                    .unwrap()
                    .unwrap(),
                "plex:playlist:list",
                &cancellation,
            )
            .await
            .unwrap()
            .unwrap();
        let entries = database
            .playlist_edit_entries(
                playlist_key,
                &library::MemberRead::new(library::Collection::Playlist(playlist_key)).selection(),
                &cancellation,
            )
            .await
            .unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.object_id.as_str())
                .collect::<Vec<_>>(),
            vec!["entry-one", "entry-two"]
        );
        assert!(
            !database
                .playlists(&[playlist_key], &cancellation)
                .await
                .unwrap()[0]
                .writable
        );
        let home = database
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
                &cancellation,
            )
            .await
            .unwrap();
        assert!(
            home.provider_sections
                .iter()
                .any(|section| section.title.as_deref() == Some("Picked for You")
                    && section.rows.tracks.len() == 1)
        );
        assert!(
            home.provider_sections
                .iter()
                .any(
                    |section| section.title.as_deref() == Some("Your Evening Mix")
                        && section.rows.tracks.len() == 1
                )
        );
        database
            .reconcile_source(&library::SourceId::new("plex-test"), "plex", "Plex")
            .await
            .unwrap();
        let catalog_source_id = ("plex-test").to_string();
        let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
        catalog.start_observation().await.unwrap();
        {
            let result = source
                .acquire_catalog(&mut catalog, &|_| {}, &|| false)
                .await;
            result
        }
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
        drop(stations);
        let unavailable_stations = Mock::given(method("POST"))
            .and(path("/playQueues"))
            .respond_with(ResponseTemplate::new(503))
            .mount_as_scoped(&server)
            .await;
        for failed_hubs in [false, true] {
            let unavailable_hubs = if failed_hubs {
                drop(hubs.take());
                Some(
                    Mock::given(method("GET"))
                        .and(path("/hubs/sections/1"))
                        .respond_with(ResponseTemplate::new(503))
                        .mount_as_scoped(&server)
                        .await,
                )
            } else {
                None
            };
            database
                .reconcile_source(&library::SourceId::new("plex-test"), "plex", "Plex")
                .await
                .unwrap();
            let catalog_source_id = ("plex-test").to_string();
            let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
            catalog.start_observation().await.unwrap();
            {
                let result = source
                    .acquire_catalog(&mut catalog, &|_| {}, &|| false)
                    .await;
                result
            }
            .unwrap();
            {
                database
                    .publish_catalog_update(&catalog_source_id, catalog.changes().clone())
                    .await;
                Ok::<_, crate::SourceError>(())
            }
            .unwrap();
            assert!(!catalog.changes().changed());
            assert!(updates.try_recv().is_err(), "retained Home stays quiet");
            let retained = database
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
                    &cancellation,
                )
                .await
                .unwrap();
            assert_eq!(retained.provider_sections, home.provider_sections);
            drop(unavailable_hubs);
        }
        drop(unavailable_stations);
        database
            .set_favorite(&library::FavoriteTarget::Track(uri.clone()), true)
            .await
            .unwrap();
        let mut updated = track();
        drop(bulk_track);
        updated["title"] = json!("Updated");
        updated["year"] = json!(2012);
        updated["userRating"] = json!({"bad":true});
        updated["Media"] = json!([{"Part":[{"Stream":[{"streamType":2,"gain":{},"loudness":[],"peak":-1,"albumPeak":[]}]}]}]);
        for (raw, item) in [
            ("track", updated),
            (
                "album",
                json!({"ratingKey":"album","type":"album","title":"Album","year":[]}),
            ),
            (
                "invalid",
                json!({"ratingKey":[],"type":"track","title":"Unusable"}),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/library/metadata/{raw}")))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"MediaContainer":{"Metadata":[item]}})),
                )
                .mount(&server)
                .await;
        }
        source_owner
            .apply_items(&database, vec!["invalid".into(), "track".into()], vec![])
            .await
            .unwrap();
        let row = database
            .track(&(&uri), &cancellation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.metadata.title, "Updated");
        assert_eq!(row.metadata.year, Some(2012));
        assert_eq!(row.state.play_count, 12);
        assert!(row.state.favorite);
    }
}
