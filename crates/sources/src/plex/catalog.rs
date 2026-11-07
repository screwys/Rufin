use super::item::{ItemResponse, rating, read_items};
use super::{PlexSource, plex_id};
use crate::policy::raw_item_id;
use crate::remote_json::{field, id, items};
use crate::source::{SourceProgress, SourceProgressStage};
use crate::{SourceError, SourceResult};
use library::HomeSectionKind;
use library::{FolderObservation, HomeEntryInput, HomeEntryKind};
use serde_json::Value;
use std::collections::HashSet;

const PAGE: usize = 512;

impl PlexSource {
    /// Lists the music sections: all of them, or `since` a sync what the server changed, played
    /// or rated since then and the deletions a count shows; a section new since then is listed
    /// whole. Then hubs, playlists and Home. Returns the server's clock at the start.
    pub(crate) async fn acquire_catalog(
        &self,
        observation: &mut library::Observation<'_>,
        since: Option<i64>,
        progress: &(dyn Fn(SourceProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<Option<i64>> {
        let walk = observation.is_walk();
        let from = since.map(|since| since - crate::source::SYNC_MARGIN);
        let known = observation
            .catalog
            .music_folders(observation.source)
            .into_iter()
            .map(|folder| folder.object_id)
            .collect::<HashSet<_>>();
        let (sections, started) = self.get_dated("/library/sections", &[]).await?;
        let sections = items(&sections["MediaContainer"]["Directory"])
            .iter()
            .filter(|section| section["type"].as_str() == Some("artist"))
            .filter_map(|section| {
                let id = id(&section["key"])?;
                let title = section["title"].as_str().unwrap_or(&id).to_owned();
                Some((id, title))
            })
            .collect::<Vec<_>>();
        let mut total = 0;
        for (section_id, title) in &sections {
            let mut section_record = FolderObservation {
                metadata: library::FolderMetadata {
                    name: Some(title.clone()),
                    ..Default::default()
                },
                object_id: plex_id("music-folder", section_id),
                ..Default::default()
            };
            let batch = library::CatalogBatch {
                folders: vec![section_record.clone()],
                ..Default::default()
            };
            if cancelled() {
                return Err(SourceError::Cancelled);
            }
            observation.apply_catalog(&batch, true).await?;
            let after = from
                .filter(|_| known.contains(&section_record.object_id))
                .map(|from| from.to_string());
            for (kind, stage) in [
                ("8", SourceProgressStage::Artists),
                ("9", SourceProgressStage::Albums),
                ("10", SourceProgressStage::Tracks),
            ] {
                self.section_pages(
                    observation,
                    &mut section_record,
                    kind,
                    after.as_deref(),
                    &|completed, total| {
                        progress(SourceProgress {
                            stage,
                            completed,
                            total,
                        })
                    },
                    cancelled,
                )
                .await?;
            }
            if walk && after.is_none() {
                if cancelled() {
                    return Err(SourceError::Cancelled);
                }
                observation
                    .complete_source_folder_membership(&section_record.object_id)
                    .await?;
            }
            if let Some(from) = from {
                total += self
                    .acquire_user_data(observation, section_id, from, cancelled)
                    .await?;
            }
            self.acquire_hubs(observation, section_id, cancelled)
                .await?;
        }
        if walk {
            if cancelled() {
                return Err(SourceError::Cancelled);
            }
            let kinds: &[&str] = match from {
                None => &["track", "album", "artist", "folder"],
                Some(_) => &["folder"],
            };
            observation
                .complete_source_observation(kinds, None, None)
                .await?;
        }
        // Since a sync the count shows deletions; only then are the ids listed.
        if from.is_some() && observation.catalog.indexed_tracks(observation.source) > total {
            let mut listed = HashSet::new();
            for (section_id, _) in &sections {
                let mut pages = Pages::new(section_id, vec![("type", "10".into())], PAGE);
                while let Some(container) = self.next_page(&mut pages, cancelled).await? {
                    listed.extend(
                        items(&container["Metadata"])
                            .iter()
                            .filter_map(|item| id(&item["ratingKey"]))
                            .map(|raw| plex_id("track", &raw)),
                    );
                }
            }
            observation.remove_unlisted("track", &listed).await?;
        }
        if cancelled() {
            return Err(SourceError::Cancelled);
        }
        let home_prefixes = sections
            .iter()
            .map(|(section_id, _)| format!("plex:{section_id}:"))
            .collect::<Vec<_>>();
        observation
            .complete_source_home_sections(&"plex:", &Vec::new(), &home_prefixes)
            .await?;
        self.acquire_playlists(observation, cancelled).await?;
        crate::source::acquire_home(observation, cancelled, |section| self.home_section(section))
            .await?;

        progress(SourceProgress {
            stage: SourceProgressStage::Finalizing,
            completed: 1,
            total: Some(1),
        });
        Ok(started)
    }

    /// The next page's container of a section listing, or None after the last.
    async fn next_page(
        &self,
        pages: &mut Pages,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<Option<Value>> {
        if pages.finished {
            return Ok(None);
        }
        if cancelled() {
            return Err(SourceError::Cancelled);
        }
        let mut query = pages.query.clone();
        query.push(("X-Plex-Container-Start", pages.offset.to_string()));
        query.push(("X-Plex-Container-Size", pages.size.to_string()));
        let mut body = self.get(&pages.path, &query).await?;
        let count = items(&body["MediaContainer"]["Metadata"]).len();
        pages.finished = page_finished(&body, pages.offset, count)?;
        pages.offset += count;
        Ok(Some(body["MediaContainer"].take()))
    }

    /// Reads one item type of a section in pages; `updated_after` keeps only the items the
    /// server changed since then.
    async fn section_pages(
        &self,
        observation: &mut library::Observation<'_>,
        section: &mut FolderObservation,
        kind: &str,
        updated_after: Option<&str>,
        progress: &(dyn Fn(usize, Option<usize>) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let mut query = vec![
            ("type", kind.to_owned()),
            ("includeGuids", "1".into()),
            ("includeStreams", "1".into()),
        ];
        query.extend(updated_after.map(|after| ("updatedAt>>", after.to_owned())));
        let mut pages = Pages::new(raw_item_id(&section.object_id), query, PAGE);
        while let Some(container) = self.next_page(&mut pages, cancelled).await? {
            let rows = items(&container["Metadata"]);
            section.tracks = if kind == "10" {
                rows.iter()
                    .filter_map(|item| id(&item["ratingKey"]))
                    .map(|raw| plex_id("track", &raw))
                    .collect()
            } else {
                Vec::new()
            };
            self.acquire_metadata_page(observation, rows, Some(section), cancelled)
                .await?;
            progress(pages.offset, field(&container, "totalSize"));
        }
        Ok(())
    }

    /// What was played or rated in a section since `from`, written from the listing rows.
    /// Returns the section's track count, which the plays listing carries.
    async fn acquire_user_data(
        &self,
        observation: &mut library::Observation<'_>,
        section_id: &str,
        from: i64,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<usize> {
        let mut total = 0;
        // Recent plays come first by date.
        let query = vec![("type", "10".into()), ("sort", "lastViewedAt:desc".into())];
        let mut pages = Pages::new(section_id, query, 64);
        while let Some(container) = self.next_page(&mut pages, cancelled).await? {
            total = field(&container, "totalSize").unwrap_or(total);
            let rows = items(&container["Metadata"]);
            let played = rows
                .iter()
                .take_while(|item| field::<i64>(item, "lastViewedAt").is_some_and(|at| at >= from))
                .count();
            write_user_data(observation, "track", &rows[..played], false).await?;
            if played < rows.len() {
                break;
            }
        }
        for (kind, name) in [("8", "artist"), ("9", "album"), ("10", "track")] {
            let query = vec![("type", kind.into()), ("lastRatedAt>>", from.to_string())];
            let mut pages = Pages::new(section_id, query, PAGE);
            while let Some(container) = self.next_page(&mut pages, cancelled).await? {
                write_user_data(observation, name, items(&container["Metadata"]), true).await?;
            }
        }
        Ok(total)
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

            // Entries are read again only when the playlist's revision changed.
            for item in playlists {
                let Some(raw) = id(&item["ratingKey"]) else {
                    continue;
                };
                let playlist = plex_id("playlist", &raw);
                let revision = format!(
                    "{}:{}",
                    field::<i64>(item, "updatedAt").unwrap_or_default(),
                    field::<i64>(item, "leafCount").unwrap_or_default()
                );
                if observation
                    .catalog
                    .source_playlist_is_fresh(source_id, &playlist, Some(&revision), 0)
                    .await?
                {
                    continue;
                }
                self.acquire_playlist_entries(observation, &playlist, cancelled)
                    .await?;
                observation
                    .write_source_playlist_freshness(&playlist, Some(&revision), None)
                    .await?;
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
        section: HomeSectionKind,
    ) -> SourceResult<Vec<HomeEntryInput>> {
        let (kind, sort) = match section {
            HomeSectionKind::Explore => return Ok(Vec::new()),
            HomeSectionKind::MostPlayed => ("10", "viewCount:desc"),
            HomeSectionKind::RecentlyPlayed => ("10", "lastViewedAt:desc"),
            HomeSectionKind::NewlyAdded => ("9", "addedAt:desc"),
            HomeSectionKind::RecentlyReleased => ("9", "originallyAvailableAt:desc"),
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

/// Where a section listing has come.
struct Pages {
    path: String,
    query: Vec<(&'static str, String)>,
    size: usize,
    offset: usize,
    finished: bool,
}

impl Pages {
    fn new(section: &str, query: Vec<(&'static str, String)>, size: usize) -> Self {
        Self {
            path: format!("/library/sections/{section}/all"),
            query,
            size,
            offset: 0,
            finished: false,
        }
    }
}

/// The play and rating state of listing rows; `rated` rows clear a rating they do not carry.
async fn write_user_data(
    observation: &mut library::Observation<'_>,
    kind: &str,
    rows: &[Value],
    rated: bool,
) -> SourceResult<()> {
    let states = rows
        .iter()
        .filter_map(|item| {
            let state = library::NativeUserState {
                rating: rating(item, rated),
                play_count: field::<i64>(item, "viewCount").filter(|v| *v >= 0 && kind == "track"),
                last_played: field::<i64>(item, "lastViewedAt")
                    .filter(|v| *v >= 0 && kind == "track"),
                ..Default::default()
            };
            Some((plex_id(kind, &id(&item["ratingKey"])?), state))
        })
        .collect::<Vec<_>>();
    observation.write_native_user_data(kind, &states).await?;
    Ok(())
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
        let catalog = library::Catalog::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let mut updates = catalog.changes();
        catalog
            .reconcile_source(&library::SourceId::new("plex-test"), "plex", "Plex")
            .await
            .unwrap();
        let catalog_source_id = ("plex-test").to_string();
        let mut observation = catalog.observe(&catalog_source_id).await.unwrap();
        observation.start_observation().await.unwrap();
        {
            let result = source
                .acquire_catalog(&mut observation, None, &|_| {}, &|| false)
                .await;
            result
        }
        .unwrap();
        let publication = crate::published(&mut updates).expect("catalog update").1;
        assert!(publication.intersects(sources::Shown::CATALOG));
        let cancellation = library::ReadCancellation::new();
        let uri = library::source_entity_uri(
            &crate::SourceId::new("plex-test"),
            "track",
            "plex:track:track",
        );
        let row = catalog
            .track(&(&uri), &cancellation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.metadata.title, "Bad � title");
        let later_page = catalog
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
        let source_key = {
            catalog.loaded().await;
            catalog.source_key(&library::SourceId::new(&catalog_source_id))
        }
        .unwrap();
        let folders = crate::listed(
            &catalog,
            library::Read::Folders(library::FolderRead {
                source: source_key,
                parent: None,
                filter: String::new(),
                descending: false,
            }),
        )
        .await
        .into_iter()
        .filter_map(|row| match row {
            library::Row::Folder(folder) => Some(folder),
            _ => None,
        })
        .collect::<Vec<_>>();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "Music");
        assert_eq!(folders[0].summary.track_count, 2);
        let moods = crate::listed(
            &catalog,
            library::Read::List(library::ListRead {
                folder: folders[0].folder_key,
                ..library::ListRead::new(
                    library::ListKind::Moods,
                    Some(source_key),
                    library::LibraryField::Title,
                )
            }),
        )
        .await
        .into_iter()
        .filter_map(|row| match row {
            library::Row::Tag(tag) => Some(tag),
            _ => None,
        })
        .collect::<Vec<_>>();
        assert_eq!(moods.len(), 1);
        assert_eq!(moods[0].name, "Calm");
        assert_eq!(moods[0].summary.track_count, 1);
        let playable = crate::playable(&catalog, &uri).await;
        assert_eq!(
            playable.gains.track_loudness().unwrap().replay_gain_db,
            Some(-4.25)
        );
        let playlist_key = crate::playlist_key(
            &catalog,
            {
                catalog.loaded().await;
                catalog.source_key(&library::SourceId::new(&catalog_source_id))
            }
            .unwrap(),
            "plex:playlist:list",
        )
        .await
        .unwrap();
        let entries = catalog
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
            !catalog
                .playlists(&[playlist_key], &cancellation)
                .await
                .unwrap()[0]
                .writable
        );
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
        catalog
            .reconcile_source(&library::SourceId::new("plex-test"), "plex", "Plex")
            .await
            .unwrap();
        let catalog_source_id = ("plex-test").to_string();
        let mut observation = catalog.observe(&catalog_source_id).await.unwrap();
        observation.start_observation().await.unwrap();
        {
            let result = source
                .acquire_catalog(&mut observation, None, &|_| {}, &|| false)
                .await;
            result
        }
        .unwrap();
        assert!(!observation.changed());
        assert!(
            crate::published(&mut updates).is_none(),
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
            catalog
                .reconcile_source(&library::SourceId::new("plex-test"), "plex", "Plex")
                .await
                .unwrap();
            let catalog_source_id = ("plex-test").to_string();
            let mut observation = catalog.observe(&catalog_source_id).await.unwrap();
            observation.start_observation().await.unwrap();
            {
                let result = source
                    .acquire_catalog(&mut observation, None, &|_| {}, &|| false)
                    .await;
                result
            }
            .unwrap();
            assert!(!observation.changed());
            assert!(
                crate::published(&mut updates).is_none(),
                "retained Home stays quiet"
            );
            let retained = catalog
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
            assert_eq!(retained.provider_sections, home.provider_sections);
            drop(unavailable_hubs);
        }
        drop(unavailable_stations);
        catalog
            .set_favorite(&library::FavoriteTarget::Track(uri.clone()), true, false)
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
            .apply_items(&catalog, vec!["invalid".into(), "track".into()], vec![])
            .await
            .unwrap();
        let row = catalog
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
