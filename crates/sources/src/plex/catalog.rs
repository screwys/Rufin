use super::item::stage_item;
use super::{PlexSource, plex_id};
use crate::policy::raw_item_id;
use crate::remote_json::{field, id, items};
use crate::source::{SourceReadProgress, SourceReadStage};
use crate::{SourceError, SourceHomeSection, SourceResult};
use library::{HomeEntryInput, HomeEntryKind, Scan};
use serde_json::Value;

const PAGE: usize = 512;

impl PlexSource {
    pub(crate) async fn stage_catalog(
        &self,
        scan: &mut Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let sections = self.get("/library/sections", &[]).await?;
        for section in items(&sections["MediaContainer"]["Directory"]) {
            if section["type"].as_str() != Some("artist") {
                continue;
            }
            let Some(section_id) = id(&section["key"]) else {
                continue;
            };
            let folder = plex_id("music-folder", &section_id);
            let title = section["title"].as_str().unwrap_or(&section_id);
            scan.write_folder(
                &folder,
                title,
                &title.to_lowercase(),
                &title.to_lowercase(),
                None,
            )
            .await?;
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
                    // Section listings omit stream analysis and track moods even when
                    // includeStreams is requested. Read one bounded bulk detail page.
                    let details = if kind == "10" {
                        let ids: Vec<_> = rows
                            .iter()
                            .filter_map(|item| id(&item["ratingKey"]))
                            .collect();
                        if ids.is_empty() {
                            Value::Null
                        } else {
                            self.get(
                                &format!("/library/metadata/{}", ids.join(",")),
                                &[("includeStreams", "1".into())],
                            )
                            .await?
                        }
                    } else {
                        Value::Null
                    };
                    scan.begin_batch().await?;
                    for item in items(&details["MediaContainer"]["Metadata"]) {
                        stage_item(scan, item).await?;
                    }
                    for item in rows {
                        stage_item(scan, item).await?;
                        if kind == "10"
                            && let Some(raw) = id(&item["ratingKey"])
                        {
                            scan.write_track_folders(&[library::ScanLink::new(
                                &plex_id("track", &raw),
                                &folder,
                                0,
                            )])
                            .await?;
                        }
                    }
                    // The section response owns its display name, not a track's optional copy.
                    scan.write_folder(
                        &folder,
                        title,
                        &title.to_lowercase(),
                        &title.to_lowercase(),
                        None,
                    )
                    .await?;
                    scan.finish_batch().await?;
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
            self.stage_hubs(scan, &section_id).await?;
        }
        self.stage_playlists(scan, cancelled).await?;
        for section in [
            SourceHomeSection::MostPlayed,
            SourceHomeSection::NewlyAdded,
            SourceHomeSection::RecentlyPlayed,
            SourceHomeSection::RecentlyReleased,
        ] {
            match self.home_section(section).await {
                Ok(entries) => {
                    for entry in entries {
                        scan.write_home_entry(&entry).await?;
                    }
                }
                Err(error) => {
                    crate::source::optional_collection_error(error)?;
                    scan.retain_home_section(section.id()).await?;
                }
            }
        }
        progress(SourceReadProgress {
            stage: SourceReadStage::Finalizing,
            completed: 1,
            total: Some(1),
        });
        Ok(())
    }

    async fn stage_playlists(
        &self,
        scan: &mut Scan,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
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
                    scan.retain_playlists().await?;
                    return Ok(());
                }
            };
            let rows = items(&page["MediaContainer"]["Metadata"]);
            let finished = page_finished(&page, offset, rows.len())?;
            for item in rows {
                if is_station(item) {
                    continue;
                }
                let Some(raw) = id(&item["ratingKey"]) else {
                    continue;
                };
                stage_item(scan, item).await?;
                self.stage_playlist_entries(scan, &plex_id("playlist", &raw))
                    .await?;
            }
            offset += rows.len();
            if finished {
                return Ok(());
            }
        }
    }

    pub(crate) async fn stage_playlist_entries(
        &self,
        scan: &mut Scan,
        playlist_id: &str,
    ) -> SourceResult<()> {
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
            scan.begin_batch().await?;
            for (position, item) in rows.iter().enumerate() {
                if item["type"].as_str() != Some("track") {
                    continue;
                }
                let (Some(raw), Some(entry)) =
                    (id(&item["ratingKey"]), id(&item["playlistItemID"]))
                else {
                    continue;
                };
                stage_item(scan, item).await?;
                scan.write_playlist_entry(
                    playlist_id,
                    &entry,
                    &plex_id("track", &raw),
                    (offset + position) as i64,
                )
                .await?;
            }
            scan.finish_batch().await?;
            offset += rows.len();
            if finished {
                return Ok(());
            }
        }
    }

    pub(crate) async fn apply_live_items(
        &self,
        database: &library::Database,
        source_id: &str,
        upserts: Vec<String>,
        removals: Vec<String>,
    ) -> SourceResult<library::ScanOutcome> {
        let mut scan = Scan::begin_items(database, source_id).await?;
        for raw in upserts {
            let item = self.metadata(&raw).await?;
            if item["type"].as_str() == Some("track")
                && let Some(album) = id(&item["parentRatingKey"])
            {
                stage_item(&mut scan, &self.metadata(&album).await?).await?;
            }
            stage_item(&mut scan, &item).await?;
            if item["type"].as_str() == Some("playlist")
                && let Some(raw) = id(&item["ratingKey"])
            {
                self.stage_playlist_entries(&mut scan, &plex_id("playlist", &raw))
                    .await?;
            }
        }
        for raw in removals {
            scan.remove_track(&plex_id("track", raw_item_id(&raw)))
                .await?;
            scan.remove_album(&plex_id("album", raw_item_id(&raw)))
                .await?;
            scan.remove_artist(&plex_id("artist", raw_item_id(&raw)))
                .await?;
            scan.remove_playlist(&plex_id("playlist", raw_item_id(&raw)))
                .await?;
        }
        Ok(scan.finish().await?)
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
            .filter_map(|(position, item)| home_entry(section.id(), position, item))
            .collect())
    }

    async fn stage_hubs(&self, scan: &mut Scan, section: &str) -> SourceResult<()> {
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
        for hub in items(&page["MediaContainer"]["Hub"]) {
            let Some(identifier) = hub["hubIdentifier"].as_str() else {
                continue;
            };
            let section_id = format!("plex:{section}:{identifier}");
            for (position, item) in items(&hub["Metadata"]).iter().take(24).enumerate() {
                if let Some(entry) = home_entry(&section_id, position, item) {
                    stage_item(scan, item).await?;
                    scan.write_home_entry(&entry).await?;
                }
            }
            if let Some(title) = hub["title"].as_str() {
                scan.write_home_section_title(&section_id, title).await?;
            }
            for station in items(&hub["Directory"])
                .iter()
                .chain(items(&hub["Metadata"]))
                .filter(|item| is_station(item))
                .take(12)
            {
                let Some(key) = station["key"].as_str() else {
                    continue;
                };
                let candidates = match self.station_candidates(key, 24).await {
                    Ok(rows) => rows,
                    Err(error) => {
                        crate::source::optional_collection_error(error)?;
                        continue;
                    }
                };
                let mix_section = format!("{section_id}:{}", crate::policy::stable_hash(key));
                for (position, item) in candidates.iter().enumerate() {
                    if let Some(entry) = home_entry(&mix_section, position, item) {
                        stage_item(scan, item).await?;
                        scan.write_home_entry(&entry).await?;
                    }
                }
                if let Some(title) = station["title"].as_str() {
                    scan.write_home_section_title(&mix_section, title).await?;
                }
            }
        }
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

fn home_entry(section: &str, position: usize, item: &Value) -> Option<HomeEntryInput> {
    let raw = id(&item["ratingKey"])?;
    let kind = match item["type"].as_str()? {
        "track" => HomeEntryKind::Track,
        "album" => HomeEntryKind::Album,
        _ => return None,
    };
    Some(HomeEntryInput {
        section_id: section.into(),
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

fn page_finished(page: &Value, offset: usize, count: usize) -> SourceResult<bool> {
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
    async fn catalog_and_live_items_ingest_independent_facts_and_preserve_occurrences() {
        let server = MockServer::start().await;
        let source = source(&server);
        let bulk_track = Mock::given(method("GET"))
            .and(path("/library/metadata/track"))
            .and(query_param("includeStreams", "1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"MediaContainer":{"Metadata":[track()]}})),
            )
            .mount_as_scoped(&server)
            .await;
        Mock::given(method("GET")).and(path("/library/metadata/unanalyzed")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"MediaContainer":{"Metadata":[{"ratingKey":"unanalyzed","type":"track","title":"No Analysis","Media":[{"Part":[{"Stream":[{"streamType":2,"gain":{},"peak":-1}]}]}]}]}}))).mount(&server).await;
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
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"MediaContainer":{"Metadata":[null,{"title":"No identity"},value],"totalSize":4}}))).mount(&server).await;
            Mock::given(method("GET")).and(path("/library/sections/1/all")).and(query_param("type",kind)).and(query_param("X-Plex-Container-Start","3"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"MediaContainer":{"Metadata":[if kind == "10" {json!({"ratingKey":"unanalyzed","type":"track","title":"No Analysis","Media":[{"Part":[{"Stream":[{"streamType":2,"gain":{},"peak":-1}]}]}]})} else {json!({"ratingKey":[],"title":"Invalid identity"})}],"totalSize":4}}))).mount(&server).await;
        }
        let playlist =
            json!({"ratingKey":"list","type":"playlist","title":"Repeated","smart":true});
        Mock::given(method("GET")).and(path("/hubs/sections/1")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"MediaContainer":{"Hub":[
            {"hubIdentifier":"related","title":"Picked for You","Metadata":[track()]},
            {"hubIdentifier":"mixes","title":"Mixes","Metadata":[{"type":"playlist","radio":true,"title":"Your Evening Mix","key":"/library/metadata/artist/station/abc?type=10"}]}
        ]}}))).mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/playQueues"))
            .and(query_param("type", "audio"))
            .and(query_param("window", "24"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"MediaContainer":{"Metadata":[track()]}})),
            )
            .mount(&server)
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
        let mut scan = Scan::begin(&database, "plex-test", "Plex", "plex", None)
            .await
            .unwrap();
        source
            .stage_catalog(&mut scan, &|_| {}, &|| false)
            .await
            .unwrap();
        let library::ScanOutcome::Changed(publication) = scan.finish().await.unwrap() else {
            panic!("initial publication")
        };
        let cancellation = library::ReadCancellation::new();
        let uri = library::source_entity_uri(
            &crate::SourceId::new("plex-test"),
            "track",
            "plex:track:track",
        );
        let row = database
            .track_row_by_uri(&uri, &cancellation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.duration_millis, 123456);
        assert_eq!(row.year, Some(2010));
        assert_eq!(row.play_count, 12);
        assert_eq!(row.rating, Some(7));
        assert_eq!(row.musicbrainz_recording_id.as_deref(), Some("recording"));
        assert!(row.artwork_binding.is_some());
        assert_eq!(
            database
                .track_loudness(publication.source, row.track_key, &cancellation)
                .await
                .unwrap()
                .unwrap()
                .replay_gain_db,
            Some(-4.25)
        );
        let playlist_key = database
            .playlist_key_by_object(publication.source, "plex:playlist:list", &cancellation)
            .await
            .unwrap()
            .unwrap();
        let order = database
            .playlist_entry_order(
                playlist_key,
                None,
                library::PlaylistEntrySort::Position,
                false,
                "",
                &cancellation,
            )
            .await
            .unwrap();
        let entries = database
            .source_playlist_entry_object_ids(
                publication.source,
                playlist_key,
                &order,
                &cancellation,
            )
            .await
            .unwrap();
        assert_eq!(entries, vec!["entry-one", "entry-two"]);
        assert!(
            !database
                .playlist_rows(&[playlist_key], &cancellation)
                .await
                .unwrap()[0]
                .writable
        );
        let home = database
            .home_page(
                publication.source,
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
        let mut scan = Scan::begin(&database, "plex-test", "Plex", "plex", None)
            .await
            .unwrap();
        source
            .stage_catalog(&mut scan, &|_| {}, &|| false)
            .await
            .unwrap();
        assert!(matches!(
            scan.finish().await.unwrap(),
            library::ScanOutcome::Identical(_)
        ));
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
        source
            .apply_live_items(
                &database,
                "plex-test",
                vec!["invalid".into(), "track".into()],
                vec![],
            )
            .await
            .unwrap();
        let row = database
            .track_row_by_uri(&uri, &cancellation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.title, "Updated");
        assert_eq!(row.year, Some(2012));
        assert_eq!(row.play_count, 12);
        assert!(row.favorite);
    }
}
