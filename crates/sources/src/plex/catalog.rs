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
        observation: &mut library::Observation<'_>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let stamp = observation.observed;

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
            observation.apply_catalog(&batch, true).await?;
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
                    self.acquire_metadata_page(observation, rows, Some(&section_record), cancelled)
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
            if stamp.is_some() {
                if cancelled() {
                    return Err(crate::SourceError::Cancelled);
                }
                observation
                    .complete_source_folder_membership(&section_record.object_id)
                    .await?;
            }
            self.acquire_hubs(observation, &section_id, cancelled)
                .await?;
        }
        if stamp.is_some() {
            if cancelled() {
                return Err(crate::SourceError::Cancelled);
            }
            observation
                .complete_source_observation(&["track", "album", "artist", "folder"], None)
                .await?;
        }
        if cancelled() {
            return Err(crate::SourceError::Cancelled);
        }
        observation
            .complete_source_home_sections(&"plex:", &Vec::new(), &home_prefixes)
            .await?;
        self.acquire_playlists(observation, cancelled).await?;
        crate::source::acquire_home(observation, cancelled, |section| self.home_section(section))
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
        observation: &mut library::Observation<'_>,
        rows: &[Value],
        folder: Option<&FolderObservation>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = observation.source_id;

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
            observation.apply_catalog(&batch, true).await?;
        }
        Ok(())
    }

    async fn acquire_playlists(
        &self,
        observation: &mut library::Observation<'_>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = observation.source_id;

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
                    observation.apply_catalog(&batch, true).await?;
                }
            };

            for item in playlists {
                if let Some(raw) = id(&item["ratingKey"]) {
                    self.acquire_playlist_entries(
                        observation,
                        &plex_id("playlist", &raw),
                        cancelled,
                    )
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
        observation: &mut library::Observation<'_>,
        playlist_id: &str,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = observation.source_id;

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
                    observation.apply_catalog(&batch, false).await?;
                }
            };
            if cancelled() {
                return Err(crate::SourceError::Cancelled);
            }
            observation
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
        observation: &mut library::Observation<'_>,
        upserts: Vec<String>,
        mut removals: Vec<String>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = observation.source_id;

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
                    observation,
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
                    observation.apply_catalog(&batch, true).await?;
                }
            };

            if item["type"] == "playlist"
                && let Some(id) = id(&item["ratingKey"])
            {
                self.acquire_playlist_entries(observation, &plex_id("playlist", &id), cancelled)
                    .await?;
            }
        }

        for raw in removals {
            if cancelled() {
                return Err(crate::SourceError::Cancelled);
            }
            for kind in ["track", "album", "artist", "playlist"] {
                observation
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
        observation: &mut library::Observation<'_>,
        section: &str,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = observation.source_id;

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
                observation.apply_catalog(&batch, false).await?;
            }
            if cancelled() {
                return Err(SourceError::Cancelled);
            }
            observation
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
        observation
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

    #[tokio::test]
    async fn live_updates_refresh_newly_added_and_retain_it_on_failure() {
        let server = MockServer::start().await;
        let source = crate::Source::new(
            crate::SourceId::new("source"),
            crate::source::Implementation::Plex(source(&server)),
        );
        let directory = tempfile::tempdir().unwrap();
        let catalog = library::Catalog::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let mut updates = catalog.changes();
        catalog
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
                .apply_items(&catalog, vec![id.into()], vec![])
                .await
                .unwrap();
            if identical {
                assert!(
                    crate::published(&mut updates).is_none(),
                    "unchanged native data stays quiet"
                );
            } else {
                assert!(crate::published(&mut updates).is_some());
            }
            let home = catalog
                .home_page(
                    {
                        catalog.loaded().await;
                        catalog.source_key(&crate::SourceId::new("source"))
                    }
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
}
