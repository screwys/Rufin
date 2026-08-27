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
    redact_error_url: None,
};

impl JellyfinSource {
    pub(crate) async fn collection_track_object_ids(
        &self,
        collection: &crate::SourceCollection,
        limit: usize,
    ) -> SourceResult<Vec<String>> {
        let mut url = endpoint(&self.base_url, "Items")?;
        url.query_pairs_mut()
            .append_pair("UserId", &self.user_id)
            .append_pair("Recursive", "true")
            .append_pair("IncludeItemTypes", "Audio")
            .append_pair("Fields", TRACK_FIELDS)
            .append_pair("SortBy", "ParentIndexNumber,IndexNumber,SortName")
            .append_pair("SortOrder", "Ascending")
            .append_pair("Limit", &limit.clamp(1, 500).to_string());
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
        Ok(self
            .get_json::<ItemQueryResult>(url)
            .await?
            .items
            .into_iter()
            .filter(is_audio_item)
            .map(|item| jellyfin_id("track", &item.id))
            .collect())
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
        let mut items = self.get_json::<ItemQueryResult>(url).await?.items;
        if items.is_empty()
            && matches!(seed, crate::SourceRadioSeed::Track(_))
            && !self.use_instant_mix
        {
            let mut url = endpoint(&self.base_url, &format!("Songs/{raw}/InstantMix"))?;
            url.query_pairs_mut()
                .append_pair("UserId", &self.user_id)
                .append_pair("Limit", &limit.clamp(1, 500).to_string());
            items = self.get_json::<ItemQueryResult>(url).await?.items;
        }
        Ok(items
            .into_iter()
            .filter(is_audio_item)
            .map(|item| jellyfin_id("track", &item.id))
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
        let response = self.get_json::<ItemQueryResult>(url).await?;
        let mut page = crate::LiveFolderPage::default();
        for item in response.items {
            if parent.is_none()
                && !item
                    .collection_type
                    .as_deref()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("music"))
            {
                continue;
            }
            if is_audio_item(&item) {
                page.tracks.push(jellyfin_id("track", &item.id));
            } else if item.name.is_some() {
                page.folders.push(crate::LiveFolder {
                    object_id: jellyfin_id("folder", &item.id),
                    name: item.name.unwrap_or_default(),
                });
            }
        }
        Ok(page)
    }

    pub(crate) async fn live_search(
        &self,
        query: &str,
        limit: usize,
    ) -> SourceResult<crate::LiveSearchResults> {
        if query.trim().is_empty() {
            return Ok(crate::LiveSearchResults::default());
        }
        let limit = limit.clamp(1, 100);
        let (artists, albums, tracks) = tokio::try_join!(
            self.search_people(query, limit),
            self.search_items("MusicAlbum", ALBUM_FIELDS, query, limit),
            self.search_items("Audio", TRACK_FIELDS, query, limit),
        )?;
        Ok(crate::LiveSearchResults {
            artists: artists
                .items
                .into_iter()
                .map(artist_from_item)
                .map(|artist| {
                    Ok(crate::LiveSearchArtist {
                        object_id: artist.id,
                        name: artist.name,
                        artwork_binding: artist
                            .image_ref
                            .as_ref()
                            .map(serde_json::to_vec)
                            .transpose()?,
                    })
                })
                .collect::<SourceResult<_>>()?,
            albums: albums
                .items
                .into_iter()
                .map(album_from_item)
                .map(|album| {
                    Ok(crate::LiveSearchAlbum {
                        object_id: album.id,
                        title: album.title,
                        artist: album.artist,
                        artwork_binding: album
                            .image_ref
                            .as_ref()
                            .map(serde_json::to_vec)
                            .transpose()?,
                    })
                })
                .collect::<SourceResult<_>>()?,
            tracks: tracks
                .items
                .into_iter()
                .filter(is_audio_item)
                .map(track_from_item)
                .map(|track| {
                    Ok(crate::LiveSearchTrack {
                        object_id: track.id,
                        title: track.title,
                        artist: track.artist,
                        album: track.album,
                        artwork_binding: track
                            .image_ref
                            .as_ref()
                            .map(serde_json::to_vec)
                            .transpose()?,
                    })
                })
                .collect::<SourceResult<_>>()?,
        })
    }

    async fn search_items(
        &self,
        item_types: &str,
        fields: &str,
        query: &str,
        limit: usize,
    ) -> SourceResult<ItemQueryResult> {
        let mut url = endpoint(&self.base_url, "Items")?;
        url.query_pairs_mut()
            .append_pair("UserId", &self.user_id)
            .append_pair("Recursive", "true")
            .append_pair("IncludeItemTypes", item_types)
            .append_pair("SearchTerm", query)
            .append_pair("StartIndex", "0")
            .append_pair("Limit", &limit.to_string())
            .append_pair("Fields", fields);
        self.get_json(url).await
    }
    async fn search_people(&self, query: &str, limit: usize) -> SourceResult<ItemQueryResult> {
        let mut url = endpoint(&self.base_url, "Artists")?;
        url.query_pairs_mut().append_pair("UserId",&self.user_id).append_pair("SearchTerm",query).append_pair("StartIndex","0").append_pair("Limit",&limit.to_string()).append_pair("Fields","ParentId,UserData,ItemCounts,ChildCount,AlbumCount,SongCount,ImageTags,ProviderIds");
        self.get_json(url).await
    }
}

impl JellyfinSource {
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
            let response = self.get_json::<ItemQueryResult>(url).await?;
            let count = response.items.len();
            let page_start = pages.offset();
            let finished = pages.advance(count, response.total_record_count)?;
            scan.begin_batch().await?;
            for (offset, item) in response.items.into_iter().enumerate() {
                let (entry_id, track_id, position) = playlist_entry(item, page_start + offset)?;
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

fn playlist_entry(item: JellyfinItem, position: usize) -> SourceResult<(String, String, i64)> {
    let entry_id = item
        .playlist_item_id
        .filter(|id| !id.is_empty())
        .ok_or_else(incomplete_playlist)?;
    Ok((entry_id, jellyfin_id("track", &item.id), position as i64))
}

fn incomplete_playlist() -> SourceError {
    SourceError::Other("Jellyfin returned an incomplete playlist".to_string())
}

impl JellyfinSource {
    pub(crate) async fn resolve_stream(
        &self,
        request: &StreamRequest,
    ) -> SourceResult<ResolvedStream> {
        stream_descriptor(
            &self.base_url,
            &self.user_id,
            &self.device_id,
            &self.access_token,
            self.trust_invalid_cert,
            request,
        )
    }
}

impl JellyfinSource {
    pub(crate) fn resolve_download(
        &self,
        request: &StreamRequest,
    ) -> SourceResult<crate::ResolvedDownload> {
        if request.quality == StreamQuality::Original {
            let stream = stream_descriptor(
                &self.base_url,
                &self.user_id,
                &self.device_id,
                &self.access_token,
                self.trust_invalid_cert,
                request,
            )?;
            return Ok(crate::ResolvedDownload::new(stream, None));
        }

        let StreamQuality::MaxBitrateKbps(kbps) = request.quality else {
            unreachable!("original downloads return before transcoding")
        };
        let raw_track_id = raw_item_id(&request.track_object_id);
        let bitrate = kbps
            .min(super::JELLYFIN_TRANSCODED_DOWNLOAD_BITRATE_LIMIT_KBPS)
            .saturating_mul(1_000)
            .to_string();
        let mut url = endpoint(&self.base_url, &format!("Audio/{raw_track_id}/Universal"))?;
        url.query_pairs_mut()
            .append_pair("UserId", &self.user_id)
            .append_pair("DeviceId", &self.device_id)
            .append_pair("api_key", &self.access_token)
            .append_pair("transcodingContainer", "ogg")
            .append_pair("audioCodec", "opus")
            .append_pair("audioBitRate", &bitrate);
        let mut redacted_url = url.clone();
        redacted_url
            .query_pairs_mut()
            .clear()
            .append_pair("UserId", &self.user_id)
            .append_pair("DeviceId", &self.device_id)
            .append_pair("api_key", "<redacted>")
            .append_pair("transcodingContainer", "ogg")
            .append_pair("audioCodec", "opus")
            .append_pair("audioBitRate", &bitrate);
        let stream = ResolvedStream::with_redacted(url.to_string(), redacted_url.to_string())
            .with_trust_invalid_certificate(self.trust_invalid_cert);
        Ok(crate::ResolvedDownload::new(stream, Some("ogg")))
    }
}

impl JellyfinSource {
    pub(crate) async fn set_favorite(&self, object_id: &str, favorite: bool) -> SourceResult<()> {
        let mut url = endpoint(
            &self.base_url,
            &format!("UserFavoriteItems/{}", raw_item_id(object_id)),
        )?;
        url.query_pairs_mut().append_pair("userId", &self.user_id);
        if favorite {
            self.send_unit(self.client.post(url)).await
        } else {
            self.send_unit(self.client.delete(url)).await
        }
    }
    pub(crate) async fn set_rating(&self, object_id: &str, rating: Option<u8>) -> SourceResult<()> {
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
        let image_kind = if image_ref.item_id.starts_with("jellyfin:backdrop:") {
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
        send_bytes(
            self.client
                .get(url)
                .header(header::AUTHORIZATION, self.authorization.clone()),
        )
        .await
    }
}

impl JellyfinSource {
    pub(crate) async fn create_playlist(
        &self,
        name: &str,
        track_ids: &[String],
    ) -> SourceResult<PlaylistId> {
        let url = endpoint(&self.base_url, "Playlists")?;
        let body = CreatePlaylistDto {
            name: name.to_string(),
            ids: raw_track_ids(track_ids),
            user_id: Some(self.user_id.clone()),
            media_type: Some("Audio".to_string()),
            is_public: false,
        };
        let result = self
            .send_json::<PlaylistCreationResult>(self.client.post(url).json(&body))
            .await?;
        Ok(String::from(jellyfin_id("playlist", &result.id)))
    }
    pub(crate) async fn rename_playlist(&self, playlist_id: &str, name: &str) -> SourceResult<()> {
        let url = endpoint(
            &self.base_url,
            &format!("Playlists/{}", raw_item_id(playlist_id)),
        )?;
        let body = UpdatePlaylistDto {
            name: Some(name.to_string()),
        };
        self.send_unit(self.client.post(url).json(&body)).await
    }
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

impl JellyfinSource {
    pub(crate) async fn lyrics(
        &self,
        track_id: &str,
        search: LyricsSearch,
    ) -> SourceResult<Option<NativeLyrics>> {
        match search {
            LyricsSearch::ServerOnly => self.server_lyrics(track_id).await,
            LyricsSearch::ServerThenRemote => {
                if let Some(lyrics) = self.server_lyrics(track_id).await? {
                    return Ok(Some(lyrics));
                }
                self.remote_lyrics(track_id).await
            }
            LyricsSearch::RemoteThenServer => {
                if let Some(lyrics) = self.remote_lyrics(track_id).await? {
                    return Ok(Some(lyrics));
                }
                self.server_lyrics(track_id).await
            }
        }
    }
}

impl JellyfinSource {
    pub(crate) async fn report_playback(&self, report: &SourceReportFact) -> SourceResult<()> {
        let path = match report.phase {
            SourceReportPhase::Started => "Sessions/Playing",
            SourceReportPhase::Progress => "Sessions/Playing/Progress",
            SourceReportPhase::QualifiedPlay => return Ok(()),
            SourceReportPhase::Ended => "Sessions/Playing/Stopped",
        };
        let url = endpoint(&self.base_url, path)?;
        let body = PlaybackReportDto::from_report(report);
        self.send_unit(self.client.post(url).json(&body)).await
    }
}

pub(super) async fn public_server_name(
    client: &Client,
    base_url: &Url,
    config: &JellyfinClientConfig,
) -> Option<String> {
    let url = endpoint(base_url, "System/Info/Public").ok()?;
    let response = send_json::<PublicSystemInfo>(
        client
            .get(url)
            .header(header::AUTHORIZATION, auth_header(config, None)),
    )
    .await
    .ok()?;
    response.server_name.or(response.local_address)
}
pub(super) async fn send_json<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> SourceResult<T> {
    remote_http::json(
        request,
        JELLYFIN_HTTP,
        BodyLimit {
            max_bytes: JELLYFIN_JSON_MAX_BYTES,
            context: "Jellyfin JSON response",
        },
    )
    .await
}
pub(super) async fn send_unit(request: reqwest::RequestBuilder) -> SourceResult<()> {
    remote_http::unit(request, JELLYFIN_HTTP).await
}
pub(super) async fn send_bytes(request: reqwest::RequestBuilder) -> SourceResult<ImageBytes> {
    remote_http::bytes(
        request,
        JELLYFIN_HTTP,
        BodyLimit {
            max_bytes: JELLYFIN_IMAGE_MAX_BYTES,
            context: "Jellyfin image response",
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

pub(super) fn stream_descriptor(
    base_url: &Url,
    user_id: &str,
    device_id: &str,
    access_token: &str,
    trust_invalid_certificate: bool,
    request: &StreamRequest,
) -> SourceResult<ResolvedStream> {
    let raw_track_id = raw_item_id(&request.track_object_id);
    let max_bitrate = request
        .quality
        .max_bitrate_kbps()
        .map(|kbps| kbps.saturating_mul(1_000).to_string());

    let mut url = endpoint(base_url, &format!("Audio/{raw_track_id}/stream"))?;
    let static_stream = if max_bitrate.is_some() {
        "false"
    } else {
        "true"
    };
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("UserId", user_id)
            .append_pair("DeviceId", device_id)
            .append_pair("Static", static_stream)
            .append_pair("api_key", access_token);
        if let Some(max_bitrate) = &max_bitrate {
            query
                .append_pair("MaxStreamingBitrate", max_bitrate)
                .append_pair("TranscodingContainer", "mp3")
                .append_pair("AudioCodec", "mp3");
        }
    }
    let mut redacted_url = url.clone();
    {
        let mut redacted_query = redacted_url.query_pairs_mut();
        redacted_query
            .clear()
            .append_pair("UserId", user_id)
            .append_pair("DeviceId", device_id)
            .append_pair("Static", static_stream)
            .append_pair("api_key", "<redacted>");
        if let Some(max_bitrate) = &max_bitrate {
            redacted_query
                .append_pair("MaxStreamingBitrate", max_bitrate)
                .append_pair("TranscodingContainer", "mp3")
                .append_pair("AudioCodec", "mp3");
        }
    }
    Ok(
        ResolvedStream::with_redacted(url.to_string(), redacted_url.to_string())
            .with_trust_invalid_certificate(trust_invalid_certificate),
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
pub(super) fn auth_header(config: &JellyfinClientConfig, token: Option<&str>) -> String {
    let mut value = format!(
        "MediaBrowser Client=\"{}\", Device=\"{}\", DeviceId=\"{}\", Version=\"{}\"",
        config.client_name, config.device_name, config.device_id, config.client_version
    );
    if let Some(token) = token {
        value.push_str(&format!(", Token=\"{token}\""));
    }
    value
}
pub(crate) fn jellyfin_id(kind: &str, id: &str) -> String {
    format!("jellyfin:{kind}:{id}")
}
pub(super) fn raw_track_ids(track_ids: &[String]) -> Vec<String> {
    track_ids
        .iter()
        .map(|id| raw_item_id(id.as_str()).to_string())
        .collect()
}
pub(super) fn stable_source_id(input: &str) -> String {
    format!("{:016x}", stable_hash(input))
}
pub(super) fn ticks_to_millis(ticks: Option<i64>) -> Option<u64> {
    ticks.map(|value| (value.max(0) / 10_000) as u64)
}

impl JellyfinSource {
    pub(super) async fn item_page(
        &self,
        include_types: &str,
        offset: usize,
        limit: usize,
    ) -> SourceResult<ItemQueryResult> {
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
    ) -> SourceResult<ItemQueryResult> {
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

        self.get_json::<ItemQueryResult>(url).await
    }

    pub(super) async fn people_page(
        &self,
        path: &str,
        offset: usize,
        limit: usize,
    ) -> SourceResult<ItemQueryResult> {
        let mut url = endpoint(&self.base_url, path)?;
        url.query_pairs_mut()
            .append_pair("UserId", &self.user_id)
            .append_pair("StartIndex", &offset.to_string())
            .append_pair("Limit", &limit.to_string())
            .append_pair(
                "Fields",
                "ParentId,UserData,ItemCounts,ChildCount,AlbumCount,SongCount,ImageTags,ProviderIds",
            );

        self.get_json::<ItemQueryResult>(url).await
    }

    pub(super) async fn music_genre_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> SourceResult<ItemQueryResult> {
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

        self.get_json::<ItemQueryResult>(url).await
    }

    pub(super) async fn get_json<T: DeserializeOwned>(&self, url: Url) -> SourceResult<T> {
        send_json(
            self.client
                .get(url)
                .header(header::AUTHORIZATION, self.authorization.clone()),
        )
        .await
    }

    pub(super) async fn send_json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> SourceResult<T> {
        send_json(request.header(header::AUTHORIZATION, self.authorization.clone())).await
    }

    pub(super) async fn send_unit(&self, request: reqwest::RequestBuilder) -> SourceResult<()> {
        send_unit(request.header(header::AUTHORIZATION, self.authorization.clone())).await
    }

    async fn server_lyrics(&self, track_id: &str) -> SourceResult<Option<NativeLyrics>> {
        let raw_track_id = raw_item_id(track_id);
        let local_url = endpoint(&self.base_url, &format!("Audio/{raw_track_id}/Lyrics"))?;
        match self.send_json::<LyricDto>(self.client.get(local_url)).await {
            Ok(dto) => Ok(Some(lyrics_from_dto(dto))),
            Err(SourceError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn remote_lyrics(&self, track_id: &str) -> SourceResult<Option<NativeLyrics>> {
        let raw_track_id = raw_item_id(track_id);
        let remote_url = endpoint(
            &self.base_url,
            &format!("Audio/{raw_track_id}/RemoteSearch/Lyrics"),
        )?;
        let results = match self
            .send_json::<Vec<RemoteLyricInfoDto>>(self.client.get(remote_url))
            .await
        {
            Ok(results) => results,
            Err(SourceError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let Some(first) = results.into_iter().find(|result| !result.id.is_empty()) else {
            return Ok(None);
        };
        let lyric_url = endpoint(&self.base_url, &format!("Providers/Lyrics/{}", first.id))?;
        match self.send_json::<LyricDto>(self.client.get(lyric_url)).await {
            Ok(dto) => Ok(Some(lyrics_from_dto(dto))),
            Err(SourceError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

pub(super) fn lyrics_from_dto(dto: LyricDto) -> NativeLyrics {
    NativeLyrics {
        documents: vec![NativeLyricsDocument {
            role: NativeLyricsRole::Original,
            language: None,
            offset_millis: 0,
            lines: dto
                .lyrics
                .unwrap_or_default()
                .into_iter()
                .filter_map(|line| {
                    let text = line.text.unwrap_or_default();
                    (!text.trim().is_empty()).then_some(NativeLyricLine {
                        text,
                        start_millis: ticks_to_millis(line.start),
                        end_millis: None,
                        cue_lines: Vec::new(),
                    })
                })
                .collect(),
            agents: Vec::new(),
        }],
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct PublicSystemInfo {
    pub(super) server_name: Option<String>,
    pub(super) local_address: Option<String>,
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
pub(super) struct AuthenticationResult {
    pub(super) access_token: String,
    pub(super) server_id: Option<String>,
    pub(super) user: JellyfinUser,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct JellyfinUser {
    pub(super) id: String,
    pub(super) name: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct CreatePlaylistDto {
    pub(super) name: String,
    pub(super) ids: Vec<String>,
    pub(super) user_id: Option<String>,
    pub(super) media_type: Option<String>,
    pub(super) is_public: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct UpdatePlaylistDto {
    pub(super) name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct PlaylistCreationResult {
    pub(super) id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct LyricDto {
    pub(super) lyrics: Option<Vec<LyricLineDto>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct LyricLineDto {
    pub(super) text: Option<String>,
    pub(super) start: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct RemoteLyricInfoDto {
    pub(super) id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct PlaybackReportDto {
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
    pub(super) fn from_report(report: &SourceReportFact) -> Self {
        let position_seconds = (report.position_millis / 1_000).min(u64::from(u32::MAX)) as u32;
        Self {
            can_seek: true,
            item_id: raw_item_id(&report.track_object_id).to_string(),
            is_paused: report.paused,
            is_muted: report.muted,
            position_ticks: i64::from(position_seconds) * 10_000_000,
            volume_level: (report.volume.clamp(0.0, 1.0) * 100.0).round() as i32,
            play_method: "DirectPlay",
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
    use crate::jellyfin::item::{
        JellyfinItem, album_from_item, stage_album, stage_track, track_from_item,
    };
    use crate::jellyfin::{JellyfinSource, JellyfinSourceConfig};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn playlist_entries_keep_provider_occurrences_when_tracks_repeat() {
        let items: Vec<JellyfinItem> = serde_json::from_value(serde_json::json!([{
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
            .map(|(position, item)| playlist_entry(item, position).expect("complete entry"))
            .collect::<Vec<_>>();

        assert_eq!(entries[0].0, "entry-one");
        assert_eq!(entries[1].0, "entry-two");
        assert_eq!(entries[0].1, entries[1].1);
        assert_eq!((entries[0].2, entries[1].2), (0, 1));
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
            let item: JellyfinItem = serde_json::from_value(item).expect("playlist item");
            assert!(playlist_entry(item, 0).is_err());
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
            &playback::StreamRequest::original("jellyfin:track:track-one"),
        )
        .expect("stream descriptor");

        assert!(stream.uri().contains("secret-token"));
        assert!(!stream.redacted_uri().contains("secret-token"));
        assert!(stream.redacted_uri().contains("api_key=%3Credacted%3E"));
    }

    #[tokio::test]
    async fn empty_similar_tracks_fall_back_to_jellyfin_instant_mix() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items/track-one/Similar"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [], "TotalRecordCount": 0
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Songs/track-one/InstantMix"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [{"Id": "track-two", "Type": "Audio"}],
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

        assert_eq!(
            source
                .generated_track_object_ids(
                    &crate::SourceRadioSeed::Track("jellyfin:track:track-one".to_string()),
                    20,
                )
                .await
                .expect("Jellyfin recommendations"),
            ["jellyfin:track:track-two"]
        );
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
        let mut scan =
            library::Scan::begin(&database, "jellyfin:test", "Jellyfin", "jellyfin", None)
                .await
                .expect("initial Scan");
        scan.begin_batch().await.expect("initial batch");
        stage_album(
            &mut scan,
            album_from_item(
                serde_json::from_value(serde_json::json!({
                    "Id": "album-one", "Name": "Old Album", "Type": "MusicAlbum"
                }))
                .expect("old Album"),
            ),
        )
        .await
        .expect("stage old Album");
        stage_track(
            &mut scan,
            track_from_item(
                serde_json::from_value(serde_json::json!({
                    "Id": "track-one", "Name": "Old Track", "Type": "Audio",
                    "AlbumId": "album-one", "Album": "Old Album"
                }))
                .expect("old Track"),
            ),
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
        let (_, albums) = database
            .album_route_page(
                source_key,
                None,
                false,
                "Updated Album",
                library::AlbumSort::Title,
                false,
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
