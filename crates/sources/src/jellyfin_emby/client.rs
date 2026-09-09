use super::*;

use crate::remote_http::{self, BodyLimit, RemoteHttpPolicy, RemoteTimeouts};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;

use super::refresh::PageState;

const JELLYFIN_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const JELLYFIN_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
pub(super) const JELLYFIN_JSON_MAX_BYTES: usize = 16 * 1024 * 1024;
pub(super) const JELLYFIN_IMAGE_MAX_BYTES: usize = 32 * 1024 * 1024;
const JELLYFIN_ERROR_BODY_MAX_BYTES: usize = 64 * 1024;
const JELLYFIN_HTTP: RemoteHttpPolicy = RemoteHttpPolicy {
    service: "jellyfin",
    auth_context: "Jellyfin returned",
    error_body: BodyLimit {
        max_bytes: JELLYFIN_ERROR_BODY_MAX_BYTES,
        context: "Jellyfin error response",
    },
    redact_error_url: Some(redact_auth_query),
};

fn redact_auth_query(url: &mut Url) {
    url.set_query(None);
    url.set_fragment(None);
}

fn http_policy(kind: ServerKind) -> RemoteHttpPolicy {
    match kind {
        ServerKind::Jellyfin => JELLYFIN_HTTP,
        ServerKind::Emby => RemoteHttpPolicy {
            service: "emby",
            auth_context: "Emby returned",
            error_body: BodyLimit {
                max_bytes: JELLYFIN_ERROR_BODY_MAX_BYTES,
                context: "Emby error response",
            },
            ..JELLYFIN_HTTP
        },
    }
}

impl JellyfinEmbySource {
    pub(super) async fn authenticated(
        &self,
        request: reqwest::RequestBuilder,
    ) -> SourceResult<reqwest::RequestBuilder> {
        let request = request.header(
            self.kind.authorization_header(),
            self.session_authorization().await?,
        );
        Ok(if self.kind == ServerKind::Emby {
            request.header("X-Emby-Token", self.session_access_token().await?)
        } else {
            request
        })
    }

    pub(super) fn item_url(&self, raw: &str) -> SourceResult<Url> {
        endpoint(
            &self.base_url,
            &match self.kind {
                ServerKind::Jellyfin => format!("Items/{raw}"),
                ServerKind::Emby => format!("Users/{}/Items/{raw}", self.user_id),
            },
        )
    }

    pub(crate) async fn stage_collection(
        &self,
        scan: &mut library::Scan,
        collection: &crate::SourceCollection,
    ) -> SourceResult<()> {
        let mut staged_albums = std::collections::HashSet::new();
        let mut start = 0;
        loop {
            let mut url = endpoint(&self.base_url, "Items")?;
            url.query_pairs_mut()
                .append_pair("UserId", &self.user_id)
                .append_pair("Recursive", "true")
                .append_pair(
                    "IncludeItemTypes",
                    match collection {
                        crate::SourceCollection::Album(_) => "Audio",
                        crate::SourceCollection::Artist(_) => "MusicAlbum,Audio",
                    },
                )
                .append_pair("Fields", MIXED_ITEM_FIELDS)
                .append_pair("SortBy", "ParentIndexNumber,IndexNumber,SortName")
                .append_pair("SortOrder", "Ascending")
                .append_pair("StartIndex", &start.to_string())
                .append_pair("Limit", "100");
            match collection {
                crate::SourceCollection::Album(id) => {
                    url.query_pairs_mut()
                        .append_pair("ParentId", raw_item_id(id));
                }
                crate::SourceCollection::Artist(id) => {
                    url.query_pairs_mut()
                        .append_pair("ArtistIds", raw_item_id(id));
                }
            }
            let page = self.get_json::<Value>(url).await?;
            let count = items(&page["Items"]).len();
            if count == 0 {
                break;
            }
            for item in items(&page["Items"]) {
                if let Some(album_id) = id(&item["AlbumId"]).filter(|_| {
                    matches!(collection, crate::SourceCollection::Album(_)) || is_audio_item(item)
                }) {
                    if staged_albums.insert(album_id.clone()) {
                        let mut album_url = self.item_url(&album_id)?;
                        album_url
                            .query_pairs_mut()
                            .append_pair("UserId", &self.user_id)
                            .append_pair("Fields", ALBUM_FIELDS);
                        let album = self.get_json::<Value>(album_url).await?;
                        scan.begin_batch().await?;
                        if let Some(mapped) = album_from_item(self.kind, album) {
                            stage_album(scan, mapped).await?;
                        }
                        scan.finish_batch().await?;
                    }
                }
            }
            scan.begin_batch().await?;
            for item in items(&page["Items"]).iter().cloned() {
                if matches!(collection, crate::SourceCollection::Album(_)) || is_audio_item(&item) {
                    if let Some(mapped) = track_from_item(self.kind, item) {
                        stage_track(scan, mapped).await?;
                    }
                } else if item["Type"]
                    .as_str()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("MusicAlbum"))
                {
                    if let Some(mapped) = album_from_item(self.kind, item) {
                        stage_album(scan, mapped).await?;
                    }
                }
            }
            scan.finish_batch().await?;
            start += count;
            if count < 100 {
                break;
            }
        }
        Ok(())
    }
    pub(crate) async fn generated_track_object_ids(
        &self,
        seed: &crate::SourceRadioSeed,
        limit: usize,
    ) -> SourceResult<Vec<String>> {
        let raw = match seed {
            crate::SourceRadioSeed::Track(id)
            | crate::SourceRadioSeed::Album(id)
            | crate::SourceRadioSeed::Artist(id)
            | crate::SourceRadioSeed::Playlist(id)
            | crate::SourceRadioSeed::Genre(id) => raw_item_id(id),
        };
        let path = match seed {
            crate::SourceRadioSeed::Track(_) if !self.use_instant_mix => {
                format!("Items/{raw}/Similar")
            }
            _ if self.kind == ServerKind::Emby => format!("Items/{raw}/InstantMix"),
            crate::SourceRadioSeed::Track(_) => format!("Songs/{raw}/InstantMix"),
            crate::SourceRadioSeed::Album(_) => format!("Albums/{raw}/InstantMix"),
            crate::SourceRadioSeed::Artist(_) => format!("Artists/{raw}/InstantMix"),
            crate::SourceRadioSeed::Playlist(_) => format!("Playlists/{raw}/InstantMix"),
            crate::SourceRadioSeed::Genre(_) => "MusicGenres/InstantMix".to_string(),
        };
        let mut url = endpoint(&self.base_url, &path)?;
        url.query_pairs_mut()
            .append_pair("UserId", &self.user_id)
            .append_pair("Limit", &limit.clamp(1, 500).to_string());
        if matches!(seed, crate::SourceRadioSeed::Genre(_)) {
            url.query_pairs_mut().append_pair("Id", raw);
        }
        let mut items = self.get_json::<Value>(url).await?["Items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if items.is_empty()
            && matches!(seed, crate::SourceRadioSeed::Track(_))
            && !self.use_instant_mix
        {
            let kind = match self.kind {
                ServerKind::Jellyfin => "Songs",
                ServerKind::Emby => "Items",
            };
            let mut url = endpoint(&self.base_url, &format!("{kind}/{raw}/InstantMix"))?;
            url.query_pairs_mut()
                .append_pair("UserId", &self.user_id)
                .append_pair("Limit", &limit.clamp(1, 500).to_string());
            items = self.get_json::<Value>(url).await?["Items"]
                .as_array()
                .cloned()
                .unwrap_or_default();
        }
        Ok(items
            .into_iter()
            .filter_map(|item| id(&item["Id"]).map(|raw| self.kind.object_id("track", &raw)))
            .collect())
    }

    pub(crate) async fn browse_folder(
        &self,
        folder_object_id: Option<&str>,
        music_folder_object_id: Option<&str>,
    ) -> SourceResult<crate::LiveFolderPage> {
        let parent = folder_object_id.or(music_folder_object_id);
        let path = parent
            .map(|_| "Items".to_string())
            .unwrap_or_else(|| format!("Users/{}/Views", self.user_id));
        let mut url = endpoint(&self.base_url, &path)?;
        if let Some(folder) = parent {
            url.query_pairs_mut()
                .append_pair("UserId", &self.user_id)
                .append_pair("ParentId", raw_item_id(folder))
                .append_pair("Recursive", "false")
                .append_pair("Fields", TRACK_FIELDS)
                .append_pair("SortBy", "SortName")
                .append_pair("SortOrder", "Ascending");
        }
        let response = self.get_json::<Value>(url).await?;
        let mut page = crate::LiveFolderPage::default();
        for item in items(&response["Items"]).iter().cloned() {
            let Some(raw_id) = id(&item["Id"]) else {
                continue;
            };
            if parent.is_none()
                && !item["CollectionType"]
                    .as_str()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("music"))
            {
                continue;
            }
            if is_audio_item(&item) {
                page.tracks.push(self.kind.object_id("track", &raw_id));
            } else if let Some(name) = field::<String>(&item, "Name") {
                page.folders.push(crate::LiveFolder {
                    object_id: self.kind.object_id("folder", &raw_id),
                    name,
                });
            }
        }
        Ok(page)
    }

    pub(crate) async fn live_search(
        &self,
        source_id: &SourceId,
        database: &library::Database,
        query: &str,
        limit: usize,
    ) -> SourceResult<(library::SearchResults, Option<library::ScanOutcome>)> {
        if query.trim().is_empty() {
            return Ok((library::SearchResults::default(), None));
        }
        let limit = limit.clamp(1, 100);
        let (artists, albums, tracks) = tokio::join!(
            self.search_people(query, limit),
            self.search_items("MusicAlbum", ALBUM_FIELDS, query, limit),
            self.search_items("Audio", TRACK_FIELDS, query, limit),
        );
        if artists.is_err() && albums.is_err() && tracks.is_err() {
            return Err(tracks.err().expect("failed track search"));
        }
        let artists = artists.unwrap_or_default();
        let albums = albums.unwrap_or_default();
        let tracks = tracks.unwrap_or_default();
        let mut scan = library::Scan::begin_items(database, source_id.as_str()).await?;
        let source = scan
            .existing_source()
            .ok_or(SourceError::InvalidRequest("Search source is unavailable"))?;
        scan.begin_batch().await?;
        let mut artist_ids = Vec::new();
        let mut album_ids = Vec::new();
        let mut track_ids = Vec::new();
        for artist in artists
            .into_iter()
            .filter_map(|item| artist_from_item(self.kind, item))
        {
            artist_ids.push(artist.id.clone());
            stage_artist(&mut scan, artist).await?;
        }
        for album in albums
            .into_iter()
            .filter_map(|item| album_from_item(self.kind, item))
        {
            album_ids.push(album.id.clone());
            stage_album(&mut scan, album).await?;
        }
        for track in tracks
            .into_iter()
            .filter_map(|item| track_from_item(self.kind, item))
        {
            track_ids.push(track.id.clone());
            stage_track(&mut scan, track).await?;
        }
        scan.finish_batch().await?;
        let outcome = scan.finish().await?;
        let rows = database
            .search_rows_by_objects(
                source,
                None,
                false,
                &track_ids,
                &album_ids,
                &artist_ids,
                &library::ReadCancellation::new(),
            )
            .await?;
        Ok((rows, Some(outcome)))
    }

    async fn search_items(
        &self,
        item_types: &str,
        fields: &str,
        query: &str,
        limit: usize,
    ) -> SourceResult<Vec<Value>> {
        let mut url = endpoint(&self.base_url, "Items")?;
        url.query_pairs_mut()
            .append_pair("UserId", &self.user_id)
            .append_pair("Recursive", "true")
            .append_pair("IncludeItemTypes", item_types)
            .append_pair("SearchTerm", query)
            .append_pair("StartIndex", "0")
            .append_pair("Limit", &limit.to_string())
            .append_pair("Fields", fields);
        let body: serde_json::Value = self.get_json(url).await?;
        Ok(body["Items"]
            .as_array()
            .into_iter()
            .flatten()
            .take(limit)
            .cloned()
            .collect())
    }
    async fn search_people(&self, query: &str, limit: usize) -> SourceResult<Vec<Value>> {
        let mut url = endpoint(&self.base_url, "Artists")?;
        url.query_pairs_mut().append_pair("UserId",&self.user_id).append_pair("SearchTerm",query).append_pair("StartIndex","0").append_pair("Limit",&limit.to_string()).append_pair("Fields","ParentId,UserData,ItemCounts,ChildCount,AlbumCount,SongCount,ImageTags,ProviderIds,SortName");
        let body: serde_json::Value = self.get_json(url).await?;
        Ok(body["Items"]
            .as_array()
            .into_iter()
            .flatten()
            .take(limit)
            .cloned()
            .collect())
    }
}

impl JellyfinEmbySource {
    pub(super) async fn stage_playlist_entries(
        &self,
        scan: &mut library::Scan,
        playlist_id: &str,
    ) -> SourceResult<()> {
        let raw_playlist_id = raw_item_id(playlist_id);
        let mut pages = PageState::default();
        loop {
            let mut url = endpoint(
                &self.base_url,
                &format!("Playlists/{raw_playlist_id}/Items"),
            )?;
            {
                let mut query = url.query_pairs_mut();
                query
                    .append_pair("UserId", &self.user_id)
                    .append_pair("StartIndex", &pages.offset().to_string())
                    .append_pair("Limit", &COLLECTION_PAGE_SIZE.to_string());
            }
            let response = self.get_json::<Value>(url).await?;
            let count = items(&response["Items"]).len();
            let page_start = pages.offset();
            let finished = pages.advance(count, field(&response, "TotalRecordCount"))?;
            scan.begin_batch().await?;
            for (offset, item) in items(&response["Items"]).iter().cloned().enumerate() {
                let Some((entry_id, track_id, position)) =
                    playlist_entry(self.kind, item, page_start + offset)
                else {
                    continue;
                };
                scan.write_playlist_entry(playlist_id, &entry_id, &track_id, position)
                    .await?;
            }
            scan.finish_batch().await?;
            if finished {
                return Ok(());
            }
        }
    }
}

fn playlist_entry(
    server: ServerKind,
    item: Value,
    position: usize,
) -> Option<(String, String, i64)> {
    let track_id = id(&item["Id"])?;
    let entry_id = id(&item["PlaylistItemId"])?;
    Some((
        entry_id,
        server.object_id("track", &track_id),
        position as i64,
    ))
}

impl JellyfinEmbySource {
    pub(crate) async fn resolve_stream(
        &self,
        track_object_id: &str,
        quality: StreamQuality,
        session_identifier: Option<&str>,
    ) -> SourceResult<ResolvedStream> {
        if self.kind == ServerKind::Emby {
            return self
                .emby_stream(track_object_id, quality, session_identifier, false)
                .await;
        }
        let stream = stream_descriptor(
            &self.base_url,
            &self.user_id,
            &self.device_id,
            &self.access_token,
            self.trust_invalid_cert,
            track_object_id,
            quality,
        )?;
        let mut url =
            Url::parse(stream.uri()).map_err(|error| SourceError::Other(error.to_string()))?;
        if let Some(session) = session_identifier {
            url.query_pairs_mut().append_pair("PlaySessionId", session);
        }
        Ok(ResolvedStream::new(url.to_string())
            .with_content_type(stream.content_type)
            .with_trust_invalid_certificate(self.trust_invalid_cert)
            .with_transcoding(quality != StreamQuality::Original))
    }

    pub(crate) async fn resolve_download(
        &self,
        track: &str,
        quality: StreamQuality,
    ) -> SourceResult<crate::ResolvedDownload> {
        match self.kind {
            ServerKind::Jellyfin => self.jellyfin_download(track, quality),
            ServerKind::Emby => Ok(crate::ResolvedDownload::new(
                self.emby_stream(track, quality, None, true).await?,
                (quality != StreamQuality::Original).then_some("mp3"),
            )),
        }
    }

    pub(crate) async fn create_playlist(
        &self,
        name: &str,
        tracks: &[String],
    ) -> SourceResult<PlaylistId> {
        match self.kind {
            ServerKind::Jellyfin => self.jellyfin_create_playlist(name, tracks).await,
            ServerKind::Emby => self.emby_create_playlist(name, tracks).await,
        }
    }

    pub(crate) async fn rename_playlist(&self, playlist: &str, name: &str) -> SourceResult<()> {
        match self.kind {
            ServerKind::Jellyfin => self.jellyfin_rename_playlist(playlist, name).await,
            ServerKind::Emby => self.emby_rename_playlist(playlist, name).await,
        }
    }
}

impl JellyfinEmbySource {
    pub(crate) async fn set_favorite(&self, object_id: &str, favorite: bool) -> SourceResult<()> {
        let mut url = endpoint(
            &self.base_url,
            &match self.kind {
                ServerKind::Jellyfin => format!("UserFavoriteItems/{}", raw_item_id(object_id)),
                ServerKind::Emby => format!(
                    "Users/{}/FavoriteItems/{}",
                    self.user_id,
                    raw_item_id(object_id)
                ),
            },
        )?;
        url.query_pairs_mut().append_pair("userId", &self.user_id);
        if favorite {
            self.send_unit(self.client.post(url)).await
        } else {
            self.send_unit(self.client.delete(url)).await
        }
    }
    pub(crate) async fn set_rating(&self, object_id: &str, rating: Option<u8>) -> SourceResult<()> {
        // Emby's UserData write updates play state, not numeric ratings. The core
        // already persists the user's stars locally before calling this method.
        if self.kind == ServerKind::Emby {
            return Ok(());
        }
        let mut url = endpoint(
            &self.base_url,
            &format!("UserItems/{}/UserData", raw_item_id(object_id)),
        )?;
        url.query_pairs_mut().append_pair("userId", &self.user_id);
        self.send_unit(
            self.client
                .post(url)
                .json(&serde_json::json!({"Rating":rating.unwrap_or(0)})),
        )
        .await
    }

    pub(crate) async fn image_bytes(
        &self,
        image_ref: &ImageRef,
        size: u32,
    ) -> SourceResult<ImageBytes> {
        let image_kind = if image_ref
            .item_id
            .starts_with(&self.kind.object_id("backdrop", ""))
        {
            "Backdrop"
        } else {
            "Primary"
        };
        let mut url = endpoint(
            &self.base_url,
            &format!(
                "Items/{}/Images/{}",
                raw_item_id(&image_ref.item_id),
                image_kind
            ),
        )?;
        url.query_pairs_mut()
            .append_pair("fillWidth", &size.max(1).to_string())
            .append_pair("fillHeight", &size.max(1).to_string())
            .append_pair("quality", "90");
        if let Some(tag) = image_ref.tag.as_deref().filter(|tag| !tag.is_empty()) {
            url.query_pairs_mut().append_pair("tag", tag);
        }
        send_bytes(self.kind, self.authenticated(self.client.get(url)).await?).await
    }
}

impl JellyfinEmbySource {
    pub(crate) async fn delete_playlist(&self, playlist_id: &str) -> SourceResult<()> {
        let url = endpoint(
            &self.base_url,
            &format!("Items/{}", raw_item_id(playlist_id)),
        )?;
        self.send_unit(self.client.delete(url)).await
    }
    pub(crate) async fn add_playlist_tracks(
        &self,
        playlist_id: &str,
        track_ids: &[String],
    ) -> SourceResult<()> {
        for track_ids in track_ids.chunks(50) {
            let mut url = endpoint(
                &self.base_url,
                &format!("Playlists/{}/Items", raw_item_id(playlist_id)),
            )?;
            url.query_pairs_mut()
                .append_pair("userId", &self.user_id)
                .append_pair("ids", &raw_track_ids(track_ids).join(","));
            self.send_unit(self.client.post(url)).await?;
        }
        Ok(())
    }
    pub(crate) async fn remove_playlist_entries(
        &self,
        playlist_id: &str,
        entry_ids: &[String],
    ) -> SourceResult<()> {
        for entry_ids in entry_ids.chunks(50) {
            let mut url = endpoint(
                &self.base_url,
                &format!("Playlists/{}/Items", raw_item_id(playlist_id)),
            )?;
            url.query_pairs_mut()
                .append_pair("entryIds", &entry_ids.join(","));
            self.send_unit(self.client.delete(url)).await?;
        }
        Ok(())
    }
    pub(crate) async fn move_playlist_entry(
        &self,
        playlist_id: &str,
        entry_id: &str,
        new_index: usize,
    ) -> SourceResult<()> {
        let url = endpoint(
            &self.base_url,
            &format!(
                "Playlists/{}/Items/{}/Move/{}",
                raw_item_id(playlist_id),
                raw_item_id(entry_id),
                new_index
            ),
        )?;
        self.send_unit(self.client.post(url)).await
    }
}

impl JellyfinEmbySource {
    pub(crate) async fn lyrics(&self, track_id: &str) -> SourceResult<Option<LyricsBundle>> {
        match self.kind {
            ServerKind::Jellyfin => self.jellyfin_lyrics(track_id).await,
            ServerKind::Emby => self.emby_lyrics(track_id).await,
        }
    }
}

impl JellyfinEmbySource {
    pub(crate) async fn report_playback(
        &self,
        track_object_id: &str,
        report: &SourceReportFact,
    ) -> SourceResult<()> {
        let path = match report.phase {
            SourceReportPhase::Started => "Sessions/Playing",
            SourceReportPhase::Progress => "Sessions/Playing/Progress",
            SourceReportPhase::QualifiedPlay => return Ok(()),
            SourceReportPhase::Ended => "Sessions/Playing/Stopped",
        };
        let url = endpoint(&self.base_url, path)?;
        let mut body =
            serde_json::to_value(PlaybackReportDto::from_report(track_object_id, report))?;
        if self.kind == ServerKind::Emby {
            let object = body.as_object_mut().unwrap();
            object.remove("PlaybackOrder");
            object.insert("Shuffle".into(), Value::Bool(report.shuffle));
            if report.phase == SourceReportPhase::Ended {
                object.retain(|key, _| {
                    matches!(
                        key.as_str(),
                        "ItemId" | "PlaySessionId" | "PositionTicks" | "Failed"
                    )
                });
            }
        }
        self.send_unit(self.client.post(url).json(&body)).await
    }
}

pub(super) async fn public_server_name(
    client: &Client,
    base_url: &Url,
    config: &JellyfinEmbyClientConfig,
) -> Option<String> {
    let url = endpoint(base_url, "System/Info/Public").ok()?;
    let response = send_json::<Value>(
        config.kind,
        client.get(url).header(
            config.kind.authorization_header(),
            auth_header(config, None),
        ),
    )
    .await
    .ok()?;
    field::<String>(&response, "ServerName").or_else(|| field(&response, "LocalAddress"))
}
pub(super) async fn send_json<T: DeserializeOwned>(
    kind: ServerKind,
    request: reqwest::RequestBuilder,
) -> SourceResult<T> {
    remote_http::json(
        request,
        http_policy(kind),
        BodyLimit {
            max_bytes: JELLYFIN_JSON_MAX_BYTES,
            context: kind.name(),
        },
    )
    .await
}
pub(super) async fn send_unit(
    kind: ServerKind,
    request: reqwest::RequestBuilder,
) -> SourceResult<()> {
    remote_http::unit(request, http_policy(kind)).await
}
pub(super) async fn send_bytes(
    kind: ServerKind,
    request: reqwest::RequestBuilder,
) -> SourceResult<ImageBytes> {
    remote_http::bytes(
        request,
        http_policy(kind),
        BodyLimit {
            max_bytes: JELLYFIN_IMAGE_MAX_BYTES,
            context: kind.name(),
        },
    )
    .await
}
pub(super) fn build_client(trust_invalid_cert: bool) -> SourceResult<Client> {
    build_client_with_timeouts(
        trust_invalid_cert,
        JELLYFIN_CONNECT_TIMEOUT,
        JELLYFIN_REQUEST_TIMEOUT,
    )
}

pub(super) fn build_websocket_client(trust_invalid_cert: bool) -> SourceResult<Client> {
    remote_http::build_http1_client(
        trust_invalid_cert,
        RemoteTimeouts {
            connect: JELLYFIN_CONNECT_TIMEOUT,
            request: JELLYFIN_REQUEST_TIMEOUT,
        },
        JELLYFIN_HTTP,
    )
}

pub(super) fn build_client_with_timeouts(
    trust_invalid_cert: bool,
    connect_timeout: Duration,
    request_timeout: Duration,
) -> SourceResult<Client> {
    remote_http::build_client(
        trust_invalid_cert,
        RemoteTimeouts {
            connect: connect_timeout,
            request: request_timeout,
        },
        JELLYFIN_HTTP,
    )
}

pub(crate) fn normalize_base_url(raw: &str) -> SourceResult<Url> {
    let trimmed = raw.trim().trim_end_matches('/');
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = Url::parse(&candidate).map_err(|error| SourceError::Other(error.to_string()))?;
    let path = url.path().trim_end_matches('/').to_string();
    let normalized_path = if path.is_empty() {
        "/".to_string()
    } else {
        format!("{path}/")
    };
    url.set_path(&normalized_path);
    Ok(url)
}
pub(super) fn endpoint(base_url: &Url, path: &str) -> SourceResult<Url> {
    let mut url = base_url.clone();
    let base_path = base_url.path().trim_end_matches('/');
    let path = path.trim_start_matches('/');
    let full_path = if base_path.is_empty() {
        format!("/{path}")
    } else {
        format!("{base_path}/{path}")
    };
    url.set_path(&full_path);
    url.set_query(None);
    Ok(url)
}
pub(super) fn auth_header(config: &JellyfinEmbyClientConfig, token: Option<&str>) -> String {
    let mut value = format!(
        "{} Client=\"{}\", Device=\"{}\", DeviceId=\"{}\", Version=\"{}\"",
        match config.kind {
            ServerKind::Jellyfin => "MediaBrowser",
            ServerKind::Emby => "Emby",
        },
        config.client_name,
        config.device_name,
        config.device_id,
        config.client_version
    );
    if let Some(token) = token {
        value.push_str(&format!(", Token=\"{token}\""));
    }
    value
}
pub(super) fn raw_track_ids(track_ids: &[String]) -> Vec<String> {
    track_ids
        .iter()
        .map(|id| raw_item_id(id.as_str()).to_string())
        .collect()
}
pub(super) fn ticks_to_millis(ticks: Option<i64>) -> Option<u64> {
    ticks.map(|value| (value.max(0) / 10_000) as u64)
}

impl JellyfinEmbySource {
    pub(super) async fn item_page(
        &self,
        include_types: &str,
        offset: usize,
        limit: usize,
    ) -> SourceResult<Value> {
        self.item_page_sorted(include_types, offset, limit, "SortName", "Ascending")
            .await
    }

    pub(super) async fn item_page_sorted(
        &self,
        include_types: &str,
        offset: usize,
        limit: usize,
        sort_by: &str,
        sort_order: &str,
    ) -> SourceResult<Value> {
        let fields = match include_types {
            "MusicAlbum" => ALBUM_FIELDS,
            "Audio" => TRACK_FIELDS,
            "Playlist" => PLAYLIST_FIELDS,
            _ => MIXED_ITEM_FIELDS,
        };
        let mut url = endpoint(&self.base_url, "Items")?;
        url.query_pairs_mut()
            .append_pair("UserId", &self.user_id)
            .append_pair("Recursive", "true")
            .append_pair("IncludeItemTypes", include_types)
            .append_pair("StartIndex", &offset.to_string())
            .append_pair("Limit", &limit.to_string())
            .append_pair("Fields", fields)
            .append_pair("SortBy", sort_by)
            .append_pair("SortOrder", sort_order);

        self.get_json::<Value>(url).await
    }

    pub(super) async fn people_page(
        &self,
        path: &str,
        offset: usize,
        limit: usize,
    ) -> SourceResult<Value> {
        let mut url = endpoint(&self.base_url, path)?;
        url.query_pairs_mut()
            .append_pair("UserId", &self.user_id)
            .append_pair("StartIndex", &offset.to_string())
            .append_pair("Limit", &limit.to_string())
            .append_pair(
                "Fields",
                "ParentId,UserData,ItemCounts,ChildCount,AlbumCount,SongCount,ImageTags,ProviderIds,SortName",
            );

        self.get_json::<Value>(url).await
    }

    pub(super) async fn music_genre_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> SourceResult<Value> {
        let mut url = endpoint(&self.base_url, "MusicGenres")?;
        url.query_pairs_mut()
            .append_pair("UserId", &self.user_id)
            .append_pair("StartIndex", &offset.to_string())
            .append_pair("Limit", &limit.to_string())
            .append_pair("IncludeItemTypes", "Audio,MusicAlbum")
            .append_pair(
                "Fields",
                "UserData,ItemCounts,ChildCount,AlbumCount,SongCount,ImageTags",
            )
            .append_pair("SortBy", "SortName");

        self.get_json::<Value>(url).await
    }

    pub(super) async fn get_json<T: DeserializeOwned>(&self, mut url: Url) -> SourceResult<T> {
        if self.kind == ServerKind::Emby {
            let params = url
                .query_pairs()
                .map(|(key, value)| {
                    let value = if key.eq_ignore_ascii_case("Fields") {
                        let mut fields: Vec<_> = value
                            .split(',')
                            .filter(|field| {
                                !matches!(*field, "NormalizationGain" | "AlbumNormalizationGain")
                            })
                            .collect();
                        fields.push("UserDataLastPlayedDate");
                        fields.join(",")
                    } else {
                        value.into_owned()
                    };
                    (key.into_owned(), value)
                })
                .collect::<Vec<_>>();
            url.query_pairs_mut().clear().extend_pairs(params);
        }
        self.send_json(self.client.get(url)).await
    }

    pub(super) async fn send_json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> SourceResult<T> {
        send_json(self.kind, self.authenticated(request).await?).await
    }

    pub(super) async fn send_unit(&self, request: reqwest::RequestBuilder) -> SourceResult<()> {
        send_unit(self.kind, self.authenticated(request).await?).await
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct AuthenticateByNameRequest {
    pub(super) username: String,
    #[serde(rename = "Pw")]
    pub(super) password: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct PlaylistCreationResult {
    pub(super) id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct PlaybackReportDto {
    pub(super) play_session_id: String,
    pub(super) can_seek: bool,
    pub(super) item_id: String,
    pub(super) is_paused: bool,
    pub(super) is_muted: bool,
    pub(super) position_ticks: i64,
    pub(super) volume_level: i32,
    pub(super) play_method: &'static str,
    pub(super) repeat_mode: &'static str,
    pub(super) playback_order: &'static str,
    pub(super) failed: bool,
}

impl PlaybackReportDto {
    pub(super) fn from_report(track_object_id: &str, report: &SourceReportFact) -> Self {
        Self {
            play_session_id: report.session_identifier.clone(),
            can_seek: true,
            item_id: raw_item_id(track_object_id).to_string(),
            is_paused: report.paused,
            is_muted: report.muted,
            position_ticks: report
                .position_millis
                .saturating_mul(10_000)
                .min(i64::MAX as u64) as i64,
            volume_level: (report.volume.clamp(0.0, 1.0) * 100.0).round() as i32,
            play_method: if report.transcoded {
                "Transcode"
            } else {
                "DirectPlay"
            },
            repeat_mode: match report.repeat_mode {
                RepeatMode::Off => "RepeatNone",
                RepeatMode::One => "RepeatOne",
                RepeatMode::All => "RepeatAll",
            },
            playback_order: if report.shuffle { "Shuffle" } else { "Default" },
            failed: report.failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{playlist_entry, stream_descriptor};
    use crate::jellyfin_emby::item::{album_from_item, stage_album, stage_track, track_from_item};
    use crate::jellyfin_emby::{JellyfinEmbySource, JellyfinEmbySourceConfig};
    use serde_json::Value;
    use wiremock::matchers::{body_string, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn authentication_ignores_optional_profile_fields_but_requires_credentials() {
        for (response, succeeds) in [
            (
                serde_json::json!({"AccessToken":"token","User":{"Id":42,"Name":{}},"ServerId":[]}),
                true,
            ),
            (
                serde_json::json!({"AccessToken":"token","User":{"Name":"Listener"}}),
                false,
            ),
            (
                serde_json::json!({"AccessToken":"","User":{"Id":"user"}}),
                false,
            ),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/Users/AuthenticateByName"))
                .respond_with(ResponseTemplate::new(200).set_body_json(response))
                .mount(&server)
                .await;
            let result = JellyfinEmbySource::authenticate(
                crate::SourceId::new("source"),
                crate::JellyfinEmbySetupInput {
                    kind: crate::ServerKind::Jellyfin,
                    credentials: crate::CredentialHostInput {
                        server_name: None,
                        server_url: server.uri(),
                        username: "listener".into(),
                        password: "password".into(),
                        trust_invalid_cert: false,
                    },
                    device_id: "device".into(),
                    use_instant_mix: false,
                },
            )
            .await;
            assert_eq!(result.is_ok(), succeeds);
            if let Ok(authenticated) = result {
                let config =
                    JellyfinEmbySourceConfig::from_configuration(&authenticated.configuration)
                        .unwrap();
                assert_eq!(config.user_id, "42");
                assert_eq!(config.username, "listener");
            }
        }
    }

    #[tokio::test]
    async fn playlist_pages_preserve_positions_after_missing_identity() {
        let server = MockServer::start().await;
        for (offset, entries) in [
            (
                "0",
                serde_json::json!([
                    {"Id":"track","PlaylistItemId":"one","UserData":[]},
                    {"PlaylistItemId":"missing"}
                ]),
            ),
            (
                "2",
                serde_json::json!([{ "Id":"track","PlaylistItemId":"two","Name":{} }]),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path("/Playlists/list/Items"))
                .and(query_param("StartIndex", offset))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({"Items":entries,"TotalRecordCount":3})),
                )
                .expect(1)
                .mount(&server)
                .await;
        }
        let source = JellyfinEmbySource::open(
            JellyfinEmbySourceConfig {
                emby_connect: false,
                kind: crate::ServerKind::Jellyfin,
                base_url: server.uri(),
                server_id: None,
                user_id: "user".into(),
                username: "listener".into(),
                trust_invalid_cert: false,
                use_instant_mix: false,
            },
            "token".into(),
            "device".into(),
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let database = library::Database::open(root.path().join("library.sqlite"))
            .await
            .unwrap();
        let mut scan = library::Scan::begin(&database, "source", "Jellyfin", "jellyfin", None)
            .await
            .unwrap();
        stage_track(
            &mut scan,
            track_from_item(
                crate::ServerKind::Jellyfin,
                serde_json::json!({"Id":"track","Name":"Track"}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        scan.write_playlist("jellyfin:playlist:list", "List", "list", "list", None)
            .await
            .unwrap();
        source
            .stage_playlist_entries(&mut scan, "jellyfin:playlist:list")
            .await
            .unwrap();
        let library::ScanOutcome::Changed(publication) = scan.finish().await.unwrap() else {
            panic!("published playlist")
        };
        let cancellation = library::ReadCancellation::new();
        let key = database
            .playlist_key_by_object(publication.source, "jellyfin:playlist:list", &cancellation)
            .await
            .unwrap()
            .unwrap();
        let page = database
            .playlist_detail_page(
                key,
                None,
                library::PlaylistEntrySort::Position,
                false,
                library::RouteSeedWindow::top(),
                &cancellation,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            page.first_rows
                .iter()
                .map(|row| row.position)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(
            database
                .source_playlist_entry_object_ids(
                    publication.source,
                    key,
                    &page.order,
                    &cancellation
                )
                .await
                .unwrap(),
            ["one", "two"]
        );
        assert_eq!(page.first_rows[0].media_uri, page.first_rows[1].media_uri);
        assert_ne!(
            page.first_rows[0].playlist_entry_key,
            page.first_rows[1].playlist_entry_key
        );
    }

    #[test]
    fn playlist_entries_keep_provider_occurrences_when_tracks_repeat() {
        let items: Vec<Value> = serde_json::from_value(serde_json::json!([{
            "Id": "track-one",
            "Type": "Audio",
            "PlaylistItemId": "entry-one"
        }, {
            "Id": "track-one",
            "Type": "Audio",
            "PlaylistItemId": "entry-two"
        }]))
        .expect("playlist items");
        let entries = items
            .into_iter()
            .enumerate()
            .map(|(position, item)| {
                playlist_entry(crate::ServerKind::Jellyfin, item, position).expect("complete entry")
            })
            .collect::<Vec<_>>();

        assert_eq!(entries[0].0, "entry-one");
        assert_eq!(entries[1].0, "entry-two");
        assert_eq!(entries[0].1, entries[1].1);
        assert_eq!((entries[0].2, entries[1].2), (0, 1));
    }

    #[tokio::test]
    async fn song_only_search_keeps_album_text_and_existing_relationship() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/Items")).and(query_param("IncludeItemTypes", "Audio"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"Items":[{
                "Id":"song","Name":"Match","Type":"Audio","Album":"Nonmatching album","AlbumId":"album","ImageTags":{"Primary":"song-cover","Broken":[]},"UserData":{"Rating":{}},"ArtistItems":[null,{"Id":"artist","Name":"Artist"}]
            },{"Type":"Audio","Name":"Missing identity"}],"TotalRecordCount":2}))).mount(&server).await;
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
        for cached in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let database = library::Database::open(root.path().join("library.sqlite"))
                .await
                .unwrap();
            let source_id = crate::SourceId::new("song-search");
            let mut scan =
                library::Scan::begin(&database, source_id.as_str(), "Search", "search", None)
                    .await
                    .unwrap();
            if cached {
                stage_album(&mut scan, album_from_item(crate::ServerKind::Jellyfin, serde_json::from_value(serde_json::json!({"Id":"album","Name":"Nonmatching album","Type":"MusicAlbum","ImageTags":{"Primary":"album-cover"}})).unwrap()).unwrap()).await.unwrap();
            }
            scan.finish().await.unwrap();
            let (results, outcome) = source
                .live_search(&source_id, &database, "Match", 10)
                .await
                .unwrap();
            assert!(matches!(outcome, Some(library::ScanOutcome::Changed(_))));
            assert!(results.albums.is_empty());
            assert_eq!(results.tracks.len(), 1);
            let track = &results.tracks[0];
            assert_eq!(track.album, "Nonmatching album");
            assert_eq!(track.album_key.is_some(), cached);
            assert!(track.artwork_binding.is_some());
        }
    }

    #[test]
    fn playlist_entries_reject_missing_or_empty_provider_occurrences() {
        for item in [
            serde_json::json!({"Id": "track-one", "Type": "Audio"}),
            serde_json::json!({
                "Id": "track-one",
                "Type": "Audio",
                "PlaylistItemId": ""
            }),
        ] {
            let item: Value = serde_json::from_value(item).expect("playlist item");
            assert!(playlist_entry(crate::ServerKind::Jellyfin, item, 0).is_none());
        }
    }

    #[test]
    fn playback_stream_keeps_authentication_out_of_diagnostics() {
        let stream = stream_descriptor(
            &reqwest::Url::parse("https://music.example/jellyfin/").expect("base URL"),
            "user-one",
            "device-one",
            "secret-token",
            false,
            "track-one",
            playback::StreamQuality::Original,
        )
        .expect("stream descriptor");

        assert!(stream.uri().contains("secret-token"));
        assert!(!stream.redacted_uri().contains("secret-token"));
        assert!(stream.redacted_uri().contains("api_key=%3Credacted%3E"));
    }

    #[tokio::test]
    async fn jellyfin_lyrics_write_uses_the_managed_upload_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Audio/track-one/Lyrics"))
            .and(query_param("fileName", "lyrics.lrc"))
            .and(body_string("[00:01.000]Line"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Lyrics": []
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

        source
            .write_lyrics("jellyfin:track:track-one", "[00:01.000]Line")
            .await
            .expect("upload lyrics");
    }

    #[tokio::test]
    async fn recommendation_policy_is_shared_and_preserves_similar_candidates() {
        for kind in [crate::ServerKind::Jellyfin, crate::ServerKind::Emby] {
            for use_instant_mix in [false, true] {
                for similar_is_empty in [false, true] {
                    let server = MockServer::start().await;
                    let prefix = if kind == crate::ServerKind::Emby {
                        "/emby"
                    } else {
                        ""
                    };
                    let mix_kind = if kind == crate::ServerKind::Emby {
                        "Items"
                    } else {
                        "Songs"
                    };
                    let similar_items = if similar_is_empty {
                        serde_json::json!([])
                    } else {
                        serde_json::json!([{"Id":"similar-track"}])
                    };
                    Mock::given(method("GET"))
                        .and(path(format!("{prefix}/Items/track-one/Similar")))
                        .respond_with(
                            ResponseTemplate::new(200)
                                .set_body_json(serde_json::json!({"Items":similar_items})),
                        )
                        .expect(u64::from(!use_instant_mix))
                        .mount(&server)
                        .await;
                    Mock::given(method("GET"))
                        .and(path(format!("{prefix}/{mix_kind}/track-one/InstantMix")))
                        .respond_with(
                            ResponseTemplate::new(200)
                                .set_body_json(serde_json::json!({"Items":[{"Id":"mix-track"}]})),
                        )
                        .expect(u64::from(use_instant_mix || similar_is_empty))
                        .mount(&server)
                        .await;
                    let source = JellyfinEmbySource::open(
                        JellyfinEmbySourceConfig {
                            kind,
                            emby_connect: false,
                            base_url: server.uri(),
                            server_id: Some("server-one".into()),
                            user_id: "user-one".into(),
                            username: "listener".into(),
                            trust_invalid_cert: false,
                            use_instant_mix,
                        },
                        "secret-token".into(),
                        "device-one".into(),
                    )
                    .unwrap();
                    let expected = if use_instant_mix || similar_is_empty {
                        "mix-track"
                    } else {
                        "similar-track"
                    };
                    assert_eq!(
                        source
                            .generated_track_object_ids(
                                &crate::SourceRadioSeed::Track(
                                    kind.object_id("track", "track-one")
                                ),
                                20,
                            )
                            .await
                            .unwrap(),
                        [kind.object_id("track", expected)]
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn album_tracks_use_requested_type_and_numeric_metadata_strings() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/Items"))
            .and(query_param("IncludeItemTypes", "Audio"))
            .and(query_param("ParentId", "album"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items":[{"Id":"track","Name":"Track","RunTimeTicks":"420000000","ProductionYear":"2024"}]
            }))).expect(1).mount(&server).await;
        let source = JellyfinEmbySource::open(
            JellyfinEmbySourceConfig {
                emby_connect: false,
                kind: crate::ServerKind::Jellyfin,
                base_url: server.uri(),
                server_id: None,
                user_id: "user".into(),
                username: "listener".into(),
                trust_invalid_cert: false,
                use_instant_mix: false,
            },
            "token".into(),
            "device".into(),
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let database = library::Database::open(root.path().join("library.sqlite"))
            .await
            .unwrap();
        let mut scan = library::Scan::begin(&database, "source", "Jellyfin", "jellyfin", None)
            .await
            .unwrap();
        source
            .stage_collection(
                &mut scan,
                &crate::SourceCollection::Album("jellyfin:album:album".into()),
            )
            .await
            .unwrap();
        scan.finish().await.unwrap();
        let track = database
            .track_row_by_uri(
                &library::source_entity_uri(
                    &crate::SourceId::new("source"),
                    "track",
                    "jellyfin:track:track",
                ),
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap()
            .expect("album track without repeated Type");
        assert_eq!(track.duration_millis, 42_000);
        assert_eq!(track.year, Some(2024));
    }

    #[tokio::test]
    async fn playback_userdata_updates_preserve_catalog_artwork_and_live_favorites() {
        for kind in [crate::ServerKind::Jellyfin, crate::ServerKind::Emby] {
            let server = MockServer::start().await;
            let source = JellyfinEmbySource::open(
                JellyfinEmbySourceConfig {
                    emby_connect: false,
                    kind,
                    base_url: server.uri(),
                    server_id: None,
                    user_id: "user".into(),
                    username: "listener".into(),
                    trust_invalid_cert: false,
                    use_instant_mix: false,
                },
                "token".into(),
                "device".into(),
            )
            .unwrap();
            let album = serde_json::json!({
                "Id":"album", "Name":"Album", "Type":"MusicAlbum",
                "AlbumArtists":[{"Id":"artist","Name":"Artist"}],
                "GenreItems":[{"Id":"genre","Name":"Rock"}],
                "ImageTags":{"Primary":"album-cover"}
            });
            let track = serde_json::json!({
                "Id":"track", "Name":"Track", "Type":"Audio", "AlbumId":"album", "Album":"Album",
                "ArtistItems":[{"Id":"artist","Name":"Artist"}],
                "AlbumArtists":[{"Id":"artist","Name":"Artist"}],
                "GenreItems":[{"Id":"genre","Name":"Rock"}],
                "AlbumPrimaryImageTag":"album-cover", "UserData":{"PlayCount":2,"IsFavorite":false}
            });
            let root = tempfile::tempdir().unwrap();
            let database = library::Database::open(root.path().join("library.sqlite"))
                .await
                .unwrap();
            let mut scan = library::Scan::begin(&database, "source", "Server", "server", None)
                .await
                .unwrap();
            stage_album(&mut scan, album_from_item(kind, album.clone()).unwrap())
                .await
                .unwrap();
            stage_track(&mut scan, track_from_item(kind, track.clone()).unwrap())
                .await
                .unwrap();
            super::stage_artist(&mut scan, super::artist_from_item(kind, serde_json::json!({
                "Id":"artist","Name":"Artist","Type":"MusicArtist",
                "ProviderIds":{"MusicBrainzArtist":"artist-mbid"},
                "ImageTags":{"Primary":"artist-cover"},"UserData":{"IsFavorite":true,"Rating":8}
            })).unwrap()).await.unwrap();
            super::stage_genre(
                &mut scan,
                super::genre_from_item(
                    kind,
                    serde_json::json!({
                        "Id":"genre","Name":"Rock","ImageTags":{"Primary":"genre-cover"}
                    }),
                )
                .unwrap(),
            )
            .await
            .unwrap();
            let initial = scan.finish().await.unwrap();
            let library::ScanOutcome::Changed(publication) = initial else {
                panic!("initial catalog");
            };
            let uri = library::source_entity_uri(
                &crate::SourceId::new("source"),
                "track",
                &kind.object_id("track", "track"),
            );
            let cancel = library::ReadCancellation::new();
            let before = database
                .track_row_by_uri(&uri, &cancel)
                .await
                .unwrap()
                .unwrap();
            for favorite in [false, true, true] {
                server.reset().await;
                let mut updated = track.clone();
                updated["UserData"] = serde_json::json!({"PlayCount":3,"IsFavorite":favorite});
                for (endpoint, body) in [
                    (
                        source.item_url("track").unwrap().path().to_string(),
                        updated,
                    ),
                    (
                        source.item_url("album").unwrap().path().to_string(),
                        album.clone(),
                    ),
                    (
                        super::endpoint(&source.base_url, "Items/track/Ancestors")
                            .unwrap()
                            .path()
                            .to_string(),
                        serde_json::json!([]),
                    ),
                ] {
                    Mock::given(method("GET"))
                        .and(path(endpoint))
                        .respond_with(ResponseTemplate::new(200).set_body_json(body))
                        .mount(&server)
                        .await;
                }
                let prior = database
                    .track_row_by_uri(&uri, &cancel)
                    .await
                    .unwrap()
                    .unwrap();
                let outcome = source
                    .apply_live_items(&database, "source", vec!["track".into()], vec![])
                    .await
                    .unwrap();
                if prior.favorite == favorite {
                    assert!(
                        matches!(outcome, library::ScanOutcome::Identical(_)),
                        "{kind:?}: {outcome:?}"
                    );
                } else {
                    assert!(
                        matches!(outcome, library::ScanOutcome::Changed(_)),
                        "favorite must publish"
                    );
                }
                let after = database
                    .track_row_by_uri(&uri, &cancel)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(after.favorite, favorite);
                assert_eq!(after.artwork_binding, before.artwork_binding);
                let artist_key = after.artists[0].artist_key;
                let artist = database
                    .artist_rows(publication.source, &[artist_key], false, None, &cancel)
                    .await
                    .unwrap()
                    .pop()
                    .unwrap();
                assert!(artist.favorite);
                assert_eq!(artist.musicbrainz_artist_id.as_deref(), Some("artist-mbid"));
            }
        }
    }

    #[tokio::test]
    async fn exact_track_change_closes_over_its_album_and_removal_fetches_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items/track-one"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Id": "track-one",
                "Name": "Updated Track",
                "Type": "Audio",
                "AlbumId": "album-one",
                "Album": "Updated Album"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Items/album-one"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Id": "album-one",
                "Name": "Updated Album",
                "Type": "MusicAlbum"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Items/track-one/Ancestors"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
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
        let directory = tempfile::tempdir().expect("Library directory");
        let database = library::Database::open(directory.path().join("library.sqlite3"))
            .await
            .expect("Library database");
        let mut scan =
            library::Scan::begin(&database, "jellyfin:test", "Jellyfin", "jellyfin", None)
                .await
                .expect("initial Scan");
        scan.begin_batch().await.expect("initial batch");
        stage_album(
            &mut scan,
            album_from_item(
                crate::ServerKind::Jellyfin,
                serde_json::from_value(serde_json::json!({
                    "Id": "album-one", "Name": "Old Album", "Type": "MusicAlbum"
                }))
                .expect("old Album"),
            )
            .unwrap(),
        )
        .await
        .expect("stage old Album");
        stage_track(
            &mut scan,
            track_from_item(
                crate::ServerKind::Jellyfin,
                serde_json::from_value(serde_json::json!({
                    "Id": "track-one", "Name": "Old Track", "Type": "Audio",
                    "AlbumId": "album-one", "Album": "Old Album"
                }))
                .expect("old Track"),
            )
            .unwrap(),
        )
        .await
        .expect("stage old Track");
        scan.finish_batch().await.expect("finish initial batch");
        scan.finish().await.expect("publish initial Scan");

        source
            .apply_live_items(
                &database,
                "jellyfin:test",
                vec!["track-one".to_string()],
                Vec::new(),
            )
            .await
            .expect("apply exact Track change");
        let cancellation = library::ReadCancellation::new();
        let source_key = database
            .cached_source("jellyfin:test", &cancellation)
            .await
            .expect("cached source")
            .expect("published source")
            .source;
        let (_, _, albums) = database
            .album_route_page(
                source_key,
                None,
                false,
                "Updated Album",
                library::AlbumSort::Title,
                false,
                library::RouteSeedWindow::top(),
                &cancellation,
            )
            .await
            .expect("updated Album page");
        assert_eq!(albums[0].title, "Updated Album");

        source
            .apply_live_items(
                &database,
                "jellyfin:test",
                Vec::new(),
                vec!["track-one".to_string()],
            )
            .await
            .expect("apply exact removal");
        assert!(
            database
                .track_route_page(
                    source_key,
                    None,
                    false,
                    "",
                    library::TrackSort::Title,
                    false,
                    library::RouteSeedWindow::top(),
                    &cancellation,
                )
                .await
                .expect("Track page")
                .order
                .is_empty()
        );
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("Jellyfin requests")
                .len(),
            3,
            "the removal path must use accepted Store identity without fetching"
        );
    }
}
