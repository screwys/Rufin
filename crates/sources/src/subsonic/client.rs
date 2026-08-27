use super::*;

use crate::remote_http::{self, BodyLimit, RemoteHttpPolicy, RemoteTimeouts};
use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeOwned, IntoDeserializer, Visitor},
};
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
                let body: RandomSongsBody = self
                    .get_json(
                        "getRandomSongs",
                        &[
                            ("size", limit.clamp(1, 500).to_string()),
                            ("genre", name.clone()),
                        ],
                    )
                    .await?;
                body.random_songs
                    .map(|songs| songs.song)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|song| track_from_dto(self, song))
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
            let body: MusicDirectoryBody = self
                .get_json(
                    "getMusicDirectory",
                    &[("id", raw_item_id(folder).to_string())],
                )
                .await?;
            for child in body.directory.child {
                if child.is_dir.unwrap_or(false) {
                    let folder = folder_from_child(self, child);
                    page.folders.push(crate::LiveFolder {
                        object_id: folder.id,
                        name: folder.name,
                    });
                } else {
                    page.tracks
                        .push(String::from(self.id("track", &raw_id_string(&child.id))));
                }
            }
        } else {
            let parameters = music_folder_object_id
                .map(|folder| vec![("musicFolderId", raw_item_id(folder).to_string())])
                .unwrap_or_default();
            let body: IndexesBody = self.get_json("getIndexes", &parameters).await?;
            for artist in body
                .indexes
                .map(|indexes| indexes.index)
                .unwrap_or_default()
                .into_iter()
                .flat_map(|index| index.artist)
            {
                let folder = folder_from_artist(self, artist);
                page.folders.push(crate::LiveFolder {
                    object_id: folder.id,
                    name: folder.name,
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
        let count = limit.clamp(1, 100).to_string();
        let body: SearchBody = self
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
        let results = body.search_result.unwrap_or_default();
        Ok(crate::LiveSearchResults {
            artists: results
                .artist
                .unwrap_or_default()
                .into_iter()
                .map(|value| artist_from_dto(self, value))
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
            albums: results
                .album
                .unwrap_or_default()
                .into_iter()
                .map(|value| album_from_dto(self, value))
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
            tracks: results
                .song
                .unwrap_or_default()
                .into_iter()
                .map(|value| track_from_dto(self, value))
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
}

impl SubsonicSource {
    pub(crate) async fn collection_track_object_ids(
        &self,
        collection: &crate::SourceCollection,
        limit: usize,
    ) -> SourceResult<Vec<String>> {
        let limit = limit.clamp(1, 500);
        let album_ids = match collection {
            crate::SourceCollection::Album(id) => vec![raw_item_id(id).to_string()],
            crate::SourceCollection::Artist(id) => {
                let body: ArtistBody = self
                    .get_json("getArtist", &[("id", raw_item_id(id).to_string())])
                    .await?;
                body.artist
                    .album
                    .into_iter()
                    .map(|album| raw_id_string(&album.id))
                    .collect()
            }
        };
        let mut tracks = Vec::new();
        for album_id in album_ids {
            if tracks.len() >= limit {
                break;
            }
            let body: AlbumBody = self.get_json("getAlbum", &[("id", album_id)]).await?;
            tracks.extend(
                body.album
                    .song
                    .into_iter()
                    .map(|song| String::from(self.id("track", &raw_id_string(&song.id)))),
            );
        }
        tracks.truncate(limit);
        Ok(tracks)
    }

    pub(super) async fn read_track(&self, track_id: &str) -> SourceResult<Track> {
        let body: SongBody = self
            .get_json("getSong", &[("id", raw_item_id(track_id).to_string())])
            .await?;
        Ok(track_from_dto(self, body.song))
    }
}

impl SubsonicSource {
    pub(super) async fn read_playlist(&self, playlist_id: &str) -> SourceResult<PlaylistSnapshot> {
        let body: PlaylistBody = self
            .get_json(
                "getPlaylist",
                &[("id", raw_item_id(playlist_id).to_string())],
            )
            .await?;
        let playlist = playlist_from_dto(self, body.playlist.clone());
        let entries = body
            .playlist
            .entry
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(index, song)| {
                let raw_track_id = raw_id_string(&song.id);
                PlaylistEntry {
                    occurrence_id: playlist_entry_id(&playlist.id, index, &raw_track_id),
                    track_id: String::from(self.id("track", &raw_track_id)),
                }
            })
            .collect::<Vec<_>>();
        Ok(PlaylistSnapshot { playlist, entries })
    }
}

impl SubsonicSource {
    pub(crate) async fn resolve_stream(
        &self,
        request: &StreamRequest,
    ) -> SourceResult<ResolvedStream> {
        let format = if request.quality.max_bitrate_kbps().is_some() {
            "mp3"
        } else {
            "raw"
        };
        self.resolve_audio(
            &request.track_object_id,
            request.quality.max_bitrate_kbps(),
            format,
        )
    }

    pub(crate) fn resolve_download(
        &self,
        request: &StreamRequest,
    ) -> SourceResult<crate::ResolvedDownload> {
        let (format, extension) = match request.quality {
            StreamQuality::Original => ("raw", None),
            StreamQuality::MaxBitrateKbps(_) if self.flavor == SubsonicFlavor::Navidrome => {
                ("opus", Some("opus"))
            }
            StreamQuality::MaxBitrateKbps(_) => ("mp3", Some("mp3")),
        };
        let stream = self.resolve_audio(
            &request.track_object_id,
            request.quality.max_bitrate_kbps(),
            format,
        )?;
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
        let body: PlaylistBody = self.get_json("createPlaylist", &extra).await?;
        Ok(String::from(
            self.id("playlist", &raw_id_string(&body.playlist.id)),
        ))
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
    pub(crate) async fn lyrics(
        &self,
        track_id: &str,
        _search: LyricsSearch,
    ) -> SourceResult<Option<NativeLyrics>> {
        let extensions: OpenSubsonicExtensionsBody = self
            .get_json("getOpenSubsonicExtensions", &[])
            .await
            .unwrap_or_default();
        let song_lyrics_version = extensions
            .open_subsonic_extensions
            .iter()
            .find(|extension| extension.name == "songLyrics")
            .and_then(|extension| extension.versions.iter().max())
            .copied()
            .unwrap_or_default();
        if song_lyrics_version >= 1 {
            let mut extra = vec![("id", raw_item_id(track_id).to_string())];
            if song_lyrics_version >= 2 {
                extra.push(("enhanced", "true".to_string()));
            }
            let body: StructuredLyricsBody = self.get_json("getLyricsBySongId", &extra).await?;
            let lyrics = native_lyrics_from_structured(body.lyrics_list.structured_lyrics);
            return Ok((!lyrics.documents.is_empty()).then_some(lyrics));
        }

        let track = self.read_track(track_id).await?;
        let body: LyricsBody = self
            .get_json(
                "getLyrics",
                &[
                    ("artist", track.artist.clone()),
                    ("title", track.title.clone()),
                ],
            )
            .await?;
        let Some(lyrics) = body.lyrics else {
            return Ok(None);
        };
        let Some(value) = lyrics.value.filter(|value| !value.trim().is_empty()) else {
            return Ok(None);
        };
        Ok(Some(NativeLyrics {
            documents: vec![NativeLyricsDocument {
                role: NativeLyricsRole::Original,
                language: None,
                offset_millis: 0,
                lines: value
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| NativeLyricLine {
                        text: line.trim().to_string(),
                        start_millis: None,
                        end_millis: None,
                        cue_lines: Vec::new(),
                    })
                    .collect(),
                agents: Vec::new(),
            }],
        }))
    }
}

pub(super) fn native_lyrics_from_structured(entries: Vec<StructuredLyricsDto>) -> NativeLyrics {
    let documents = entries
        .into_iter()
        .filter_map(|entry| {
            let role = match entry.kind.as_deref().unwrap_or("main") {
                "main" => NativeLyricsRole::Original,
                "translation" => NativeLyricsRole::Translation,
                "pronunciation" => NativeLyricsRole::Pronunciation,
                _ => return None,
            };
            let agents = entry
                .agents
                .into_iter()
                .filter_map(|agent| {
                    let role = match agent.role.as_str() {
                        "main" => NativeLyricAgentRole::Main,
                        "voice" => NativeLyricAgentRole::Voice,
                        "bg" => NativeLyricAgentRole::Background,
                        "group" => NativeLyricAgentRole::Group,
                        _ => return None,
                    };
                    Some(NativeLyricAgent {
                        id: agent.id,
                        role,
                        name: agent.name,
                    })
                })
                .collect::<Vec<_>>();
            let mut cue_lines_by_index = vec![Vec::new(); entry.line.len()];
            for cue_line in entry.cue_line {
                let Some(lines) = cue_lines_by_index.get_mut(cue_line.index) else {
                    continue;
                };
                let cues = cue_line
                    .cue
                    .into_iter()
                    .filter_map(|cue| {
                        let byte_end_exclusive = cue.byte_end.checked_add(1)?;
                        (cue.byte_start <= cue.byte_end
                            && byte_end_exclusive <= cue_line.value.len()
                            && cue_line.value.is_char_boundary(cue.byte_start)
                            && cue_line.value.is_char_boundary(byte_end_exclusive))
                        .then_some(NativeLyricCue {
                            text: cue.value,
                            start_millis: cue.start,
                            end_millis: cue.end,
                            byte_start: cue.byte_start,
                            byte_end_exclusive,
                        })
                    })
                    .collect();
                lines.push(NativeLyricCueLine {
                    text: cue_line.value,
                    start_millis: cue_line.start,
                    end_millis: cue_line.end,
                    agent_id: cue_line.agent_id,
                    cues,
                });
            }
            let lines = entry
                .line
                .into_iter()
                .zip(cue_lines_by_index)
                .filter_map(|(line, cue_lines)| {
                    (!line.value.trim().is_empty()).then_some(NativeLyricLine {
                        text: line.value,
                        start_millis: line.start,
                        end_millis: cue_lines.iter().filter_map(|line| line.end_millis).max(),
                        cue_lines,
                    })
                })
                .collect::<Vec<_>>();
            (!lines.is_empty()).then_some(NativeLyricsDocument {
                role,
                language: normalize_native_language(entry.lang),
                offset_millis: entry.offset.unwrap_or_default(),
                lines,
                agents,
            })
        })
        .collect();
    NativeLyrics { documents }
}

fn normalize_native_language(language: String) -> Option<String> {
    let language = language.trim();
    (!language.is_empty()
        && !language.eq_ignore_ascii_case("und")
        && !language.eq_ignore_ascii_case("xxx"))
    .then(|| language.to_string())
}

impl SubsonicSource {
    pub(crate) async fn report_playback(&self, report: &SourceReportFact) -> SourceResult<()> {
        match report.phase {
            SourceReportPhase::Started => {
                self.get_unit(
                    "scrobble",
                    &[
                        ("id", raw_item_id(&report.track_object_id).to_string()),
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
                        ("id", raw_item_id(&report.track_object_id).to_string()),
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
        }
    }

    pub(super) fn navidrome_password(&self) -> Option<&str> {
        match self {
            Self::Token {
                navidrome_password, ..
            } => navidrome_password.as_deref(),
            Self::ApiKey(_) => None,
        }
    }

    pub(super) fn authentication(&self) -> SubsonicAuthentication {
        match self {
            Self::Token { .. } => SubsonicAuthentication::Password,
            Self::ApiKey(_) => SubsonicAuthentication::ApiKey,
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
            Self::ApiKey(api_key) => api_key.is_empty(),
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
pub(super) struct SubsonicApiResponse<T> {
    pub(super) body: T,
    pub(super) server_type: Option<String>,
}
pub(super) async fn subsonic_json<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> SourceResult<SubsonicApiResponse<T>> {
    let endpoint = request
        .try_clone()
        .and_then(|request| request.build().ok())
        .map(|request| request.url().path().to_string())
        .unwrap_or_else(|| "/rest".to_string());
    let envelope = remote_http::json::<SubsonicEnvelope>(
        request,
        SUBSONIC_HTTP,
        BodyLimit {
            max_bytes: SUBSONIC_JSON_MAX_BYTES,
            context: "Subsonic JSON response",
        },
    )
    .await?;
    if envelope.response.status != "ok" {
        let error = envelope.response.error;
        let code = error.as_ref().and_then(|error| error.code);
        let message = error
            .map(|error| {
                let message = error
                    .message
                    .filter(|message| !message.trim().is_empty())
                    .unwrap_or_else(|| {
                        error.code.map_or_else(
                            || "Subsonic request failed".to_string(),
                            |code| format!("Subsonic error {code}"),
                        )
                    });
                match error.help_url {
                    Some(help_url) if !help_url.trim().is_empty() => {
                        format!("{message} ({help_url})")
                    }
                    _ => message,
                }
            })
            .unwrap_or_else(|| format!("Subsonic returned {}", envelope.response.status));
        return Err(if matches!(code, Some(40..=44)) {
            SourceError::Auth(message)
        } else {
            SourceError::Server {
                status: 200,
                message,
            }
        });
    }
    let body = serde_path_to_error::deserialize::<_, T>(
        serde_json::Value::Object(envelope.response.body).into_deserializer(),
    )
    .map_err(|error| {
        SourceError::Other(format!(
            "opensubsonic response at {endpoint} field {}: {}",
            error.path(),
            error.inner()
        ))
    })?;
    Ok(SubsonicApiResponse {
        body,
        server_type: envelope.response.server_type,
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

pub(super) fn rest_endpoint_identity(base_url: &Url) -> String {
    let mut url = base_url.clone();
    let base_path = base_url.path().trim_end_matches('/');
    let path = if base_path.is_empty() {
        "/rest/".to_string()
    } else {
        format!("{base_path}/rest/")
    };
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
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

pub(super) fn unauthenticated_url(base_url: &Url, method: &str) -> SourceResult<Url> {
    let mut url = endpoint(base_url, method)?;
    url.query_pairs_mut()
        .extend_pairs([("v", API_VERSION), ("c", CLIENT_NAME), ("f", "json")]);
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
pub(super) fn raw_id_string(id: &SubsonicId) -> String {
    id.0.clone()
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
pub(super) fn stable_source_id(source_id: &str, base_url: &str, username: &str) -> String {
    format!(
        "{:016x}",
        stable_hash(&format!("{source_id}:{base_url}:{username}"))
    )
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

    pub(super) async fn get_json<T: DeserializeOwned>(
        &self,
        method: &str,
        extra: &[(&str, String)],
    ) -> SourceResult<T> {
        let url = self.authenticated_url(method, extra)?;
        subsonic_json(self.client.get(url))
            .await
            .map(|response: SubsonicApiResponse<T>| response.body)
    }

    async fn get_unit(&self, method: &str, extra: &[(&str, String)]) -> SourceResult<()> {
        let url = self.authenticated_url(method, extra)?;
        subsonic_json::<SubsonicEmpty>(self.client.get(url))
            .await
            .map(|_| ())
    }

    async fn similar_songs(&self, raw_id: &str, count: usize) -> SourceResult<Vec<Track>> {
        let body: SimilarSongsBody = self
            .get_json(
                "getSimilarSongs",
                &[
                    ("id", raw_id.to_string()),
                    ("count", count.clamp(1, 500).to_string()),
                ],
            )
            .await?;
        Ok(body
            .similar_songs
            .map(|songs| songs.song)
            .unwrap_or_default()
            .into_iter()
            .map(|song| track_from_dto(self, song))
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

#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct SubsonicEmpty {}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicEnvelope {
    #[serde(rename = "subsonic-response")]
    pub(super) response: SubsonicResponse,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicResponse {
    pub(super) status: String,
    #[serde(default, rename = "type")]
    pub(super) server_type: Option<String>,
    #[serde(default)]
    pub(super) error: Option<SubsonicError>,
    #[serde(flatten)]
    pub(super) body: serde_json::Map<String, serde_json::Value>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicError {
    #[serde(default)]
    pub(super) code: Option<u16>,
    #[serde(default)]
    pub(super) message: Option<String>,
    #[serde(default, rename = "helpUrl")]
    pub(super) help_url: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct AuthenticateBody {
    pub(super) user: SubsonicUser,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct TokenInfoBody {
    #[serde(rename = "tokenInfo")]
    pub(super) token_info: TokenInfo,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct TokenInfo {
    pub(super) username: String,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicUser {
    pub(super) username: String,
    #[serde(default, rename = "adminRole")]
    pub(super) admin_role: bool,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct ScanStatusBody {
    #[serde(rename = "scanStatus")]
    pub(super) scan_status: ScanStatus,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct ScanStatus {
    pub(super) scanning: bool,
    #[serde(default)]
    pub(super) count: i64,
    #[serde(default, rename = "folderCount")]
    pub(super) folder_count: Option<i64>,
    #[serde(default, rename = "lastScan")]
    pub(super) last_scan: Option<String>,
    #[serde(default)]
    pub(super) error: Option<String>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct AlbumListBody {
    #[serde(default, rename = "albumList2")]
    pub(super) album_list: AlbumList,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct AlbumBody {
    #[serde(default)]
    pub(super) album: AlbumDetail,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct AlbumDetail {
    #[serde(default)]
    pub(super) song: Vec<SubsonicSong>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct ArtistBody {
    #[serde(default)]
    pub(super) artist: ArtistDetail,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct ArtistDetail {
    #[serde(default)]
    pub(super) album: Vec<SubsonicAlbum>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct AlbumList {
    #[serde(default)]
    pub(super) album: Vec<SubsonicAlbum>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct SearchBody {
    #[serde(default, rename = "searchResult3")]
    pub(super) search_result: Option<SearchResult>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct SearchResult {
    #[serde(default)]
    pub(super) artist: Option<Vec<SubsonicArtist>>,
    #[serde(default)]
    pub(super) album: Option<Vec<SubsonicAlbum>>,
    #[serde(default)]
    pub(super) song: Option<Vec<SubsonicSong>>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct MusicFoldersBody {
    #[serde(default, rename = "musicFolders")]
    pub(super) music_folders: MusicFolders,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct MusicFolders {
    #[serde(default, rename = "musicFolder")]
    pub(super) music_folder: Vec<SubsonicMusicFolder>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicMusicFolder {
    pub(super) id: SubsonicId,
    pub(super) name: String,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct IndexesBody {
    #[serde(default)]
    pub(super) indexes: Option<ArtistsIndex>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct MusicDirectoryBody {
    pub(super) directory: SubsonicDirectory,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicDirectory {
    #[serde(default)]
    pub(super) child: Vec<SubsonicSong>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct ArtistsIndex {
    #[serde(default)]
    pub(super) index: Vec<ArtistIndex>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct ArtistIndex {
    #[serde(default)]
    pub(super) artist: Vec<SubsonicArtist>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct GenresBody {
    pub(super) genres: GenresList,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct GenresList {
    #[serde(default)]
    pub(super) genre: Vec<SubsonicGenre>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct PlaylistsBody {
    #[serde(default)]
    pub(super) playlists: Option<PlaylistsList>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct PlaylistsList {
    #[serde(default)]
    pub(super) playlist: Vec<SubsonicPlaylist>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct PlaylistBody {
    pub(super) playlist: SubsonicPlaylist,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SongBody {
    pub(super) song: SubsonicSong,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct RandomSongsBody {
    #[serde(default, rename = "randomSongs")]
    pub(super) random_songs: Option<SongsList>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct SimilarSongsBody {
    #[serde(default, rename = "similarSongs")]
    pub(super) similar_songs: Option<SongsList>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SongsList {
    #[serde(default)]
    pub(super) song: Vec<SubsonicSong>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct LyricsBody {
    #[serde(default)]
    pub(super) lyrics: Option<SubsonicLyrics>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct OpenSubsonicExtensionsBody {
    #[serde(default, rename = "openSubsonicExtensions")]
    pub(super) open_subsonic_extensions: Vec<OpenSubsonicExtensionDto>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct OpenSubsonicExtensionDto {
    pub(super) name: String,
    #[serde(default)]
    pub(super) versions: Vec<u32>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct StructuredLyricsBody {
    #[serde(default, rename = "lyricsList")]
    pub(super) lyrics_list: StructuredLyricsListDto,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct StructuredLyricsListDto {
    #[serde(default, rename = "structuredLyrics")]
    pub(super) structured_lyrics: Vec<StructuredLyricsDto>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct StructuredLyricsDto {
    pub(super) lang: String,
    #[serde(default)]
    pub(super) line: Vec<StructuredLyricLineDto>,
    #[serde(default)]
    pub(super) offset: Option<i64>,
    #[serde(default)]
    pub(super) kind: Option<String>,
    #[serde(default)]
    pub(super) agents: Vec<StructuredLyricAgentDto>,
    #[serde(default, rename = "cueLine")]
    pub(super) cue_line: Vec<StructuredLyricCueLineDto>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct StructuredLyricLineDto {
    pub(super) value: String,
    #[serde(default)]
    pub(super) start: Option<u64>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct StructuredLyricAgentDto {
    pub(super) id: String,
    pub(super) role: String,
    #[serde(default)]
    pub(super) name: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct StructuredLyricCueLineDto {
    pub(super) index: usize,
    pub(super) value: String,
    #[serde(default)]
    pub(super) start: Option<u64>,
    #[serde(default)]
    pub(super) end: Option<u64>,
    #[serde(default, rename = "agentId")]
    pub(super) agent_id: Option<String>,
    #[serde(default)]
    pub(super) cue: Vec<StructuredLyricCueDto>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct StructuredLyricCueDto {
    pub(super) value: String,
    pub(super) start: u64,
    #[serde(default)]
    pub(super) end: Option<u64>,
    #[serde(rename = "byteStart")]
    pub(super) byte_start: usize,
    #[serde(rename = "byteEnd")]
    pub(super) byte_end: usize,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicLyrics {
    #[serde(default)]
    pub(super) value: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicAlbum {
    pub(super) id: SubsonicId,
    #[serde(default)]
    pub(super) album: Option<String>,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) artist: Option<String>,
    #[serde(default, rename = "displayArtist")]
    pub(super) display_artist: Option<String>,
    #[serde(default, rename = "artistId")]
    pub(super) artist_id: Option<SubsonicId>,
    #[serde(default)]
    pub(super) artists: Vec<SubsonicArtistRef>,
    #[serde(default, rename = "coverArt")]
    pub(super) cover_art: Option<SubsonicId>,
    #[serde(default)]
    pub(super) year: Option<i32>,
    #[serde(default, rename = "releaseDate")]
    pub(super) release_date: Option<SubsonicItemDate>,
    #[serde(default)]
    pub(super) created: Option<String>,
    #[serde(default)]
    pub(super) played: Option<String>,
    #[serde(default, rename = "playCount")]
    pub(super) play_count: Option<u64>,
    #[serde(default, rename = "userRating")]
    pub(super) user_rating: Option<u32>,
    #[serde(default)]
    pub(super) genre: Option<String>,
    #[serde(default)]
    pub(super) genres: Vec<GenreName>,
    #[serde(default, rename = "releaseTypes")]
    pub(super) release_types: Vec<String>,
    #[serde(default, rename = "isCompilation")]
    pub(super) is_compilation: Option<bool>,
    #[serde(default, rename = "musicBrainzId")]
    pub(super) musicbrainz_album_id: Option<String>,
    #[serde(default)]
    pub(super) starred: Option<serde_json::Value>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicSong {
    pub(super) id: SubsonicId,
    #[serde(default, rename = "isDir")]
    pub(super) is_dir: Option<bool>,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) album: Option<String>,
    #[serde(default, rename = "albumId")]
    pub(super) album_id: Option<SubsonicId>,
    #[serde(default)]
    pub(super) artist: Option<String>,
    #[serde(default, rename = "displayArtist")]
    pub(super) display_artist: Option<String>,
    #[serde(default, rename = "artistId")]
    pub(super) artist_id: Option<SubsonicId>,
    #[serde(default)]
    pub(super) artists: Vec<SubsonicArtistRef>,
    #[serde(default, rename = "albumArtists")]
    pub(super) album_artists: Vec<SubsonicArtistRef>,
    #[serde(default, rename = "coverArt")]
    pub(super) cover_art: Option<SubsonicId>,
    #[serde(default)]
    pub(super) duration: Option<u32>,
    #[serde(default)]
    pub(super) track: Option<i32>,
    #[serde(default)]
    pub(super) year: Option<i32>,
    #[serde(default)]
    pub(super) created: Option<String>,
    #[serde(default)]
    pub(super) played: Option<String>,
    #[serde(default, rename = "playCount")]
    pub(super) play_count: Option<u64>,
    #[serde(default, rename = "userRating")]
    pub(super) user_rating: Option<u32>,
    #[serde(default)]
    pub(super) genre: Option<String>,
    #[serde(default)]
    pub(super) comment: Option<String>,
    #[serde(default)]
    pub(super) genres: Vec<GenreName>,
    #[serde(default)]
    pub(super) moods: Vec<String>,
    #[serde(default)]
    pub(super) bpm: Option<u32>,
    #[serde(default, rename = "discNumber")]
    pub(super) disc_number: Option<i32>,
    #[serde(default)]
    pub(super) path: Option<String>,
    #[serde(default)]
    pub(super) suffix: Option<String>,
    #[serde(default, rename = "contentType")]
    pub(super) content_type: Option<String>,
    #[serde(default)]
    pub(super) starred: Option<serde_json::Value>,
    #[serde(default, rename = "musicBrainzId")]
    pub(super) musicbrainz_recording_id: Option<String>,
    #[serde(default, rename = "replayGain")]
    pub(super) replay_gain: SubsonicReplayGain,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct SubsonicReplayGain {
    #[serde(default, rename = "trackGain")]
    pub(super) track_gain: Option<f64>,
    #[serde(default, rename = "albumGain")]
    pub(super) album_gain: Option<f64>,
    #[serde(default, rename = "trackPeak")]
    pub(super) track_peak: Option<f64>,
    #[serde(default, rename = "albumPeak")]
    pub(super) album_peak: Option<f64>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicArtist {
    pub(super) id: SubsonicId,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default, rename = "coverArt")]
    pub(super) cover_art: Option<SubsonicId>,
    #[serde(default)]
    pub(super) played: Option<String>,
    #[serde(default, rename = "playCount")]
    pub(super) play_count: Option<u64>,
    #[serde(default, rename = "userRating")]
    pub(super) user_rating: Option<u32>,
    #[serde(default)]
    pub(super) starred: Option<serde_json::Value>,
    #[serde(default, rename = "musicBrainzId")]
    pub(super) musicbrainz_artist_id: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicGenre {
    #[serde(default, alias = "name")]
    pub(super) value: String,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicPlaylist {
    pub(super) id: SubsonicId,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default, rename = "coverArt")]
    pub(super) cover_art: Option<SubsonicId>,
    #[serde(default)]
    pub(super) entry: Option<Vec<SubsonicSong>>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct GenreName {
    pub(super) name: String,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SubsonicArtistRef {
    pub(super) id: SubsonicId,
    pub(super) name: String,
}
#[derive(Clone, Copy, Debug, Deserialize)]
pub(super) struct SubsonicItemDate {
    #[serde(default)]
    pub(super) year: i32,
    #[serde(default)]
    pub(super) month: i32,
    #[serde(default)]
    pub(super) day: i32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SubsonicId(pub(super) String);
impl<'de> Deserialize<'de> for SubsonicId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(SubsonicIdVisitor)
    }
}
pub(super) struct SubsonicIdVisitor;
impl Visitor<'_> for SubsonicIdVisitor {
    type Value = SubsonicId;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a string or numeric Subsonic id")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(SubsonicId(value.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(SubsonicId(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(SubsonicId(value.to_string()))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(SubsonicId(value.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{SubsonicCredential, SubsonicSong, redacted_subsonic_url};
    use crate::subsonic::{
        SubsonicAuthentication, SubsonicFlavor, SubsonicSource, SubsonicSourceConfig,
    };
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

    #[test]
    fn opensubsonic_song_reads_replay_gain() {
        let song = serde_json::from_value::<SubsonicSong>(serde_json::json!({
            "id": "track-one",
            "replayGain": {
                "trackGain": -4.25,
                "albumGain": -3.5,
                "trackPeak": 0.91,
                "albumPeak": 0.95
            }
        }))
        .expect("OpenSubsonic song");

        assert_eq!(song.replay_gain.track_gain, Some(-4.25));
        assert_eq!(song.replay_gain.album_gain, Some(-3.5));
        assert_eq!(song.replay_gain.track_peak, Some(0.91));
        assert_eq!(song.replay_gain.album_peak, Some(0.95));
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
