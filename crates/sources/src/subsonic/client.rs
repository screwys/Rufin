use super::*;

use crate::remote_http::{self, BodyLimit, RemoteHttpPolicy, RemoteTimeouts};
use crate::remote_json as json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SUBSONIC_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SUBSONIC_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
pub(super) const SUBSONIC_JSON_MAX_BYTES: usize = 64 * 1024 * 1024;
pub(super) const SUBSONIC_IMAGE_MAX_BYTES: usize = 32 * 1024 * 1024;
const SUBSONIC_ERROR_BODY_MAX_BYTES: usize = 64 * 1024;
const SUBSONIC_HTTP: RemoteHttpPolicy = RemoteHttpPolicy {
    service: "opensubsonic",
    auth_context: "Subsonic server returned",
    error_body: BodyLimit {
        max_bytes: SUBSONIC_ERROR_BODY_MAX_BYTES,
        context: "Subsonic error response",
    },
    redact_error_url: Some(redact_subsonic_query),
};

impl SubsonicSource {
    pub(crate) async fn generated_track_object_ids(
        &self,
        seed: &crate::SourceRadioSeed,
        limit: usize,
    ) -> SourceResult<Vec<String>> {
        let tracks = match seed {
            crate::SourceRadioSeed::Track(id)
            | crate::SourceRadioSeed::Album(id)
            | crate::SourceRadioSeed::Artist(id) => {
                self.similar_songs(raw_item_id(id), limit).await?
            }
            crate::SourceRadioSeed::Playlist(id) => {
                let playlist = self.read_playlist(id).await?;
                let first = playlist.entries.first().ok_or(SourceError::NotFound)?;
                self.similar_songs(raw_item_id(&first.track_id), limit)
                    .await?
            }
            crate::SourceRadioSeed::Genre(name) => {
                let body: Value = self
                    .get_json(
                        "getRandomSongs",
                        &[
                            ("size", limit.clamp(1, 500).to_string()),
                            ("genre", name.clone()),
                        ],
                    )
                    .await?;
                json::items(&body["randomSongs"]["song"])
                    .iter()
                    .filter_map(|song| track_from_json(self, song))
                    .collect()
            }
        };
        Ok(tracks.into_iter().map(|track| track.id).collect())
    }

    pub(crate) async fn browse_folder(
        &self,
        folder_object_id: Option<&str>,
        music_folder_object_id: Option<&str>,
    ) -> SourceResult<crate::LiveFolderPage> {
        let mut page = crate::LiveFolderPage::default();
        if let Some(folder) = folder_object_id {
            let body: Value = self
                .get_json(
                    "getMusicDirectory",
                    &[("id", raw_item_id(folder).to_string())],
                )
                .await?;
            for child in json::items(&body["directory"]["child"]) {
                let Some(id) = json::id(&child["id"]) else {
                    continue;
                };
                if json::boolean(&child["isDir"]).unwrap_or(false) {
                    page.folders.push(crate::LiveFolder {
                        object_id: self.id("folder", &id),
                        name: json::field(child, "title")
                            .unwrap_or_else(|| "Untitled Folder".to_string()),
                    });
                } else {
                    page.tracks.push(self.id("track", &id));
                }
            }
        } else {
            let parameters = music_folder_object_id
                .map(|folder| vec![("musicFolderId", raw_item_id(folder).to_string())])
                .unwrap_or_default();
            let body: Value = self.get_json("getIndexes", &parameters).await?;
            for artist in json::items(&body["indexes"]["index"])
                .iter()
                .flat_map(|index| json::items(&index["artist"]))
            {
                let Some(id) = json::id(&artist["id"]) else {
                    continue;
                };
                page.folders.push(crate::LiveFolder {
                    object_id: self.id("folder", &id),
                    name: json::field(artist, "name")
                        .unwrap_or_else(|| "Untitled Folder".to_string()),
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
        let count = limit.clamp(1, 100).to_string();
        let body: serde_json::Value = self
            .get_json(
                "search3",
                &[
                    ("query", query.to_string()),
                    ("artistCount", count.clone()),
                    ("artistOffset", "0".to_string()),
                    ("albumCount", count.clone()),
                    ("albumOffset", "0".to_string()),
                    ("songCount", count),
                    ("songOffset", "0".to_string()),
                ],
            )
            .await?;
        let results = &body["searchResult3"];
        let mut scan = library::Scan::begin_items(database, source_id.as_str()).await?;
        let source = scan
            .existing_source()
            .ok_or(SourceError::InvalidRequest("Search source is unavailable"))?;
        scan.begin_batch().await?;
        let mut artist_ids = Vec::new();
        let mut album_ids = Vec::new();
        let mut track_ids = Vec::new();
        for artist in json::items(&results["artist"])
            .iter()
            .filter_map(|value| artist_from_json(self, value))
            .take(limit.clamp(1, 100))
        {
            artist_ids.push(artist.id.clone());
            stage_artist(&mut scan, artist).await?;
        }
        for album in json::items(&results["album"])
            .iter()
            .filter_map(|value| album_from_json(self, value))
            .take(limit.clamp(1, 100))
        {
            album_ids.push(album.id.clone());
            stage_album(&mut scan, album).await?;
        }
        for track in json::items(&results["song"])
            .iter()
            .filter_map(|value| track_from_json(self, value))
            .take(limit.clamp(1, 100))
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
}

impl SubsonicSource {
    pub(crate) async fn stage_collection(
        &self,
        scan: &mut library::Scan,
        collection: &crate::SourceCollection,
    ) -> SourceResult<()> {
        let album_ids = match collection {
            crate::SourceCollection::Album(id) => vec![raw_item_id(id).to_string()],
            crate::SourceCollection::Artist(id) => {
                let body: Value = self
                    .get_json("getArtist", &[("id", raw_item_id(id).to_string())])
                    .await?;
                let mut ids = Vec::new();
                for page in json::items(&body["artist"]["album"]).chunks(100) {
                    scan.begin_batch().await?;
                    for album in page {
                        if let Some(album) = album_from_json(self, album) {
                            ids.push(raw_item_id(&album.id).to_string());
                            stage_album(scan, album).await?;
                        }
                    }
                    scan.finish_batch().await?;
                }
                ids
            }
        };
        for album_id in album_ids {
            let body: serde_json::Value = self.get_json("getAlbum", &[("id", album_id)]).await?;
            let album = &body["album"];
            if let Some(metadata) = album_from_json(self, album) {
                scan.begin_batch().await?;
                stage_album(scan, metadata).await?;
                scan.finish_batch().await?;
            }
            for page in json::items(&album["song"]).chunks(100) {
                scan.begin_batch().await?;
                for song in page.iter().filter_map(|song| track_from_json(self, song)) {
                    stage_track(scan, song).await?;
                }
                scan.finish_batch().await?;
            }
        }
        Ok(())
    }

    pub(super) async fn read_track(&self, track_id: &str) -> SourceResult<Track> {
        let body: Value = self
            .get_json("getSong", &[("id", raw_item_id(track_id).to_string())])
            .await?;
        track_from_json(self, &body["song"]).ok_or(SourceError::NotFound)
    }
}

impl SubsonicSource {
    pub(super) async fn read_playlist(&self, playlist_id: &str) -> SourceResult<PlaylistSnapshot> {
        let body: Value = self
            .get_json(
                "getPlaylist",
                &[("id", raw_item_id(playlist_id).to_string())],
            )
            .await?;
        let playlist = playlist_from_json(self, &body["playlist"]).ok_or(SourceError::NotFound)?;
        let entries = json::items(&body["playlist"]["entry"])
            .iter()
            .enumerate()
            .filter_map(|(index, song)| {
                let raw_track_id = json::id(&song["id"])?;
                Some(PlaylistEntry {
                    occurrence_id: playlist_entry_id(&playlist.id, index, &raw_track_id),
                    track_id: self.id("track", &raw_track_id),
                })
            })
            .collect::<Vec<_>>();
        Ok(PlaylistSnapshot { playlist, entries })
    }
}

impl SubsonicSource {
    pub(crate) async fn resolve_stream(
        &self,
        track_object_id: &str,
        quality: StreamQuality,
    ) -> SourceResult<ResolvedStream> {
        let format = if quality.max_bitrate_kbps().is_some() {
            "mp3"
        } else {
            "raw"
        };
        self.resolve_audio(track_object_id, quality.max_bitrate_kbps(), format)
    }

    pub(crate) fn resolve_download(
        &self,
        track_object_id: &str,
        quality: StreamQuality,
    ) -> SourceResult<crate::ResolvedDownload> {
        let (format, extension) = match quality {
            StreamQuality::Original => ("raw", None),
            StreamQuality::MaxBitrateKbps(_) if self.flavor == SubsonicFlavor::Navidrome => {
                ("opus", Some("opus"))
            }
            StreamQuality::MaxBitrateKbps(_) => ("mp3", Some("mp3")),
        };
        let stream = self.resolve_audio(track_object_id, quality.max_bitrate_kbps(), format)?;
        Ok(crate::ResolvedDownload::new(stream, extension))
    }

    fn resolve_audio(
        &self,
        track_id: &str,
        max_bitrate_kbps: Option<u32>,
        format: &str,
    ) -> SourceResult<ResolvedStream> {
        let mut extra = vec![("id", raw_item_id(track_id).to_string())];
        if let Some(kbps) = max_bitrate_kbps {
            extra.push(("maxBitRate", kbps.to_string()));
        }
        extra.push(("format", format.to_string()));
        let url = self.authenticated_url("stream", &extra)?;
        let redacted = redacted_subsonic_url(&url);
        Ok(ResolvedStream::with_redacted(url.to_string(), redacted)
            .with_content_type(match format {
                "mp3" => Some("audio/mpeg".to_string()),
                "opus" => Some("audio/ogg".to_string()),
                _ => None,
            })
            .with_trust_invalid_certificate(self.trust_invalid_cert))
    }
}

impl SubsonicSource {
    pub(crate) async fn set_favorite(
        &self,
        kind: crate::SourceEntityKind,
        object_id: &str,
        favorite: bool,
    ) -> SourceResult<()> {
        let method = if favorite { "star" } else { "unstar" };
        let key = match kind {
            crate::SourceEntityKind::Track => "id",
            crate::SourceEntityKind::Album => "albumId",
            crate::SourceEntityKind::Artist => "artistId",
        };
        self.get_unit(method, &[(key, raw_item_id(object_id).to_string())])
            .await
    }
    pub(crate) async fn set_rating(&self, object_id: &str, rating: Option<u8>) -> SourceResult<()> {
        let whole = rating.unwrap_or(0).div_ceil(2).min(5);
        self.get_unit(
            "setRating",
            &[
                ("id", raw_item_id(object_id).to_string()),
                ("rating", whole.to_string()),
            ],
        )
        .await
    }

    pub(crate) async fn image_bytes(
        &self,
        image_ref: &ImageRef,
        size: u32,
    ) -> SourceResult<ImageBytes> {
        let mut extra = vec![("id", raw_item_id(&image_ref.item_id).to_string())];
        if size > 0 {
            extra.push(("size", size.to_string()));
        }
        let url = self.authenticated_url("getCoverArt", &extra)?;
        subsonic_bytes(self.client.get(url)).await
    }
}

impl SubsonicSource {
    pub(crate) async fn create_playlist(
        &self,
        name: &str,
        track_ids: &[String],
    ) -> SourceResult<PlaylistId> {
        let mut extra = vec![("name", name.trim().to_string())];
        extra.extend(
            track_ids
                .iter()
                .map(|track_id| ("songId", raw_item_id(track_id).to_string())),
        );
        let body: Value = self.get_json("createPlaylist", &extra).await?;
        json::id(&body["playlist"]["id"])
            .map(|id| self.id("playlist", &id))
            .ok_or(SourceError::NotFound)
    }
    pub(crate) async fn rename_playlist(&self, playlist_id: &str, name: &str) -> SourceResult<()> {
        self.get_unit(
            "updatePlaylist",
            &[
                ("playlistId", raw_item_id(playlist_id).to_string()),
                ("name", name.trim().to_string()),
            ],
        )
        .await
    }
    pub(crate) async fn delete_playlist(&self, playlist_id: &str) -> SourceResult<()> {
        self.get_unit(
            "deletePlaylist",
            &[("id", raw_item_id(playlist_id).to_string())],
        )
        .await
    }
    pub(crate) async fn add_playlist_tracks(
        &self,
        playlist_id: &str,
        track_ids: &[String],
    ) -> SourceResult<()> {
        let mut extra = vec![("playlistId", raw_item_id(playlist_id).to_string())];
        extra.extend(
            track_ids
                .iter()
                .map(|track_id| ("songIdToAdd", raw_item_id(track_id).to_string())),
        );
        self.get_unit("updatePlaylist", &extra).await
    }
    pub(crate) async fn remove_playlist_entries(
        &self,
        playlist_id: &str,
        entry_ids: &[String],
    ) -> SourceResult<()> {
        let prefix = format!("{}:", playlist_id);
        let mut extra = vec![("playlistId", raw_item_id(playlist_id).to_string())];
        for entry_id in entry_ids {
            let index = entry_id
                .strip_prefix(&prefix)
                .and_then(|value| value.split_once(':'))
                .and_then(|(index, _)| index.parse::<usize>().ok())
                .ok_or(SourceError::InvalidRequest(
                    "playlist entry does not belong to this playlist",
                ))?;
            extra.push(("songIndexToRemove", index.to_string()));
        }
        self.get_unit("updatePlaylist", &extra).await
    }
    pub(crate) async fn move_playlist_entry(
        &self,
        playlist_id: &str,
        entry_id: &str,
        new_index: usize,
    ) -> SourceResult<()> {
        let mut entries = self.read_playlist(playlist_id).await?.entries;
        if let Some(old_index) = entries
            .iter()
            .position(|entry| entry.occurrence_id == entry_id)
        {
            let entry = entries.remove(old_index);
            entries.insert(new_index.min(entries.len()), entry);
        }
        let ids = entries
            .into_iter()
            .map(|entry| entry.track_id)
            .collect::<Vec<_>>();
        self.replace_playlist_tracks(playlist_id, &ids).await
    }
}

impl SubsonicSource {
    pub(crate) async fn lyrics(&self, track_id: &str) -> SourceResult<Option<LyricsBundle>> {
        let extensions: Value = self
            .get_json("getOpenSubsonicExtensions", &[])
            .await
            .unwrap_or_default();
        let song_lyrics_version = json::items(&extensions["openSubsonicExtensions"])
            .iter()
            .find(|extension| extension["name"].as_str() == Some("songLyrics"))
            .and_then(|extension| {
                json::items(&extension["versions"])
                    .iter()
                    .filter_map(Value::as_u64)
                    .max()
            })
            .unwrap_or_default();
        if song_lyrics_version >= 1 {
            let mut extra = vec![("id", raw_item_id(track_id).to_string())];
            if song_lyrics_version >= 2 {
                extra.push(("enhanced", "true".to_string()));
            }
            let body: Value = self.get_json("getLyricsBySongId", &extra).await?;
            let lyrics =
                native_lyrics_from_structured(json::items(&body["lyricsList"]["structuredLyrics"]));
            return Ok((!lyrics.documents().is_empty()).then_some(lyrics));
        }

        let track = self.read_track(track_id).await?;
        let body: Value = self
            .get_json(
                "getLyrics",
                &[
                    ("artist", track.artist.clone()),
                    ("title", track.title.clone()),
                ],
            )
            .await?;
        let Some(value) = json::field::<String>(&body["lyrics"], "value")
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(None);
        };
        Ok(Some(LyricsBundle::from_documents(
            LyricsOrigin::Native,
            vec![LyricsDocument {
                role: LyricsRole::Original,
                language: None,
                offset_millis: 0,
                lines: value
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| LyricsLine {
                        text: line.trim().to_string(),
                        start_millis: None,
                        end_millis: None,
                        cue_lines: Vec::new(),
                    })
                    .collect(),
                agents: Vec::new(),
            }],
        )))
    }
}

pub(super) fn native_lyrics_from_structured(entries: &[Value]) -> LyricsBundle {
    let documents = entries
        .iter()
        .filter_map(|entry| {
            let role = match entry["kind"].as_str().unwrap_or("main") {
                "main" => LyricsRole::Original,
                "translation" => LyricsRole::Translation,
                "pronunciation" => LyricsRole::Pronunciation,
                _ => return None,
            };
            let agents = json::items(&entry["agents"])
                .iter()
                .filter_map(|agent| {
                    let role = match agent["role"].as_str()? {
                        "main" => LyricsAgentRole::Main,
                        "voice" => LyricsAgentRole::Voice,
                        "bg" => LyricsAgentRole::Background,
                        "group" => LyricsAgentRole::Group,
                        _ => return None,
                    };
                    Some(LyricsAgent {
                        id: json::id(&agent["id"])?,
                        role,
                        name: json::field(agent, "name"),
                    })
                })
                .collect::<Vec<_>>();
            let raw_lines = json::items(&entry["line"]);
            let mut cue_lines_by_index = vec![Vec::new(); raw_lines.len()];
            for cue_line in json::items(&entry["cueLine"]) {
                let Some(index) = json::field::<usize>(cue_line, "index") else {
                    continue;
                };
                let Some(lines) = cue_lines_by_index.get_mut(index) else {
                    continue;
                };
                let Some(value) = cue_line["value"].as_str() else {
                    continue;
                };
                let cues = json::items(&cue_line["cue"])
                    .iter()
                    .filter_map(|cue| {
                        let byte_start = json::field::<usize>(cue, "byteStart")?;
                        let byte_end = json::field::<usize>(cue, "byteEnd")?;
                        let byte_end_exclusive = byte_end.checked_add(1)?;
                        if byte_start > byte_end
                            || byte_end_exclusive > value.len()
                            || !value.is_char_boundary(byte_start)
                            || !value.is_char_boundary(byte_end_exclusive)
                        {
                            return None;
                        }
                        Some(LyricsCue {
                            text: json::field(cue, "value")?,
                            start_millis: json::field(cue, "start")?,
                            end_millis: json::field(cue, "end"),
                            byte_start,
                            byte_end_exclusive,
                        })
                    })
                    .collect();
                lines.push(LyricsCueLine {
                    text: value.to_string(),
                    start_millis: json::field(cue_line, "start"),
                    end_millis: json::field(cue_line, "end"),
                    agent_id: json::id(&cue_line["agentId"]),
                    cues,
                });
            }
            let lines = raw_lines
                .iter()
                .zip(cue_lines_by_index)
                .filter_map(|(line, cue_lines)| {
                    let text = json::field::<String>(line, "value")
                        .filter(|value| !value.trim().is_empty())?;
                    Some(LyricsLine {
                        text,
                        start_millis: json::field(line, "start"),
                        end_millis: cue_lines.iter().filter_map(|line| line.end_millis).max(),
                        cue_lines,
                    })
                })
                .collect::<Vec<_>>();
            (!lines.is_empty()).then(|| {
                let mut document = LyricsDocument {
                    role,
                    language: json::field(entry, "lang"),
                    offset_millis: json::field(entry, "offset").unwrap_or_default(),
                    lines,
                    agents,
                };
                document.normalize_timing_and_language();
                document
            })
        })
        .collect();
    LyricsBundle::from_documents(LyricsOrigin::Native, documents)
}

impl SubsonicSource {
    pub(crate) async fn report_playback(
        &self,
        track_object_id: &str,
        report: &SourceReportFact,
    ) -> SourceResult<()> {
        match report.phase {
            SourceReportPhase::Started => {
                self.get_unit(
                    "scrobble",
                    &[
                        ("id", raw_item_id(track_object_id).to_string()),
                        ("submission", "false".to_string()),
                    ],
                )
                .await
            }
            SourceReportPhase::QualifiedPlay => {
                let started_at_millis = u64::try_from(report.started_at_unix_seconds)
                    .ok()
                    .and_then(|seconds| seconds.checked_mul(1_000))
                    .ok_or(SourceError::InvalidRequest(
                        "playback start time is outside the OpenSubsonic range",
                    ))?;
                self.get_unit(
                    "scrobble",
                    &[
                        ("id", raw_item_id(track_object_id).to_string()),
                        ("submission", "true".to_string()),
                        ("time", started_at_millis.to_string()),
                    ],
                )
                .await
            }
            SourceReportPhase::Progress | SourceReportPhase::Ended => Ok(()),
        }
    }
}
#[derive(Clone)]
pub(super) enum SubsonicCredential {
    Token {
        salt: String,
        token: String,
        navidrome_password: Option<String>,
    },
    ApiKey(String),
    LegacyPassword(String),
}

#[derive(Deserialize, Serialize)]
struct StoredSubsonicCredential {
    version: u32,
    salt: String,
    token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    navidrome_password: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct StoredApiKeyCredential {
    version: u32,
    api_key: String,
}

#[derive(Deserialize, Serialize)]
struct StoredLegacyCredential {
    version: u32,
    password: String,
}

#[derive(Deserialize)]
struct StoredCredentialVersion {
    version: u32,
}

impl fmt::Debug for SubsonicCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Token {
                navidrome_password, ..
            } => formatter
                .debug_struct("SubsonicCredential::Token")
                .field("salt", &"<redacted>")
                .field("token", &"<redacted>")
                .field(
                    "navidrome_password",
                    &navidrome_password.as_ref().map(|_| "<redacted>"),
                )
                .finish(),
            Self::ApiKey(_) => formatter
                .debug_tuple("SubsonicCredential::ApiKey")
                .field(&"<redacted>")
                .finish(),
            Self::LegacyPassword(_) => formatter
                .debug_tuple("SubsonicCredential::LegacyPassword")
                .field(&"<redacted>")
                .finish(),
        }
    }
}
impl SubsonicCredential {
    pub(super) fn from_password(password: &str) -> Self {
        let salt = random_salt();
        let token = format!("{:x}", md5::compute(format!("{password}{salt}")));
        Self::Token {
            salt,
            token,
            navidrome_password: None,
        }
    }

    pub(super) fn from_navidrome_password(password: &str) -> Self {
        let Self::Token { salt, token, .. } = Self::from_password(password) else {
            unreachable!("a password creates a token credential")
        };
        Self::Token {
            salt,
            token,
            navidrome_password: Some(password.to_string()),
        }
    }

    pub(super) fn from_api_key(api_key: &str) -> SourceResult<Self> {
        if api_key.is_empty() {
            return Err(SourceError::Auth(
                "the OpenSubsonic API key is missing".to_string(),
            ));
        }
        Ok(Self::ApiKey(api_key.to_string()))
    }

    pub(super) fn parse(raw: &str) -> SourceResult<Self> {
        if raw.trim_start().starts_with('{') {
            let version = serde_json::from_str::<StoredCredentialVersion>(raw)
                .map_err(saved_credential_error)?
                .version;
            return match version {
                1 => {
                    let stored = serde_json::from_str::<StoredSubsonicCredential>(raw)
                        .map_err(saved_credential_error)?;
                    let credential = Self::Token {
                        salt: stored.salt,
                        token: stored.token,
                        navidrome_password: stored.navidrome_password,
                    };
                    credential.validate()?;
                    Ok(credential)
                }
                2 => {
                    let stored = serde_json::from_str::<StoredApiKeyCredential>(raw)
                        .map_err(saved_credential_error)?;
                    Self::from_api_key(&stored.api_key)
                }
                3 => {
                    let stored = serde_json::from_str::<StoredLegacyCredential>(raw)
                        .map_err(saved_credential_error)?;
                    let credential = Self::LegacyPassword(stored.password);
                    credential.validate()?;
                    Ok(credential)
                }
                version => Err(SourceError::Other(format!(
                    "saved Subsonic credential version {version} is not supported"
                ))),
            };
        }
        let Some((salt, token)) = raw.split_once(':') else {
            return Err(SourceError::Other(
                "saved Subsonic credential is invalid".to_string(),
            ));
        };
        if salt.is_empty() || token.is_empty() {
            return Err(SourceError::Other(
                "saved Subsonic credential is invalid".to_string(),
            ));
        }
        Ok(Self::Token {
            salt: salt.to_string(),
            token: token.to_string(),
            navidrome_password: None,
        })
    }

    pub(super) fn serialize(&self) -> String {
        match self {
            Self::Token {
                salt,
                token,
                navidrome_password: None,
            } => format!("{salt}:{token}"),
            Self::Token {
                salt,
                token,
                navidrome_password: Some(password),
            } => serde_json::to_string(&StoredSubsonicCredential {
                version: 1,
                salt: salt.clone(),
                token: token.clone(),
                navidrome_password: Some(password.clone()),
            })
            .expect("the Navidrome credential contains only JSON strings"),
            Self::ApiKey(api_key) => serde_json::to_string(&StoredApiKeyCredential {
                version: 2,
                api_key: api_key.clone(),
            })
            .expect("the OpenSubsonic API key is a JSON string"),
            Self::LegacyPassword(password) => serde_json::to_string(&StoredLegacyCredential {
                version: 3,
                password: password.clone(),
            })
            .expect("the legacy password is a JSON string"),
        }
    }

    pub(super) fn navidrome_password(&self) -> Option<&str> {
        match self {
            Self::Token {
                navidrome_password, ..
            } => navidrome_password.as_deref(),
            Self::ApiKey(_) | Self::LegacyPassword(_) => None,
        }
    }

    pub(super) fn authentication(&self) -> SubsonicAuthentication {
        match self {
            Self::Token { .. } => SubsonicAuthentication::Password,
            Self::ApiKey(_) => SubsonicAuthentication::ApiKey,
            Self::LegacyPassword(_) => SubsonicAuthentication::LegacyPassword,
        }
    }

    fn validate(&self) -> SourceResult<()> {
        let invalid = match self {
            Self::Token {
                salt,
                token,
                navidrome_password,
            } => {
                salt.is_empty()
                    || token.is_empty()
                    || navidrome_password
                        .as_ref()
                        .is_some_and(|password| password.is_empty())
            }
            Self::ApiKey(secret) | Self::LegacyPassword(secret) => secret.is_empty(),
        };
        if invalid {
            return Err(SourceError::Other(
                "saved Subsonic credential is invalid".to_string(),
            ));
        }
        Ok(())
    }

    pub(super) fn common_query<'a>(
        &'a self,
        username: &'a str,
        extra: &'a [(&'a str, &'a str)],
    ) -> Vec<(&'a str, &'a str)> {
        let mut query = match self {
            Self::Token { salt, token, .. } => {
                vec![("u", username), ("s", salt.as_str()), ("t", token.as_str())]
            }
            Self::ApiKey(api_key) => vec![("apiKey", api_key.as_str())],
            Self::LegacyPassword(password) => vec![("u", username), ("p", password.as_str())],
        };
        query.extend_from_slice(&[("v", API_VERSION), ("c", CLIENT_NAME), ("f", "json")]);
        query.extend_from_slice(extra);
        query
    }
}

fn saved_credential_error(error: serde_json::Error) -> SourceError {
    SourceError::Other(format!("saved Subsonic credential is invalid: {error}"))
}
#[derive(Debug)]
pub(super) struct SubsonicApiResponse {
    pub(super) body: Value,
    pub(super) server_type: Option<String>,
}
pub(super) async fn subsonic_json(
    request: reqwest::RequestBuilder,
) -> SourceResult<SubsonicApiResponse> {
    let mut envelope: Value = remote_http::json(
        request,
        SUBSONIC_HTTP,
        BodyLimit {
            max_bytes: SUBSONIC_JSON_MAX_BYTES,
            context: "Subsonic JSON response",
        },
    )
    .await?;
    let body = envelope
        .get_mut("subsonic-response")
        .map(Value::take)
        .unwrap_or_default();
    if body["status"].as_str() != Some("ok") {
        let error = &body["error"];
        let code = json::field::<u16>(error, "code");
        let message = json::field::<String>(error, "message")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                code.map_or_else(
                    || "Subsonic request failed".to_string(),
                    |code| format!("Subsonic error {code}"),
                )
            });
        let message = match json::field::<String>(error, "helpUrl")
            .filter(|value| !value.trim().is_empty())
        {
            Some(url) => format!("{message} ({url})"),
            None => message,
        };
        return Err(if matches!(code, Some(40..=44)) {
            SourceError::Auth(message)
        } else {
            SourceError::Server {
                status: 200,
                message,
            }
        });
    }
    Ok(SubsonicApiResponse {
        server_type: json::field(&body, "type"),
        body,
    })
}
pub(super) async fn subsonic_bytes(request: reqwest::RequestBuilder) -> SourceResult<ImageBytes> {
    remote_http::bytes(
        request,
        SUBSONIC_HTTP,
        BodyLimit {
            max_bytes: SUBSONIC_IMAGE_MAX_BYTES,
            context: "Subsonic image response",
        },
    )
    .await
}
pub(super) fn build_client(trust_invalid_cert: bool) -> SourceResult<Client> {
    build_client_with_timeouts(
        trust_invalid_cert,
        SUBSONIC_CONNECT_TIMEOUT,
        SUBSONIC_REQUEST_TIMEOUT,
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
        SUBSONIC_HTTP,
    )
}
pub(super) fn normalize_base_url(raw: &str) -> SourceResult<Url> {
    let trimmed = raw.trim().trim_end_matches('/');
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = Url::parse(&candidate).map_err(|error| SourceError::Other(error.to_string()))?;
    let path = url.path().trim_end_matches('/');
    let path = path.strip_suffix("/rest").unwrap_or(path);
    let normalized_path = if path.is_empty() {
        "/".to_string()
    } else {
        format!("{path}/")
    };
    url.set_path(&normalized_path);
    Ok(url)
}

pub(super) fn endpoint(base_url: &Url, method: &str) -> SourceResult<Url> {
    let mut url = base_url.clone();
    let base_path = base_url.path().trim_end_matches('/');
    let method = method.trim_end_matches(".view");
    let full_path = if base_path.is_empty() {
        format!("/rest/{method}.view")
    } else {
        format!("{base_path}/rest/{method}.view")
    };
    url.set_path(&full_path);
    url.set_query(None);
    Ok(url)
}

const CLIENT_NAME: &str = "Rufin";
const API_VERSION: &str = "1.16.1";
const SALT_BYTES: usize = 12;

pub(super) fn redact_subsonic_query(url: &mut Url) {
    let pairs = url
        .query_pairs()
        .map(|(key, value)| {
            let value = if matches!(key.as_ref(), "apiKey" | "p" | "s" | "t") {
                "<redacted>".into()
            } else {
                value
            };
            (key.into_owned(), value.into_owned())
        })
        .collect::<Vec<_>>();
    url.query_pairs_mut().clear().extend_pairs(pairs);
}
pub(super) fn redacted_subsonic_url(url: &Url) -> String {
    let mut redacted = url.clone();
    redact_subsonic_query(&mut redacted);
    redacted.to_string()
}
pub(super) fn playlist_entry_id(playlist_id: &str, index: usize, track_id: &str) -> String {
    format!("{}:{index}:{track_id}", playlist_id)
}
pub(super) fn current_year() -> u16 {
    let days_since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() / 86_400)
        .unwrap_or_default();
    year_from_unix_days(days_since_epoch)
}
pub(super) fn year_from_unix_days(mut days: u64) -> u16 {
    let mut year = 1970_u16;
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days < days_in_year {
            return year;
        }
        days -= days_in_year;
        year = year.saturating_add(1);
    }
}
pub(super) fn is_leap_year(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}
pub(super) fn random_salt() -> String {
    let mut bytes = [0_u8; SALT_BYTES];
    if getrandom::fill(&mut bytes).is_err() {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (seed.rotate_left(index as u32) & 0xff) as u8;
        }
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
pub(super) fn color_seed(id: &str) -> u32 {
    (stable_hash(id) & 0xffff_ffff) as u32
}
pub(super) fn favorite(value: &Option<serde_json::Value>) -> bool {
    value.as_ref().is_some_and(|value| match value {
        serde_json::Value::Bool(value) => *value,
        serde_json::Value::String(value) => !value.trim().is_empty(),
        serde_json::Value::Null
        | serde_json::Value::Number(_)
        | serde_json::Value::Array(_)
        | serde_json::Value::Object(_) => false,
    })
}

impl SubsonicSource {
    fn source_id(&self) -> &str {
        self.flavor.source_id()
    }

    pub(super) fn id(&self, kind: &str, raw_id: &str) -> String {
        format!("{}:{kind}:{raw_id}", self.source_id())
    }

    fn authenticated_url(&self, method: &str, extra: &[(&str, String)]) -> SourceResult<Url> {
        let mut url = endpoint(&self.base_url, method)?;
        {
            let mut query = url.query_pairs_mut();
            query.extend_pairs(self.credential.common_query(&self.username, &[]));
            for (key, value) in extra {
                query.append_pair(key, value);
            }
        }
        Ok(url)
    }

    pub(super) async fn get_json(
        &self,
        method: &str,
        extra: &[(&str, String)],
    ) -> SourceResult<Value> {
        let url = self.authenticated_url(method, extra)?;
        subsonic_json(self.client.get(url))
            .await
            .map(|response: SubsonicApiResponse| response.body)
    }

    async fn get_unit(&self, method: &str, extra: &[(&str, String)]) -> SourceResult<()> {
        let url = self.authenticated_url(method, extra)?;
        subsonic_json(self.client.get(url)).await.map(|_| ())
    }

    async fn similar_songs(&self, raw_id: &str, count: usize) -> SourceResult<Vec<Track>> {
        let body: Value = self
            .get_json(
                "getSimilarSongs",
                &[
                    ("id", raw_id.to_string()),
                    ("count", count.clamp(1, 500).to_string()),
                ],
            )
            .await?;
        Ok(json::items(&body["similarSongs"]["song"])
            .iter()
            .filter_map(|song| track_from_json(self, song))
            .collect())
    }

    async fn replace_playlist_tracks(
        &self,
        playlist_id: &str,
        track_ids: &[String],
    ) -> SourceResult<()> {
        let mut extra = vec![("playlistId", raw_item_id(playlist_id).to_string())];
        extra.extend(
            track_ids
                .iter()
                .map(|track_id| ("songId", raw_item_id(track_id).to_string())),
        );
        self.get_unit("createPlaylist", &extra).await
    }
}

#[cfg(test)]
mod tests {
    use super::super::native_lyrics_from_structured;
    use super::{SubsonicCredential, redacted_subsonic_url};
    use crate::subsonic::{
        SubsonicAuthentication, SubsonicFlavor, SubsonicSource, SubsonicSourceConfig,
    };
    use lyrics::{LyricsAgentRole, LyricsRole};
    use reqwest::Url;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn subsonic_urls_and_credentials_never_expose_authentication_values() {
        let url = Url::parse(
            "https://music.example/rest/stream?apiKey=secret-key&p=password&s=salt&t=token&id=track-one",
        )
        .expect("Subsonic URL");
        let redacted = redacted_subsonic_url(&url);
        for secret in ["secret-key", "password", "salt", "token"] {
            assert!(!redacted.contains(secret));
        }
        assert!(redacted.contains("id=track-one"));

        let credential = SubsonicCredential::from_api_key("secret-key").expect("API key");
        assert!(!format!("{credential:?}").contains("secret-key"));
    }

    fn source(url: &str) -> SubsonicSource {
        SubsonicSource::open(
            SubsonicFlavor::Subsonic,
            SubsonicSourceConfig {
                base_url: url.to_string(),
                username: "listener".into(),
                trust_invalid_cert: false,
                navidrome_library_version: 0,
                authentication: SubsonicAuthentication::Password,
            },
            SubsonicCredential::from_password("password").serialize(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn optional_metadata_does_not_block_login_tracks_or_playlist_positions() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/ping.view"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response": {"status":"ok", "type": {"unexpected": true}}
            })))
            .mount(&server)
            .await;
        for (admin, expected) in [
            (serde_json::json!("false"), false),
            (serde_json::json!(true), true),
            (serde_json::json!("true"), true),
            (serde_json::json!({"unexpected":true}), false),
        ] {
            let _user = Mock::given(method("GET")).and(path("/rest/getUser.view"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "subsonic-response": {"status":"ok", "user":{"username":"canonical", "adminRole":admin}}
                }))).mount_as_scoped(&server).await;
            let authenticated = SubsonicSource::authenticate(
                crate::SourceId::new("source"),
                SubsonicFlavor::Subsonic,
                SubsonicAuthentication::Password,
                crate::CredentialHostInput {
                    server_name: None,
                    server_url: server.uri(),
                    username: "listener".into(),
                    password: "password".into(),
                    trust_invalid_cert: false,
                },
            )
            .await
            .unwrap();
            assert_eq!(authenticated.source.username, "canonical");
            assert_eq!(authenticated.source.metadata_editing_available(), expected);
        }
        let source = source(&server.uri());
        let song = serde_json::json!({
            "id": "one", "title": "Playable", "isDir":"false", "duration": "42", "year":"2024",
            "artists":[{"id":"artist","name":"Artist"}, {"name":"unidentified"}],
            "replayGain":{"trackGain":"-4.25","albumGain":{},"trackPeak":0.91}
        });
        Mock::given(method("GET"))
            .and(path("/rest/getSong.view"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response":{"status":"ok","song":song}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/rest/getPlaylist.view"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response":{"status":"ok","playlist":{
                    "id":"list", "name":[], "entry":[song, {"title":"no id"}, {"id":"one","isDir":{}}]
                }}
            }))).mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/rest/getMusicDirectory.view"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response":{"status":"ok","directory":{"child":[
                    {"id":"folder","isDir":"true"}, {"title":"no id"}, song
                ]}}
            })))
            .mount(&server)
            .await;
        let track = source.read_track("one").await.unwrap();
        assert_eq!(track.title, "Playable");
        assert_eq!(track.duration_seconds, 42);
        assert_eq!(track.year, 2024);
        assert_eq!(track.relations.artists.len(), 1);
        assert_eq!(track.replay_gain_track_db, Some(-4.25));
        assert_eq!(track.replay_gain_track_peak, Some(0.91));
        assert_eq!(track.replay_gain_album_db, None);
        let playlist = source.read_playlist("list").await.unwrap();
        assert_eq!(playlist.entries.len(), 2);
        assert_eq!(
            playlist.entries[1].occurrence_id,
            super::playlist_entry_id(&playlist.playlist.id, 2, "one")
        );
        let folder = source.browse_folder(Some("root"), None).await.unwrap();
        assert_eq!(folder.folders[0].object_id, "subsonic:folder:folder");
        assert_eq!(folder.tracks, ["subsonic:track:one"]);
    }

    #[test]
    fn opensubsonic_song_reads_replay_gain() {
        let song = super::track_from_json(
            &source("https://music.example"),
            &serde_json::json!({
                "id": "track-one",
                "replayGain": {"trackGain":-4.25,"albumGain":-3.5,"trackPeak":0.91,"albumPeak":0.95}
            }),
        )
        .unwrap();
        assert_eq!(song.replay_gain_track_db, Some(-4.25));
        assert_eq!(song.replay_gain_album_db, Some(-3.5));
        assert_eq!(song.replay_gain_track_peak, Some(0.91));
        assert_eq!(song.replay_gain_album_peak, Some(0.95));
    }

    #[tokio::test]
    async fn login_keeps_server_errors_and_unreadable_envelopes_as_errors() {
        for (body, auth_error) in [
            (
                serde_json::json!({"subsonic-response":{"status":"failed","error":{"code":40,"message":"Wrong password"}}}),
                true,
            ),
            (
                serde_json::json!({"subsonic-response":{"status":"failed","error":{"code":0,"message":"Unavailable"}}}),
                false,
            ),
            (serde_json::json!([]), false),
            (serde_json::json!(42), false),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/rest/ping.view"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
            let result = SubsonicSource::authenticate(
                crate::SourceId::new("source"),
                SubsonicFlavor::Subsonic,
                SubsonicAuthentication::Password,
                crate::CredentialHostInput {
                    server_name: None,
                    server_url: server.uri(),
                    username: "listener".into(),
                    password: "password".into(),
                    trust_invalid_cert: false,
                },
            )
            .await;
            let error = result.err().expect("failed login");
            assert_eq!(matches!(error, crate::SourceError::Auth(_)), auth_error);
        }
    }

    #[tokio::test]
    async fn structured_lyrics_keep_lines_and_cues_when_optional_facts_are_unreadable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/getOpenSubsonicExtensions.view"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response":{"status":"ok","openSubsonicExtensions":[
                    null, {"name":"songLyrics","versions":[{},2]}
                ]}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/getLyricsBySongId.view"))
            .and(query_param("enhanced", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response":{"status":"ok","lyricsList":{"structuredLyrics":[{
                    "lang":"en","offset":{},
                    "agents":[null,{"id":"singer","role":"main","name":false}],
                    "line":[{"value":"Hi","start":10},{"value":{}},{"value":"There","start":{}}],
                    "cueLine":[{"index":2,"value":"There","agentId":"singer","cue":[
                        {"value":"There","start":20,"byteStart":0,"byteEnd":4},
                        {"value":"Unreadable","start":{}}
                    ]}]
                }]}}
            })))
            .mount(&server)
            .await;
        let lyrics = source(&server.uri()).lyrics("one").await.unwrap().unwrap();
        let document = &lyrics.documents()[0];
        assert_eq!(document.language.as_deref(), Some("en"));
        assert_eq!(document.offset_millis, 0);
        assert_eq!(document.agents.len(), 1);
        assert_eq!(document.lines.len(), 2);
        assert_eq!(document.lines[0].start_millis, Some(10));
        assert_eq!(document.lines[1].text, "There");
        let cue = &document.lines[1].cue_lines[0].cues[0];
        assert_eq!(cue.text, "There");
        assert_eq!(cue.byte_end_exclusive, 5);
    }

    #[test]
    fn structured_lyrics_normalize_offsets_and_languages_without_losing_cues() {
        for (offset, start, end) in [(20, 0, 80), (-20, 30, 120)] {
            let lyrics = native_lyrics_from_structured(&[serde_json::json!({
                "kind":"translation", "lang":"eng", "offset":offset,
                "agents":[{"id":"singer","role":"bg","name":"Singer"}],
                "line":[{"value":"Hi","start":10}],
                "cueLine":[{"index":0,"value":"Hi","start":10,"end":100,"agentId":"singer",
                    "cue":[{"value":"Hi","start":10,"end":100,"byteStart":0,"byteEnd":1}]}]
            })]);
            let document = &lyrics.documents()[0];
            assert_eq!(document.role, LyricsRole::Translation);
            assert_eq!(document.language.as_deref(), Some("en"));
            assert_eq!(document.offset_millis, 0);
            assert_eq!(document.agents[0].role, LyricsAgentRole::Background);
            assert_eq!(document.agents[0].name.as_deref(), Some("Singer"));
            let line = &document.lines[0];
            assert_eq!(
                (line.start_millis, line.end_millis),
                (Some(start), Some(end))
            );
            let cue_line = &line.cue_lines[0];
            assert_eq!(
                (cue_line.start_millis, cue_line.end_millis),
                (Some(start), Some(end))
            );
            assert_eq!(cue_line.agent_id.as_deref(), Some("singer"));
            let cue = &cue_line.cues[0];
            assert_eq!((cue.start_millis, cue.end_millis), (start, Some(end)));
            assert_eq!((cue.byte_start, cue.byte_end_exclusive), (0, 2));
            let mut normalized = document.clone();
            normalized.normalize_timing_and_language();
            assert_eq!(&normalized, document);
        }
        let lyrics = native_lyrics_from_structured(&[serde_json::json!({
            "lang":"und", "line":[{"value":"Unknown language"}]
        })]);
        assert_eq!(lyrics.documents()[0].language, None);
    }

    #[tokio::test]
    async fn explicit_authentication_modes_survive_reopen_and_optional_endpoint_failure() {
        for authentication in [
            SubsonicAuthentication::Password,
            SubsonicAuthentication::LegacyPassword,
            SubsonicAuthentication::ApiKey,
        ] {
            let server = MockServer::start().await;
            let endpoint = if authentication == SubsonicAuthentication::ApiKey {
                "tokenInfo"
            } else {
                "ping"
            };
            Mock::given(method("GET"))
                .and(path(format!("/rest/{endpoint}.view")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "subsonic-response": { "status": "ok", "version": "1.16.1", "type": "Nextcloud Music",
                        "tokenInfo": { "username": "api-user" } }
                })))
                .expect(1)
                .mount(&server).await;
            let source_id = crate::SourceId::new("opaque-source-instance");
            let authenticated = SubsonicSource::authenticate(
                source_id.clone(),
                SubsonicFlavor::Subsonic,
                authentication,
                crate::CredentialHostInput {
                    server_name: None,
                    server_url: server.uri(),
                    username: "api-user".into(),
                    password: "generated & password".into(),
                    trust_invalid_cert: false,
                },
            )
            .await
            .expect("optional getUser must not reject a source");
            assert_eq!(authenticated.configuration.source_id, source_id);
            let saved =
                SubsonicSourceConfig::from_configuration(&authenticated.configuration).unwrap();
            assert_eq!(saved.authentication, authentication);
            let restored = SubsonicCredential::parse(&authenticated.credential).unwrap();
            assert_eq!(restored.authentication(), authentication);
            assert!(!format!("{restored:?}").contains("generated & password"));
            let requests = server.received_requests().await.unwrap();
            let request = &requests[0];
            let query = request
                .url
                .query_pairs()
                .collect::<std::collections::HashMap<_, _>>();
            match authentication {
                SubsonicAuthentication::Password => {
                    assert_eq!(query.get("u").unwrap(), "api-user");
                    assert!(query.contains_key("s") && query.contains_key("t"));
                    assert!(!query.contains_key("p") && !query.contains_key("apiKey"));
                }
                SubsonicAuthentication::LegacyPassword => {
                    assert_eq!(query.get("u").unwrap(), "api-user");
                    assert_eq!(query.get("p").unwrap(), "generated & password");
                    assert!(
                        !query.contains_key("t")
                            && !query.contains_key("s")
                            && !query.contains_key("apiKey")
                    );
                }
                SubsonicAuthentication::ApiKey => {
                    assert_eq!(query.get("apiKey").unwrap(), "generated & password");
                    assert!(
                        !query.contains_key("u")
                            && !query.contains_key("p")
                            && !query.contains_key("t")
                    );
                    assert_eq!(requests.len(), 1, "API key login uses tokenInfo directly");
                }
            }
        }
    }

    #[test]
    fn opensubsonic_song_accepts_absent_optional_metadata() {
        for value in [
            serde_json::json!({ "id": "track-one" }),
            serde_json::json!({
                "id": "track-one",
                "isDir": null,
                "artists": null,
                "albumArtists": null,
                "genres": null,
                "moods": null,
                "replayGain": null
            }),
        ] {
            let song = super::track_from_json(&source("https://music.example"), &value)
                .expect("OpenSubsonic song without optional metadata");

            assert!(song.relations.artists.is_empty());
            assert!(song.relations.album_artists.is_empty());
            assert!(song.relations.genres.is_empty());
            assert!(song.relations.moods.is_empty());
            assert!(song.replay_gain_track_db.is_none());
        }
    }

    #[tokio::test]
    async fn live_search_publishes_prepared_uri_rows_and_skips_malformed_entries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/search3.view"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response": { "status": "ok", "version": "1.16.1", "searchResult3": {
                    "artist": [{"id":"artist-one", "name":"Artist"}, {"name":"broken"}],
                    "album": [{"id":"album-one", "name":"Album", "artist":"Artist", "artistId":"artist-one", "coverArt":"cover-one"}],
                    "song": [{"id":"track-one", "title":"Track", "artist":"Artist", "artistId":"artist-one", "album":"Album", "albumId":"album-one", "coverArt":"cover-one"}, {"title":"broken"}]
                }}
            }))).mount(&server).await;
        let source = SubsonicSource::open(
            SubsonicFlavor::Subsonic,
            SubsonicSourceConfig {
                base_url: server.uri(),
                username: "listener".to_string(),
                trust_invalid_cert: false,
                navidrome_library_version: 0,
                authentication: SubsonicAuthentication::Password,
            },
            SubsonicCredential::from_password("password").serialize(),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let source_id = crate::SourceId::new("search-source");
        library::Scan::begin(&database, source_id.as_str(), "Search", "search", None)
            .await
            .unwrap()
            .finish()
            .await
            .unwrap();
        let (results, outcome) = source
            .live_search(&source_id, &database, "Track", 60)
            .await
            .unwrap();
        assert!(matches!(outcome, Some(library::ScanOutcome::Changed(_))));
        assert_eq!(
            (
                results.tracks.len(),
                results.albums.len(),
                results.artists.len()
            ),
            (1, 1, 1)
        );
        let row = &results.tracks[0];
        assert_eq!(
            row.media_uri,
            library::source_entity_uri(&source_id, "track", "subsonic:track:track-one")
        );
        let stored = database
            .track_row_by_uri(&row.media_uri, &library::ReadCancellation::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row, &stored);
        assert_eq!(row.artwork_binding, results.albums[0].artwork_binding);
        assert!(row.artwork_binding.is_some());
        let album = database
            .album_detail(
                &results.albums[0].media_uri,
                library::TrackSort::TrackNumber,
                false,
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(album.track_order, vec![row.media_uri.clone()]);
        let songs = (0..125).map(|index| serde_json::json!({
            "id": format!("collection-{index}"), "title":format!("Track {index}"),
            "albumId":"album-one", "album":"Album", "artistId":"artist-one", "artist":"Artist"
        })).collect::<Vec<_>>();
        Mock::given(method("GET")).and(path("/rest/getAlbum.view"))
            .and(query_param("id", "album-one"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response":{"status":"ok","version":"1.16.1","album":{"id":"album-one","name":"Album","artistId":"artist-one","artist":"Artist","song":songs}}
            }))).mount(&server).await;
        let mut scan = library::Scan::begin_items(&database, source_id.as_str())
            .await
            .unwrap();
        source
            .stage_collection(
                &mut scan,
                &crate::SourceCollection::Album("subsonic:album:album-one".to_string()),
            )
            .await
            .unwrap();
        scan.finish().await.unwrap();
        let album = database
            .album_detail(
                &results.albums[0].media_uri,
                library::TrackSort::TrackNumber,
                false,
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            album.track_order.len(),
            126,
            "explicit collection acquisition retains more than one presentation window and never removes unseen rows"
        );
    }

    #[tokio::test]
    async fn song_only_search_keeps_album_text_and_existing_relationship() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/rest/search3.view"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response":{"status":"ok","version":"1.16.1","searchResult3":{
                    "song":[{"id":"song","title":"Match","album":"Nonmatching album","albumId":"album","coverArt":"song-cover"}]
                }}
            }))).mount(&server).await;
        let source = SubsonicSource::open(
            SubsonicFlavor::Subsonic,
            SubsonicSourceConfig {
                base_url: server.uri(),
                username: "listener".into(),
                trust_invalid_cert: false,
                navidrome_library_version: 0,
                authentication: SubsonicAuthentication::Password,
            },
            SubsonicCredential::from_password("password").serialize(),
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
                super::stage_album(&mut scan, super::album_from_json(&source, &serde_json::json!({"id":"album","name":"Nonmatching album","coverArt":"album-cover"})).unwrap()).await.unwrap();
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

    #[tokio::test]
    async fn opensubsonic_recommendations_use_provider_similar_songs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/getSimilarSongs.view"))
            .and(query_param("id", "artist-one"))
            .and(query_param("count", "25"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response": {
                    "status": "ok",
                    "version": "1.16.1",
                    "similarSongs": {"song": [{"id": "track-two", "title": "Two"}]}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let credential = SubsonicCredential::from_password("password").serialize();
        let source = SubsonicSource::open(
            SubsonicFlavor::Subsonic,
            SubsonicSourceConfig {
                base_url: server.uri(),
                username: "listener".to_string(),
                trust_invalid_cert: false,
                navidrome_library_version: 0,
                authentication: SubsonicAuthentication::Password,
            },
            credential,
        )
        .expect("OpenSubsonic source");

        assert_eq!(
            source
                .generated_track_object_ids(
                    &crate::SourceRadioSeed::Artist("subsonic:artist:artist-one".to_string()),
                    25,
                )
                .await
                .expect("OpenSubsonic recommendations"),
            ["subsonic:track:track-two"]
        );
    }
}
