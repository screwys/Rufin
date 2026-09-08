use super::*;
use crate::policy::raw_item_id;
use crate::remote_json::{field, id, items};

impl PlexSource {
    pub(crate) async fn image_bytes(
        &self,
        image: &crate::NativeImageRef,
        size: u32,
    ) -> SourceResult<crate::ImageBytes> {
        let request = self
            .request(
                Method::GET,
                "/photo/:/transcode",
                &[
                    ("url", image.item_id.clone()),
                    ("width", size.to_string()),
                    ("height", size.to_string()),
                    ("minSize", "1".into()),
                ],
            )
            .await?;
        remote_http::bytes(request, HTTP, RESPONSE).await
    }
    pub(crate) async fn set_rating(&self, object: &str, rating: Option<u8>) -> SourceResult<()> {
        self.unit(
            Method::PUT,
            "/:/rate",
            &[
                ("key", raw_item_id(object).into()),
                ("identifier", "com.plexapp.plugins.library".into()),
                (
                    "rating",
                    rating.map_or_else(|| "-1".into(), |rating| rating.to_string()),
                ),
            ],
        )
        .await
    }
    fn library_uri(&self, ids: &[String]) -> String {
        format!(
            "server://{}/com.plexapp.plugins.library/library/metadata/{}",
            self.config.server_id,
            ids.iter()
                .map(|id| raw_item_id(id))
                .collect::<Vec<_>>()
                .join(",")
        )
    }
    pub(crate) async fn create_playlist(&self, name: &str, ids: &[String]) -> SourceResult<String> {
        let response: Value = remote_http::json(
            self.request(
                Method::POST,
                "/playlists",
                &[
                    ("type", "audio".into()),
                    ("smart", "0".into()),
                    ("title", name.into()),
                    ("uri", self.library_uri(ids)),
                ],
            )
            .await?,
            HTTP,
            RESPONSE,
        )
        .await?;
        items(&response["MediaContainer"]["Metadata"])
            .first()
            .and_then(|item| id(&item["ratingKey"]))
            .map(|id| plex_id("playlist", &id))
            .ok_or(SourceError::NotFound)
    }
    pub(crate) async fn rename_playlist(&self, playlist: &str, name: &str) -> SourceResult<()> {
        self.unit(
            Method::PUT,
            &format!("/playlists/{}", raw_item_id(playlist)),
            &[("title", name.into())],
        )
        .await
    }
    pub(crate) async fn delete_playlist(&self, playlist: &str) -> SourceResult<()> {
        self.unit(
            Method::DELETE,
            &format!("/playlists/{}", raw_item_id(playlist)),
            &[],
        )
        .await
    }
    pub(crate) async fn add_playlist_tracks(
        &self,
        playlist: &str,
        ids: &[String],
    ) -> SourceResult<()> {
        self.unit(
            Method::PUT,
            &format!("/playlists/{}/items", raw_item_id(playlist)),
            &[("uri", self.library_uri(ids))],
        )
        .await
    }
    pub(crate) async fn remove_playlist_entries(
        &self,
        playlist: &str,
        occurrences: &[String],
    ) -> SourceResult<()> {
        for occurrence in occurrences {
            self.unit(
                Method::DELETE,
                &format!(
                    "/playlists/{}/items/{}",
                    raw_item_id(playlist),
                    raw_item_id(occurrence)
                ),
                &[],
            )
            .await?;
        }
        Ok(())
    }
    pub(crate) async fn move_playlist_entry(
        &self,
        playlist: &str,
        occurrence: &str,
        previous_position: usize,
        position: usize,
    ) -> SourceResult<()> {
        if previous_position == position {
            return Ok(());
        }
        let previous = if position == 0 {
            "0".into()
        } else {
            let offset = if previous_position < position {
                position
            } else {
                position - 1
            };
            let page = self
                .get(
                    &format!("/playlists/{}/items", raw_item_id(playlist)),
                    &[
                        ("X-Plex-Container-Start", offset.to_string()),
                        ("X-Plex-Container-Size", "1".into()),
                    ],
                )
                .await?;
            items(&page["MediaContainer"]["Metadata"])
                .first()
                .and_then(|entry| id(&entry["playlistItemID"]))
                .ok_or(SourceError::NotFound)?
        };
        self.unit(
            Method::PUT,
            &format!(
                "/playlists/{}/items/{}/move",
                raw_item_id(playlist),
                raw_item_id(occurrence)
            ),
            &[("after", previous)],
        )
        .await
    }
    pub(crate) async fn generated_track_object_ids(
        &self,
        seed: &crate::SourceRadioSeed,
        limit: usize,
    ) -> SourceResult<Vec<String>> {
        let limit = limit.min(100);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let response = match seed {
            crate::SourceRadioSeed::Track(track) => {
                self.get(
                    &format!("/library/metadata/{}/nearest", raw_item_id(track)),
                    &[("limit", limit.to_string())],
                )
                .await?
            }
            crate::SourceRadioSeed::Artist(artist) => {
                let metadata = self.metadata(artist).await?;
                let station = items(&metadata["Stations"]["Metadata"])
                    .iter()
                    .find_map(|station| station["key"].as_str());
                let Some(station) = station else {
                    return Ok(Vec::new());
                };
                return Ok(self
                    .station_candidates(station, limit)
                    .await?
                    .iter()
                    .filter_map(|item| id(&item["ratingKey"]))
                    .map(|id| plex_id("track", &id))
                    .collect());
            }
            crate::SourceRadioSeed::Album(album) => {
                self.get(
                    &format!("/library/metadata/{}/similar", raw_item_id(album)),
                    &[("limit", limit.to_string())],
                )
                .await?
            }
            crate::SourceRadioSeed::Playlist(playlist) => {
                self.get(
                    &format!("/playlists/{}/items", raw_item_id(playlist)),
                    &[
                        ("X-Plex-Container-Start", "0".into()),
                        ("X-Plex-Container-Size", limit.to_string()),
                    ],
                )
                .await?
            }
            crate::SourceRadioSeed::Genre(_) => return Ok(Vec::new()),
        };
        Ok(items(&response["MediaContainer"]["Metadata"])
            .iter()
            .filter(|item| item["type"] == "track")
            .filter_map(|item| id(&item["ratingKey"]))
            .take(limit)
            .map(|id| plex_id("track", &id))
            .collect())
    }
    pub(crate) async fn stage_collection(
        &self,
        scan: &mut library::Scan,
        collection: &crate::SourceCollection,
    ) -> SourceResult<()> {
        let (object, suffix) = match collection {
            crate::SourceCollection::Album(id) => (id, "children"),
            crate::SourceCollection::Artist(id) => (id, "allLeaves"),
        };
        let metadata = self.metadata(object).await?;
        item::stage_item(scan, &metadata).await?;
        let mut offset = 0;
        loop {
            let response = self
                .get(
                    &format!("/library/metadata/{}/{suffix}", raw_item_id(object)),
                    &[
                        ("X-Plex-Container-Start", offset.to_string()),
                        ("X-Plex-Container-Size", "512".into()),
                    ],
                )
                .await?;
            let entries = items(&response["MediaContainer"]["Metadata"]);
            scan.begin_batch().await?;
            for entry in entries {
                item::stage_item(scan, entry).await?;
            }
            scan.finish_batch().await?;
            offset += entries.len();
            if entries.is_empty()
                || field::<usize>(&response["MediaContainer"], "totalSize")
                    .is_some_and(|total| offset >= total)
                || entries.len() < 512
            {
                break;
            }
        }
        Ok(())
    }
    pub(crate) async fn live_search(
        &self,
        source_id: &crate::SourceId,
        database: &library::Database,
        query: &str,
        limit: usize,
    ) -> SourceResult<(library::SearchResults, Option<library::ScanOutcome>)> {
        let response = self
            .get(
                "/hubs/search",
                &[
                    ("query", query.into()),
                    ("limit", limit.min(100).to_string()),
                    ("includeCollections", "0".into()),
                ],
            )
            .await?;
        let mut scan = library::Scan::begin_items(database, source_id.as_str()).await?;
        let source = scan.existing_source().ok_or(SourceError::NotFound)?;
        let (mut tracks, mut albums, mut artists) = (Vec::new(), Vec::new(), Vec::new());
        scan.begin_batch().await?;
        for hub in items(&response["MediaContainer"]["Hub"]) {
            for entry in items(&hub["Metadata"]).iter().take(limit.min(100)) {
                let Some(raw) = id(&entry["ratingKey"]) else {
                    continue;
                };
                match entry["type"].as_str() {
                    Some("track") => tracks.push(plex_id("track", &raw)),
                    Some("album") => albums.push(plex_id("album", &raw)),
                    Some("artist") => artists.push(plex_id("artist", &raw)),
                    _ => continue,
                }
                item::stage_item(&mut scan, entry).await?;
            }
        }
        scan.finish_batch().await?;
        let outcome = scan.finish().await?;
        Ok((
            database
                .search_rows_by_objects(
                    source,
                    None,
                    false,
                    &tracks,
                    &albums,
                    &artists,
                    &library::ReadCancellation::new(),
                )
                .await?,
            Some(outcome),
        ))
    }
    pub(crate) async fn browse_folder(
        &self,
        folder: Option<&str>,
        section: Option<&str>,
    ) -> SourceResult<crate::LiveFolderPage> {
        let path = folder
            .map(|id| format!("/library/metadata/{}/children", raw_item_id(id)))
            .or_else(|| section.map(|id| format!("/library/sections/{}/folder", raw_item_id(id))))
            .unwrap_or_else(|| "/library/sections".into());
        let response = self.get(&path, &[]).await?;
        let container = &response["MediaContainer"];
        let mut page = crate::LiveFolderPage::default();
        for entry in items(&container["Directory"])
            .iter()
            .chain(items(&container["Metadata"]))
        {
            let Some(raw) = id(&entry["ratingKey"]).or_else(|| id(&entry["key"])) else {
                continue;
            };
            if entry["type"] == "track" {
                page.tracks.push(plex_id("track", &raw));
            } else {
                page.folders.push(crate::LiveFolder {
                    object_id: plex_id("folder", &raw),
                    name: entry["title"].as_str().unwrap_or_default().into(),
                });
            }
        }
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn artist_radio_uses_the_returned_station_metadata_key() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path, query_param},
        };
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        Mock::given(method("GET")).and(path("/library/metadata/artist")).and(query_param("includeStations","1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"MediaContainer":{"Metadata":[{"ratingKey":"artist","type":"artist","Stations":{"Metadata":[{"key":"/library/metadata/artist/station/server-selected?type=10","type":"playlist"}]}}]}}))).mount(&server).await;
        Mock::given(method("POST")).and(path("/playQueues")).and(query_param("uri","server://server/com.plexapp.plugins.library/library/metadata/artist/station/server-selected?type=10")).and(query_param("window","5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"MediaContainer":{"Metadata":[{"ratingKey":"song","type":"track"}]}}))).expect(1).mount(&server).await;
        assert_eq!(
            source
                .generated_track_object_ids(
                    &crate::SourceRadioSeed::Artist("plex:artist:artist".into()),
                    5
                )
                .await
                .unwrap(),
            ["plex:track:song"]
        );
    }
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    #[tokio::test]
    async fn playlist_writes_address_occurrences_and_keep_track_order() {
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        Mock::given(method("POST"))
            .and(path("/playlists"))
            .and(query_param(
                "uri",
                "server://server/com.plexapp.plugins.library/library/metadata/one,two",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"MediaContainer":{"Metadata":[{"ratingKey":"mix"}]}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/playlists/mix/items"))
            .and(query_param(
                "uri",
                "server://server/com.plexapp.plugins.library/library/metadata/one,two",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/playlists/mix/items/occurrence-two"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/playlists/mix/items"))
            .and(query_param("X-Plex-Container-Start", "7"))
            .and(query_param("X-Plex-Container-Size", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"MediaContainer":{"Metadata":[{"playlistItemID":"preceding"}]}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/playlists/mix/items/occurrence-three/move"))
            .and(query_param("after", "preceding"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let tracks = vec!["plex:track:one".into(), "plex:track:two".into()];
        let playlist = source.create_playlist("Mix", &tracks).await.unwrap();
        assert_eq!(playlist, "plex:playlist:mix");
        source
            .add_playlist_tracks(&playlist, &tracks)
            .await
            .unwrap();
        source
            .remove_playlist_entries(&playlist, &["occurrence-two".into()])
            .await
            .unwrap();
        source
            .move_playlist_entry(&playlist, "occurrence-three", 2, 7)
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn server_ratings_keep_half_star_steps_and_clear_separately() {
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        for rating in ["7", "-1"] {
            Mock::given(method("PUT"))
                .and(path("/:/rate"))
                .and(query_param("key", "track"))
                .and(query_param("rating", rating))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;
        }
        source
            .set_rating("plex:track:track", Some(7))
            .await
            .unwrap();
        source.set_rating("plex:track:track", None).await.unwrap();
    }
}
