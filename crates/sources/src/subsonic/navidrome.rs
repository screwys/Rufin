//! Navidrome's private library supplement to the OpenSubsonic source.
//!
//! Standard server operations stay in the OpenSubsonic client. This module
//! owns the password login, rotating UI token, and richer album, track, and
//! artist records used during a Navidrome library refresh.

use std::fmt;
use std::future::Future;

mod events;

use reqwest::Url;
use reqwest::header::HeaderName;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::policy::normalized_date;
use crate::remote_http::{self, BodyLimit, RemoteHttpPolicy};
use crate::remote_json::{boolean, field, id, items, strings};
use crate::source::{SourceReadProgress, SourceReadStage};
use crate::{SourceError, SourceResult};

use super::{ImageRef, SubsonicSource, normalize_release_types};

const NAVIDROME_PAGE_SIZE: usize = 1_000;
const NAVIDROME_JSON_MAX_BYTES: usize = 16 * 1024 * 1024;
const NAVIDROME_LOGIN_MAX_BYTES: usize = 64 * 1024;
const NAVIDROME_AUTH_HEADER: HeaderName = HeaderName::from_static("x-nd-authorization");
const NAVIDROME_HTTP: RemoteHttpPolicy = RemoteHttpPolicy {
    service: "navidrome",
    auth_context: "Navidrome returned",
    error_body: BodyLimit {
        max_bytes: 64 * 1024,
        context: "Navidrome error response",
    },
    redact_error_url: None,
};

pub(super) fn build_client(trust_invalid_cert: bool) -> SourceResult<reqwest::Client> {
    remote_http::build_client(
        trust_invalid_cert,
        crate::remote_http::RemoteTimeouts {
            connect: std::time::Duration::from_secs(10),
            request: std::time::Duration::from_secs(60),
        },
        NAVIDROME_HTTP,
    )
}

#[derive(Serialize)]
struct NavidromeLoginRequest<'a> {
    username: &'a str,
    password: &'a str,
}

pub(super) struct NavidromeSession(Mutex<Option<String>>);

impl Default for NavidromeSession {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl fmt::Debug for NavidromeSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NavidromeSession")
            .field("token", &"<redacted>")
            .finish()
    }
}

impl SubsonicSource {
    pub(super) async fn navidrome_radio_tracks(
        &self,
        seed: &crate::SourceRadioSeed,
        limit: usize,
    ) -> SourceResult<Option<Vec<String>>> {
        if self.flavor != super::SubsonicFlavor::Navidrome {
            return Ok(None);
        }
        let result = async {
            let raw = match seed {
                crate::SourceRadioSeed::Playlist(id) => crate::policy::raw_item_id(id).to_string(),
                crate::SourceRadioSeed::Genre(name)
                    if self.credential.navidrome_password().is_some() =>
                {
                    self.navidrome_genre_id(self.genre_name(name)).await?
                }
                _ => return Ok(None),
            };
            self.similar_songs(&raw, limit).await.map(Some)
        }
        .await;
        match result {
            // Older Navidrome versions do not accept playlist or genre seeds.
            Err(SourceError::NotFound) => Ok(None),
            result => result,
        }
    }

    async fn navidrome_genre_id(&self, name: &str) -> SourceResult<String> {
        let mut offset = 0;
        loop {
            let page: Vec<Value> = self
                .navidrome_json(
                    "genre",
                    &[
                        ("name", name.to_string()),
                        ("_start", offset.to_string()),
                        ("_end", (offset + NAVIDROME_PAGE_SIZE).to_string()),
                        ("_sort", "id".to_string()),
                        ("_order", "ASC".to_string()),
                    ],
                )
                .await?;
            if let Some(raw) = page
                .iter()
                .filter(|genre| genre["name"].as_str() == Some(name))
                .find_map(|genre| id(&genre["id"]))
            {
                return Ok(raw);
            }
            if page.len() < NAVIDROME_PAGE_SIZE {
                return Err(SourceError::NotFound);
            }
            offset += page.len();
        }
    }

    async fn navidrome_items(&self, kind: &str, values: &[Value]) -> SourceResult<Vec<Value>> {
        let ids: Vec<_> = values.iter().filter_map(|value| id(&value["id"])).collect();
        let mut result = Vec::with_capacity(ids.len());
        for ids in ids.chunks(100) {
            let mut query = vec![("_start", "0".to_string()), ("_end", ids.len().to_string())];
            query.extend(ids.iter().map(|id| ("id", id.clone())));
            let page: Vec<Value> = self.navidrome_json(kind, &query).await?;
            let mut page: std::collections::HashMap<_, _> = page
                .into_iter()
                .filter_map(|value| Some((id(&value["id"])?, value)))
                .collect();
            result.extend(ids.iter().filter_map(|id| page.remove(id)));
        }
        Ok(result)
    }

    pub(super) async fn read_albums(
        &self,
        source_id: &str,
        values: &[Value],
    ) -> SourceResult<Vec<library::AlbumObservation>> {
        if self.has_navidrome_library() {
            self.navidrome_items("album", values)
                .await?
                .iter()
                .filter_map(|value| {
                    Some(album_from_navidrome(
                        self,
                        source_id,
                        &id(&value["id"])?,
                        value,
                    ))
                })
                .collect::<library::LibraryResult<Vec<_>>>()
                .map_err(Into::into)
        } else {
            values
                .iter()
                .filter_map(|value| super::album_from_json(self, source_id, value).transpose())
                .collect::<library::LibraryResult<Vec<_>>>()
                .map_err(Into::into)
        }
    }

    pub(super) async fn read_tracks(
        &self,
        source_id: &str,
        values: &[Value],
    ) -> SourceResult<Vec<library::TrackObservation>> {
        if self.has_navidrome_library() {
            Ok(self
                .navidrome_items("song", values)
                .await?
                .iter()
                .filter_map(|value| Some(track_from_navidrome(self, &id(&value["id"])?, value)))
                .collect())
        } else {
            values
                .iter()
                .filter_map(|value| super::track_from_json(self, source_id, value).transpose())
                .collect::<library::LibraryResult<Vec<_>>>()
                .map_err(Into::into)
        }
    }

    pub(super) async fn read_artists(
        &self,
        source_id: &str,
        values: &[Value],
    ) -> SourceResult<Vec<library::ArtistObservation>> {
        if self.has_navidrome_library() {
            self.navidrome_items("artist", values)
                .await?
                .iter()
                .filter_map(|value| {
                    Some(artist_from_navidrome(
                        self,
                        source_id,
                        &id(&value["id"])?,
                        value,
                    ))
                })
                .collect::<library::LibraryResult<Vec<_>>>()
                .map_err(Into::into)
        } else {
            values
                .iter()
                .filter_map(|value| super::artist_from_json(self, source_id, value).transpose())
                .collect::<library::LibraryResult<Vec<_>>>()
                .map_err(Into::into)
        }
    }

    pub(crate) fn has_navidrome_library(&self) -> bool {
        self.navidrome_library
    }

    pub(super) async fn acquire_navidrome_library(
        &self,
        catalog: &mut library::Catalog<'_>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let source_id = catalog.source_id;
        let stamp = catalog.observed;

        use crate::SourceEntityKind::{Album, Artist, Track};
        for (kind, endpoint, stage) in [
            (Album, "album", SourceReadStage::Albums),
            (Track, "song", SourceReadStage::Tracks),
            (Artist, "artist", SourceReadStage::Artists),
        ] {
            progress(SourceReadProgress {
                stage,
                completed: 0,
                total: None,
            });
            let mut offset = 0usize;
            loop {
                if cancelled() {
                    return Err(SourceError::Cancelled);
                }
                let page = self.navidrome_page(endpoint, offset).await?;
                let page_len = page.len();
                if page_len == 0 {
                    break;
                }
                offset = offset.checked_add(page_len).ok_or_else(|| {
                    SourceError::Other(format!("Navidrome {endpoint} offset overflowed"))
                })?;
                let mut batch = library::CatalogBatch::default();
                for item in &page {
                    let Some(id) = id(&item["id"]) else {
                        continue;
                    };
                    match kind {
                        Album => batch
                            .albums
                            .push(album_from_navidrome(self, source_id, &id, item)?),
                        Track => batch.tracks.push(track_from_navidrome(self, &id, item)),
                        Artist => batch
                            .artists
                            .push(artist_from_navidrome(self, source_id, &id, item)?),
                    }
                }
                if cancelled() {
                    return Err(crate::SourceError::Cancelled);
                }
                catalog.apply_catalog(&batch, true).await?;
                progress(SourceReadProgress {
                    stage,
                    completed: offset,
                    total: None,
                });
                if page_len < NAVIDROME_PAGE_SIZE {
                    break;
                }
            }
            if let Some(stamp) = stamp {
                if cancelled() {
                    return Err(crate::SourceError::Cancelled);
                }
                catalog
                    .complete_source_observation(stamp, &[kind.as_str()], None)
                    .await?;
            }
        }
        Ok(())
    }

    async fn navidrome_page(&self, kind: &str, offset: usize) -> SourceResult<Vec<Value>> {
        let end = offset.checked_add(NAVIDROME_PAGE_SIZE).ok_or_else(|| {
            SourceError::Other("Navidrome library page offset overflowed".to_string())
        })?;
        self.navidrome_json(
            kind,
            &[
                ("_start", offset.to_string()),
                ("_end", end.to_string()),
                ("_sort", "id".to_string()),
                ("_order", "ASC".to_string()),
                ("missing", "false".to_string()),
            ],
        )
        .await
    }

    async fn navidrome_json<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        query: &[(&str, String)],
    ) -> SourceResult<T> {
        let mut url = navidrome_endpoint(&self.base_url, &format!("api/{endpoint}"))?;
        url.query_pairs_mut()
            .extend_pairs(query.iter().map(|(key, value)| (*key, value.as_str())));
        let url = &url;
        self.navidrome_authenticated(|token| async move {
            remote_http::json_with_header(
                self.navidrome_client
                    .get(url.clone())
                    .header(&NAVIDROME_AUTH_HEADER, format!("Bearer {token}")),
                NAVIDROME_HTTP,
                BodyLimit {
                    max_bytes: NAVIDROME_JSON_MAX_BYTES,
                    context: "Navidrome JSON response",
                },
                &NAVIDROME_AUTH_HEADER,
            )
            .await
        })
        .await
    }

    async fn navidrome_authenticated<T, F>(
        &self,
        mut send: impl FnMut(String) -> F,
    ) -> SourceResult<T>
    where
        F: Future<Output = SourceResult<(T, Option<String>)>>,
    {
        let mut retried = false;
        loop {
            let token = self.navidrome_token().await?;
            let response = send(token.clone()).await;
            match response {
                Ok((body, rotated)) => {
                    if let Some(rotated) = rotated.filter(|value| !value.trim().is_empty()) {
                        *self.navidrome_session.0.lock().await = Some(rotated);
                    }
                    return Ok(body);
                }
                Err(SourceError::Auth(_)) if !retried => {
                    let mut current = self.navidrome_session.0.lock().await;
                    if current.as_deref() == Some(token.as_str()) {
                        *current = None;
                    }
                    retried = true;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn supports_playlist_artwork(&self) -> bool {
        self.flavor == super::SubsonicFlavor::Navidrome
            && self.credential.navidrome_password().is_some()
    }

    pub async fn change_playlist_artwork(
        &self,
        object: &str,
        change: &crate::ArtworkChange,
    ) -> SourceResult<()> {
        if !self.supports_playlist_artwork() {
            return Err(SourceError::InvalidRequest(
                "This server cannot edit playlist artwork",
            ));
        }
        let raw = crate::policy::raw_item_id(object);
        let url = navidrome_endpoint(&self.base_url, &format!("api/playlist/{raw}/image"))?;
        let url = &url;
        self.navidrome_authenticated(|token| async move {
            let request = match change {
                crate::ArtworkChange::Replace(image) => {
                    let part = reqwest::multipart::Part::bytes(image.bytes.clone())
                        .file_name("artwork")
                        .mime_str(
                            image
                                .content_type
                                .as_deref()
                                .unwrap_or("application/octet-stream"),
                        )
                        .map_err(|error| SourceError::Other(error.to_string()))?;
                    self.navidrome_client
                        .post(url.clone())
                        .multipart(reqwest::multipart::Form::new().part("image", part))
                }
                crate::ArtworkChange::Remove => self.navidrome_client.delete(url.clone()),
            }
            .header(&NAVIDROME_AUTH_HEADER, format!("Bearer {token}"));
            remote_http::unit_with_header(request, NAVIDROME_HTTP, &NAVIDROME_AUTH_HEADER)
                .await
                .map(|rotated| ((), rotated))
        })
        .await
    }

    async fn navidrome_token(&self) -> SourceResult<String> {
        let mut token = self.navidrome_session.0.lock().await;
        if let Some(token) = token.as_ref() {
            return Ok(token.clone());
        }
        let password = self.credential.navidrome_password().ok_or_else(|| {
            SourceError::InvalidConfig(
                "saved Navidrome credentials cannot use its private library API".to_string(),
            )
        })?;
        let login = navidrome_login(
            &self.navidrome_client,
            &self.base_url,
            &self.username,
            password,
        )
        .await?;
        let next = required(
            field(&login, "token").unwrap_or_default(),
            "Navidrome session token",
        )?;
        *token = Some(next.clone());
        Ok(next)
    }
}

async fn navidrome_login(
    client: &reqwest::Client,
    base_url: &Url,
    username: &str,
    password: &str,
) -> SourceResult<Value> {
    let url = navidrome_endpoint(base_url, "auth/login")?;
    remote_http::json(
        client
            .post(url)
            .json(&NavidromeLoginRequest { username, password }),
        NAVIDROME_HTTP,
        BodyLimit {
            max_bytes: NAVIDROME_LOGIN_MAX_BYTES,
            context: "Navidrome login response",
        },
    )
    .await
}

fn navidrome_endpoint(base_url: &Url, endpoint: &str) -> SourceResult<Url> {
    let mut url = base_url.clone();
    let base_path = base_url.path().trim_end_matches('/');
    let endpoint = endpoint.trim_start_matches('/');
    let path = if base_path.is_empty() {
        format!("/{endpoint}")
    } else {
        format!("{base_path}/{endpoint}")
    };
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn required(value: String, name: &str) -> SourceResult<String> {
    clean(value).ok_or_else(|| SourceError::Other(format!("{name} is missing")))
}

fn clean(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(clean)
}

fn item_release_date(item: &Value) -> Option<String> {
    ["releaseDate", "date", "originalDate"]
        .into_iter()
        .find_map(|key| normalized_date(field(item, key)))
}

fn album_from_navidrome(
    source: &SubsonicSource,
    source_id: &str,
    raw_id: &str,
    album: &Value,
) -> library::LibraryResult<library::AlbumObservation> {
    let mut album_artists = participant_credits(
        source,
        &album["participants"],
        "albumartist",
        &id(&album["albumArtistId"]).unwrap_or_default(),
        field::<String>(album, "mbzAlbumArtistId").as_deref(),
    );
    let artist = clean_optional(field(album, "albumArtist"))
        .or_else(|| clean_optional(field(album, "artist")))
        .or_else(|| super::joined_artist_names(&album_artists))
        .unwrap_or_else(|| "Unknown Artist".to_string());
    if album_artists.is_empty() {
        album_artists = artist_credit(
            source,
            id(&album["albumArtistId"]).unwrap_or_default(),
            artist.clone(),
            field(album, "mbzAlbumArtistId"),
            field(album, "sortAlbumArtistName"),
        )
        .into_iter()
        .collect();
    }
    let release_date = item_release_date(album);
    let year = positive_u16(field(album, "maxYear"))
        .or_else(|| {
            release_date
                .as_deref()
                .and_then(|date| date.get(..4))
                .and_then(|year| year.parse().ok())
        })
        .unwrap_or_default();
    let title =
        clean_optional(field(album, "name")).unwrap_or_else(|| "Untitled Album".to_string());
    Ok(library::AlbumObservation {
        metadata: library::AlbumMetadata {
            sort_text: clean_optional(field(album, "sortAlbumName"))
                .unwrap_or_else(|| title.clone())
                .to_lowercase(),
            title: title,
            artist: artist,
            year: Some(i64::from(year)).filter(|year| *year > 0),
            release_date: release_date,
            date_added: normalized_date(field(album, "createdAt")),
            is_compilation: Some(boolean(&album["compilation"]).unwrap_or_default()),
            musicbrainz_release_id: clean_optional(field(album, "mbzAlbumId")),
            musicbrainz_release_group_id: clean_optional(field(album, "mbzReleaseGroupId")),
            album_artists: album_artists,
            genres: genre_credits(source, &album["genres"]),
            release_types: normalize_release_types(tag_values(&album["tags"], "releasetype")),
            ..Default::default()
        },
        user_data: library::NativeUserState {
            rating: Some(rating(field(album, "rating")).map(|rating| i64::from(rating) * 10)),
            favorite: boolean(&album["starred"]),
            ..Default::default()
        },
        object_id: source.id("album", raw_id),
        artwork_binding: Some(crate::native_artwork_binding(
            source_id,
            &navidrome_image_ref(
                source,
                "al",
                raw_id,
                clean_optional(field(album, "updatedAt")),
            ),
        )?),
        ..Default::default()
    })
}

fn track_from_navidrome(
    source: &SubsonicSource,
    raw_id: &str,
    track: &Value,
) -> library::TrackObservation {
    let mut artists = participant_credits(
        source,
        &track["participants"],
        "artist",
        &id(&track["artistId"]).unwrap_or_default(),
        field::<String>(track, "mbzArtistId").as_deref(),
    );
    let artist = clean_optional(field(track, "artist"))
        .or_else(|| super::joined_artist_names(&artists))
        .unwrap_or_else(|| "Unknown Artist".to_string());
    if artists.is_empty() {
        artists = artist_credit(
            source,
            id(&track["artistId"]).unwrap_or_default(),
            artist.clone(),
            field(track, "mbzArtistId"),
            field(track, "sortArtistName"),
        )
        .into_iter()
        .collect();
    }
    let mut album_artists = participant_credits(
        source,
        &track["participants"],
        "albumartist",
        &id(&track["albumArtistId"]).unwrap_or_default(),
        field::<String>(track, "mbzAlbumArtistId").as_deref(),
    );
    let album_artist = clean_optional(field(track, "albumArtist"))
        .or_else(|| super::joined_artist_names(&album_artists))
        .unwrap_or_else(|| artist.clone());
    if album_artists.is_empty() {
        album_artists = artist_credit(
            source,
            id(&track["albumArtistId"]).unwrap_or_default(),
            album_artist,
            field(track, "mbzAlbumArtistId"),
            field(track, "sortAlbumArtistName"),
        )
        .into_iter()
        .collect();
    }
    let album_id = id(&track["albumId"]).map(|id| String::from(source.id("album", &id)));
    let object_id = source.id("track", raw_id);
    let title =
        clean_optional(field(track, "title")).unwrap_or_else(|| "Untitled Track".to_string());
    let source_path = server_path(
        field::<String>(track, "libraryPath").as_deref(),
        &field::<String>(track, "path").unwrap_or_default(),
    );
    let source_format = clean_optional(field(track, "suffix"));
    let duration_seconds = duration_seconds(field(track, "duration").unwrap_or_default());
    let loudness_analysis_key = super::audio_key(
        &object_id,
        source_path.as_deref(),
        source_format.as_deref(),
        duration_seconds,
    );
    library::TrackObservation {
        metadata: library::TrackMetadata {
            audio_properties: library::AudioProperties {
                bitrate_kbps: field(track, "bitRate"),
                sample_rate_hz: field(track, "sampleRate"),
                bit_depth: field(track, "bitDepth"),
                channels: field(track, "channels"),
            },
            sort_text: clean_optional(field(track, "sortTitle"))
                .unwrap_or_else(|| title.clone())
                .to_lowercase(),
            title: title,
            artist: artist,
            album: clean_optional(field(track, "album"))
                .unwrap_or_else(|| "Unknown Album".to_string()),
            year: positive_u16(field(track, "year")).map(i64::from),
            release_date: item_release_date(track),
            date_added: normalized_date(field(track, "createdAt")),
            duration_millis: i64::from(duration_seconds) * 1000,
            disc_number: i64::from(positive_u16(field(track, "discNumber")).unwrap_or_default()),
            track_number: i64::from(positive_u16(field(track, "trackNumber")).unwrap_or_default()),
            musicbrainz_recording_id: clean_optional(
                field(track, "mbzRecordingID").or_else(|| field(track, "mbzRecordingId")),
            ),
            musicbrainz_release_track_id: clean_optional(field(track, "mbzReleaseTrackId")),
            source_format: source_format,
            comment: clean_optional(field(track, "comment")),
            bpm: positive_u16(field(track, "bpm")).map(i64::from),
            source_loudness: field::<f64>(track, "rgTrackGain")
                .filter(|value| value.is_finite())
                .map(|gain| library::NativeGain {
                    replay_gain_db: Some(gain),
                    replay_gain_peak: field::<f64>(track, "rgTrackPeak")
                        .filter(|value| value.is_finite() && *value >= 0.0),
                    ..Default::default()
                }),
            album_loudness: field::<f64>(track, "rgAlbumGain")
                .filter(|value| value.is_finite())
                .map(|gain| library::NativeGain {
                    replay_gain_db: Some(gain),
                    replay_gain_peak: field::<f64>(track, "rgAlbumPeak")
                        .filter(|value| value.is_finite() && *value >= 0.0),
                    ..Default::default()
                }),
            artists: artists,
            album_artists: album_artists,
            genres: genre_credits(source, &track["genres"]),
            moods: super::moods_from_item(source, tag_values(&track["tags"], "mood")),
            ..Default::default()
        },
        user_data: library::NativeUserState {
            last_played: crate::policy::unix_seconds(field(track, "playDate")),
            play_count: capped_u32(field(track, "playCount")).map(i64::from),
            rating: Some(rating(field(track, "rating")).map(|rating| i64::from(rating) * 10)),
            favorite: boolean(&track["starred"]),
            ..Default::default()
        },
        object_id,
        album_object_id: album_id,
        loudness_analysis_key,
        source_path,
        folders: Some(
            field::<i64>(track, "libraryId")
                .filter(|library_id| *library_id > 0)
                .map(|library_id| source.id("music-folder", &library_id.to_string()))
                .into_iter()
                .collect(),
        ),
        ..Default::default()
    }
}

fn artist_from_navidrome(
    source: &SubsonicSource,
    source_id: &str,
    raw_id: &str,
    artist: &Value,
) -> library::LibraryResult<library::ArtistObservation> {
    let name =
        clean_optional(field(artist, "name")).unwrap_or_else(|| "Unknown Artist".to_string());
    Ok(library::ArtistObservation {
        metadata: library::ArtistMetadata {
            sort_text: Some(
                clean_optional(field(artist, "sortArtistName"))
                    .unwrap_or_else(|| name.clone())
                    .to_lowercase(),
            ),
            name: name,
            musicbrainz_artist_id: clean_optional(field(artist, "mbzArtistId")),
            ..Default::default()
        },
        user_data: library::NativeUserState {
            favorite: boolean(&artist["starred"]),
            rating: Some(rating(field(artist, "rating")).map(|rating| i64::from(rating) * 10)),
            ..Default::default()
        },
        object_id: source.id("artist", raw_id),
        artwork_binding: Some(crate::native_artwork_binding(
            source_id,
            &navidrome_image_ref(source, "ar", raw_id, None),
        )?),
    })
}

fn artist_credit(
    source: &SubsonicSource,
    raw_id: String,
    name: String,
    musicbrainz_artist_id: Option<String>,
    sort_name: Option<String>,
) -> Option<library::ArtistCredit> {
    clean(raw_id).map(|raw_id| library::ArtistCredit {
        sort_text: clean_optional(sort_name).map(|name| name.to_lowercase()),
        object_id: String::from(source.id("artist", &raw_id)),
        name,
        musicbrainz_artist_id: clean_optional(musicbrainz_artist_id),
        ..Default::default()
    })
}

fn participant_credits(
    source: &SubsonicSource,
    participants: &Value,
    role: &str,
    fallback_id: &str,
    fallback_musicbrainz_id: Option<&str>,
) -> Vec<library::ArtistCredit> {
    let mut credits = items(&participants[role])
        .iter()
        .filter_map(|participant| {
            let raw_id = id(&participant["id"])?;
            Some(library::ArtistCredit {
                sort_text: clean_optional(field::<String>(participant, "sortArtistName"))
                    .map(|name| name.to_lowercase()),
                object_id: String::from(source.id("artist", &raw_id)),
                name: clean_optional(field(participant, "name"))
                    .unwrap_or_else(|| "Unknown Artist".to_string()),
                musicbrainz_artist_id: clean_optional(field(participant, "mbzArtistId")),
                ..Default::default()
            })
        })
        .collect::<Vec<_>>();
    if let (Some(fallback_id), Some(musicbrainz_id)) = (
        clean(fallback_id.to_string()),
        fallback_musicbrainz_id.and_then(|value| clean(value.to_string())),
    ) && let Some(credit) = credits
        .iter_mut()
        .find(|credit| credit.object_id.as_str() == source.id("artist", &fallback_id))
        && credit.musicbrainz_artist_id.is_none()
    {
        credit.musicbrainz_artist_id = Some(musicbrainz_id);
    }
    credits
}

fn genre_credits(source: &SubsonicSource, genres: &Value) -> Vec<library::TagReference> {
    items(genres)
        .iter()
        .filter_map(|genre| clean_optional(field(genre, "name")))
        .map(|name| library::TagReference {
            object_id: String::from(source.id("genre", &name)),
            name,
            ..Default::default()
        })
        .collect()
}

fn tag_values(tags: &Value, name: &str) -> Vec<String> {
    strings(&tags[name])
}

fn navidrome_image_ref(
    source: &SubsonicSource,
    kind: &str,
    raw_id: &str,
    revision: Option<String>,
) -> ImageRef {
    ImageRef::new(source.id("cover", &format!("{kind}-{raw_id}_0")), revision)
}

fn server_path(library_path: Option<&str>, path: &str) -> Option<String> {
    let reported_library_path = library_path?.trim();
    let library_path = reported_library_path.trim_end_matches(['/', '\\']);
    let path = path.trim().trim_start_matches(['/', '\\']);
    if reported_library_path.is_empty() || path.is_empty() {
        return None;
    }
    if library_path.is_empty() {
        let root = reported_library_path
            .chars()
            .find(|character| matches!(character, '/' | '\\'))?;
        return Some(format!("{root}{path}"));
    }
    Some(format!("{library_path}/{path}"))
}

fn positive_u16(value: Option<i32>) -> Option<u16> {
    let value = value?;
    u16::try_from(value).ok().filter(|value| *value > 0)
}

fn capped_u32(value: Option<u64>) -> Option<u32> {
    value.map(|value| value.min(u64::from(u32::MAX)) as u32)
}

fn rating(value: Option<f64>) -> Option<u8> {
    value.and_then(|value| {
        (value.is_finite() && value > 0.0).then(|| (value * 2.0).round().clamp(1.0, 10.0) as u8)
    })
}

fn duration_seconds(value: f64) -> u32 {
    if value.is_finite() && value > 0.0 {
        value.round().clamp(0.0, f64::from(u32::MAX)) as u32
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::{
        SubsonicAuthentication, SubsonicCredential, SubsonicFlavor, SubsonicSourceConfig,
    };
    use super::*;

    #[test]
    fn native_and_opensubsonic_sort_names_keep_display_names_and_optional_credits() {
        let source = navidrome_source("http://localhost/");
        let album = album_from_navidrome(&source, "source", "album", &serde_json::json!({"name":"The Album", "sortAlbumName":"Album, The", "albumArtistId":"artist", "albumArtist":"The Artist", "sortAlbumArtistName":"Artist, The"})).unwrap();
        assert_eq!(album.metadata.title, "The Album");
        assert_eq!(album.metadata.sort_text, "album, the");
        assert_eq!(
            album.metadata.album_artists[0].sort_text.as_deref(),
            Some("artist, the")
        );
        let track = track_from_navidrome(
            &source,
            "track",
            &serde_json::json!({"title":"The Song", "sortTitle":"Song, The", "participants":{"artist":[{"id":"artist", "name":"The Artist", "sortArtistName":"Artist, The"}]}}),
        );
        assert_eq!(track.metadata.title, "The Song");
        assert_eq!(track.metadata.sort_text, "song, the");
        assert_eq!(
            track.metadata.artists[0].sort_text.as_deref(),
            Some("artist, the")
        );
        let artist = artist_from_navidrome(
            &source,
            "source",
            "artist",
            &serde_json::json!({"name":"The Artist", "sortArtistName":"Artist, The"}),
        )
        .unwrap();
        assert_eq!(artist.metadata.name, "The Artist");
        assert_eq!(artist.metadata.sort_text.as_deref(), Some("artist, the"));

        let value = serde_json::json!({"id":"1", "title":"The Name", "name":"The Name", "sortName":"Name, The", "artists":[{"id":"artist", "name":"The Artist", "sortName":"Artist, The"}]});
        let album = super::super::item::album_from_json(&source, "source", &value)
            .unwrap()
            .unwrap();
        assert_eq!(album.metadata.title, "The Name");
        assert_eq!(album.metadata.sort_text, "name, the");
        assert_eq!(
            album.metadata.album_artists[0].sort_text.as_deref(),
            Some("artist, the")
        );
        let track = super::super::item::track_from_json(&source, "source", &value)
            .unwrap()
            .unwrap();
        assert_eq!(track.metadata.title, "The Name");
        assert_eq!(track.metadata.sort_text, "name, the");
        let artist = super::super::item::artist_from_json(&source, "source", &value)
            .unwrap()
            .unwrap();
        assert_eq!(artist.metadata.name, "The Name");
        assert_eq!(artist.metadata.sort_text.as_deref(), Some("name, the"));
        let plain = serde_json::json!({"id":"1", "name":"Plain", "sortName":""});
        assert_eq!(
            super::super::item::album_from_json(&source, "source", &plain)
                .unwrap()
                .unwrap()
                .metadata
                .sort_text,
            "plain"
        );
        assert_eq!(
            super::super::item::track_from_json(&source, "source", &plain)
                .unwrap()
                .unwrap()
                .metadata
                .sort_text,
            "untitled track"
        );
        assert_eq!(
            super::super::item::artist_from_json(&source, "source", &plain)
                .unwrap()
                .unwrap()
                .metadata
                .sort_text
                .as_deref(),
            Some("plain")
        );
    }

    #[test]
    fn navidrome_file_path_combines_the_reported_library_and_relative_path() {
        assert_eq!(
            server_path(Some("/srv/navidrome/audio/"), "/Artist/Album/Track.flac").as_deref(),
            Some("/srv/navidrome/audio/Artist/Album/Track.flac")
        );
        assert_eq!(
            server_path(Some(r"D:\Navidrome\Audio\\"), r"\Artist\Album\Track.flac").as_deref(),
            Some(r"D:\Navidrome\Audio/Artist\Album\Track.flac")
        );
        assert_eq!(
            server_path(Some("/"), "Artist/Album/Track.flac").as_deref(),
            Some("/Artist/Album/Track.flac")
        );
        assert_eq!(server_path(None, "Track.flac"), None);
    }

    #[test]
    fn navidrome_track_reads_replay_gain_columns() {
        let track = serde_json::json!({
            "id": "track-one",
            "rgTrackGain": -4.25,
            "rgTrackPeak": 0.91,
            "rgAlbumGain": -3.5,
            "rgAlbumPeak": 0.95
        });

        let source = navidrome_source("http://localhost/");
        let track = track_from_navidrome(&source, "track-one", &track);
        assert_eq!(
            track
                .metadata
                .source_loudness
                .and_then(|value| value.replay_gain_db),
            Some(-4.25)
        );
        assert_eq!(
            track
                .metadata
                .source_loudness
                .and_then(|value| value.replay_gain_peak),
            Some(0.91)
        );
        assert_eq!(
            track
                .metadata
                .album_loudness
                .and_then(|value| value.replay_gain_db),
            Some(-3.5)
        );
        assert_eq!(
            track
                .metadata
                .album_loudness
                .and_then(|value| value.replay_gain_peak),
            Some(0.95)
        );
    }

    #[test]
    fn navidrome_album_and_artist_keep_the_identifiers_missing_from_opensubsonic() {
        let source = navidrome_source("http://localhost/");
        let album = serde_json::json!({
            "id": "album-one",
            "name": "Album",
            "albumArtist": "Album Artist",
            "albumArtistId": "artist-one",
            "maxYear": 2025,
            "releaseDate": "2025-04-03",
            "updatedAt": "2025-04-04T12:00:00Z",
            "mbzAlbumId": "11111111-1111-1111-1111-111111111111",
            "mbzReleaseGroupId": "22222222-2222-2222-2222-222222222222",
            "mbzAlbumArtistId": "33333333-3333-3333-3333-333333333333",
            "participants": {
                "albumartist": [{
                    "id": "artist-one",
                    "name": "Album Artist",
                    "mbzArtistId": "33333333-3333-3333-3333-333333333333"
                }],
                "artist": [{
                    "id": "guest-one",
                    "name": "Guest Artist"
                }]
            },
            "tags": {
                "releasetype": ["Album", "Live", "album"]
            }
        });
        let album = album_from_navidrome(&source, "source", "album-one", &album).unwrap();
        assert_eq!(
            album.metadata.musicbrainz_release_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(
            album.metadata.musicbrainz_release_group_id.as_deref(),
            Some("22222222-2222-2222-2222-222222222222")
        );
        assert_eq!(
            album.metadata.album_artists[0]
                .musicbrainz_artist_id
                .as_deref(),
            Some("33333333-3333-3333-3333-333333333333")
        );
        assert_eq!(album.metadata.release_date.as_deref(), Some("2025-04-03"));
        assert_eq!(album.metadata.release_types, ["album", "live"]);
        let binding: Vec<library::ArtworkCandidate> =
            serde_json::from_slice(album.artwork_binding.as_deref().unwrap()).unwrap();
        assert_eq!(
            binding[0].source_id().unwrap().clone(),
            crate::SourceId::new("source")
        );
        assert_eq!(
            match &binding[0].image {
                library::ArtworkImage::Native { item_id, .. } => item_id.as_str(),
                _ => panic!("native image"),
            },
            "navidrome:cover:al-album-one_0"
        );
        assert_eq!(
            match &binding[0].image {
                library::ArtworkImage::Native { tag, .. } => tag.as_deref(),
                _ => panic!("native image"),
            },
            Some("2025-04-04T12:00:00Z")
        );

        for (data, expected) in [
            (
                serde_json::json!({"releaseDate":"2020", "date":"2010-02", "originalDate":"1990"}),
                Some("2020"),
            ),
            (
                serde_json::json!({"releaseDate":"", "date":"2010-02", "originalDate":"1990"}),
                Some("2010-02"),
            ),
            (
                serde_json::json!({"originalDate":"1990-03-04"}),
                Some("1990-03-04"),
            ),
            (
                serde_json::json!({"releaseDate":"", "date":"", "originalDate":""}),
                None,
            ),
        ] {
            assert_eq!(
                album_from_navidrome(&source, "source", "dates", &data)
                    .unwrap()
                    .metadata
                    .release_date
                    .as_deref(),
                expected
            );
            assert_eq!(
                track_from_navidrome(&source, "dates", &data)
                    .metadata
                    .release_date
                    .as_deref(),
                expected
            );
        }

        let artist = serde_json::json!({
            "id": "artist-one",
            "name": "Album Artist",
            "mbzArtistId": "33333333-3333-3333-3333-333333333333"
        });
        let artist = artist_from_navidrome(&source, "source", "artist-one", &artist).unwrap();
        assert_eq!(
            artist.metadata.musicbrainz_artist_id.as_deref(),
            Some("33333333-3333-3333-3333-333333333333")
        );
        let binding: Vec<library::ArtworkCandidate> =
            serde_json::from_slice(artist.artwork_binding.as_deref().unwrap()).unwrap();
        assert_eq!(
            binding[0].source_id().unwrap().clone(),
            crate::SourceId::new("source")
        );
        assert_eq!(
            match &binding[0].image {
                library::ArtworkImage::Native { item_id, .. } => item_id.as_str(),
                _ => panic!("native image"),
            },
            "navidrome:cover:ar-artist-one_0"
        );
    }

    fn navidrome_source(base_url: &str) -> SubsonicSource {
        let credential = SubsonicCredential::from_navidrome_password("password").serialize();
        SubsonicSource::open(
            SubsonicFlavor::Navidrome,
            SubsonicSourceConfig {
                base_url: base_url.to_string(),
                username: "listener".to_string(),
                trust_invalid_cert: false,
                navidrome_library_version: super::super::NAVIDROME_LIBRARY_VERSION,
                authentication: SubsonicAuthentication::Password,
            },
            credential,
        )
        .expect("open Navidrome source")
    }

    pub(super) fn navidrome_source_with_token(server: &MockServer, token: &str) -> SubsonicSource {
        let mut source = navidrome_source(&server.uri());
        source.navidrome_session = NavidromeSession(Mutex::new(Some(token.to_string())));
        source
    }

    fn page_match(endpoint: &str, offset: usize, token: &str) -> wiremock::MockBuilder {
        Mock::given(method("GET"))
            .and(path(format!("/api/{endpoint}")))
            .and(header(
                "x-nd-authorization",
                format!("Bearer {token}").as_str(),
            ))
            .and(query_param("_start", offset.to_string()))
            .and(query_param(
                "_end",
                offset.saturating_add(NAVIDROME_PAGE_SIZE).to_string(),
            ))
            .and(query_param("_sort", "id"))
            .and(query_param("_order", "ASC"))
            .and(query_param("missing", "false"))
    }

    #[tokio::test]
    async fn native_metadata_survives_search_and_collection_reads_and_accepts_native_clears() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let server = MockServer::start().await;
        let cleared = Arc::new(AtomicBool::new(false));
        let response_cleared = cleared.clone();
        Mock::given(method("GET"))
            .and(path("/api/song"))
            .respond_with(move |_: &wiremock::Request| {
                let cleared = response_cleared.load(Ordering::Relaxed);
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id":"track", "title":"Track", "albumId":"album", "album":"Album",
                    "libraryId":1, "duration":42, "starred":!cleared,
                    "releaseDate":if cleared { None } else { Some("2025-04-03") },
                    "mbzReleaseTrackId":if cleared { None } else { Some("release-track") }
                }]))
            })
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/album"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id":"album","name":"Album","mbzReleaseGroupId":"release-group"
                }])),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/artist"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/rest/search3.view"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response":{"status":"ok","searchResult3":{
                    "album":[{"id":"album","name":"Summary"}], "song":[{"id":"track","title":"Summary"}]
                }}
            }))).mount(&server).await;
        Mock::given(method("GET")).and(path("/rest/getAlbum.view"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subsonic-response":{"status":"ok","album":{"id":"album","name":"Summary","song":[{"id":"track","title":"Summary"}]}}
            }))).mount(&server).await;
        let source = navidrome_source_with_token(&server, "token");
        let source_owner = crate::Source::new(
            library::SourceId::new("source"),
            crate::Implementation::OpenSubsonic(source),
        );
        let crate::Implementation::OpenSubsonic(source) = &source_owner.implementation else {
            unreachable!()
        };
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let source_id = crate::SourceId::new("source");
        database
            .reconcile_source(&library::SourceId::new("source"), "source", "Source")
            .await
            .unwrap();
        let catalog_source_id = ("source").to_string();
        let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
        catalog.start_observation().await.unwrap();
        (async {
            let mut batch = library::CatalogBatch::default();
            let result = batch
                .folders
                .extend_from_slice(&[library::FolderObservation {
                    metadata: library::FolderMetadata {
                        name: Some("Folder".to_string()),
                        sort_text: Some("folder".to_string()),
                        ..Default::default()
                    },
                    object_id: "navidrome:music-folder:1".to_string(),
                    artwork_binding: None,
                    ..Default::default()
                }]);
            catalog.apply_catalog(&batch, true).await?;
            Ok::<_, library::LibraryError>(result)
        })
        .await
        .unwrap();
        {
            let result = source
                .acquire_navidrome_library(&mut catalog, &|_| {}, &|| false)
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
        for clear in [false, true] {
            cleared.store(clear, Ordering::Relaxed);
            source_owner
                .live_search(&database, "Track", 10)
                .await
                .unwrap();
            let catalog_source_id = ("source").to_string();
            let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
            {
                let result = source
                    .acquire_collection(
                        &mut catalog,
                        &crate::SourceCollection::Album("navidrome:album:album".into()),
                        false,
                        &|| false,
                    )
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
            let row = database
                .track(
                    &(&library::source_entity_uri(&source_id, "track", "navidrome:track:track")),
                    &library::ReadCancellation::new(),
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.metadata.title, "Track");
            assert_eq!(
                row.metadata.release_date.as_deref(),
                if clear { None } else { Some("2025-04-03") }
            );
            assert_eq!(
                row.metadata.musicbrainz_release_track_id.as_deref(),
                if clear { None } else { Some("release-track") }
            );
            assert_eq!(row.state.favorite, !clear);
        }
        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().any(|request| {
            request.url.path() == "/api/song"
                && request
                    .url
                    .query_pairs()
                    .any(|(key, value)| key == "id" && value == "track")
        }));
    }

    #[tokio::test]
    async fn saved_password_logs_in_only_when_the_navidrome_library_is_read() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "token-a"
            })))
            .expect(1)
            .mount(&server)
            .await;
        page_match("song", 0, "token-a")
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;
        let source = navidrome_source(&server.uri());

        source
            .navidrome_page("song", 0)
            .await
            .expect("Navidrome song page");
    }

    #[tokio::test]
    async fn pages_preserve_the_exact_bounded_offsets() {
        let server = MockServer::start().await;
        let first_page = (0..NAVIDROME_PAGE_SIZE)
            .map(|index| serde_json::json!({ "id": format!("artist-{index}") }))
            .collect::<Vec<_>>();
        page_match("artist", 0, "token-a")
            .respond_with(ResponseTemplate::new(200).set_body_json(first_page))
            .expect(1)
            .mount(&server)
            .await;
        page_match("artist", NAVIDROME_PAGE_SIZE, "token-a")
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": "artist-last"
                }])),
            )
            .expect(1)
            .mount(&server)
            .await;
        let source = navidrome_source_with_token(&server, "token-a");
        let first = source
            .navidrome_page("artist", 0)
            .await
            .expect("first Navidrome artist page");
        let second = source
            .navidrome_page("artist", NAVIDROME_PAGE_SIZE)
            .await
            .expect("second Navidrome artist page");

        assert_eq!(first.len(), NAVIDROME_PAGE_SIZE);
        assert_eq!(second.len(), 1);
    }

    #[tokio::test]
    async fn private_library_pages_publish_rich_navidrome_facts_through_scan() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "token-a"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let mut first_albums = vec![serde_json::json!({
            "id": "album-one",
            "name": "AAA Rich Album",
            "albumArtist": "Album Artist",
            "albumArtistId": "album-artist-one",
            "updatedAt": "2026-08-27T00:00:00Z",
            "mbzAlbumId": "release-one",
            "mbzReleaseGroupId": "release-group-one",
            "participants": {"albumartist": [{
                "id": "album-artist-one",
                "name": "Album Artist",
                "mbzArtistId": "album-artist-mbid"
            }]},
            "tags": {"releasetype": ["Album", "Live"]}
        })];
        first_albums.extend((1..NAVIDROME_PAGE_SIZE).map(|index| {
            serde_json::json!({
                "id": format!("album-{index}"),
                "name": format!("ZZZ Album {index}")
            })
        }));
        first_albums[1] = serde_json::json!({"name": "Missing ID"});
        page_match("album", 0, "token-a")
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-nd-authorization", "token-b")
                    .set_body_json(first_albums),
            )
            .expect(1)
            .mount(&server)
            .await;
        page_match("album", NAVIDROME_PAGE_SIZE, "token-b")
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": "album-last",
                    "name": "ZZZ Last Album"
                }])),
            )
            .expect(1)
            .mount(&server)
            .await;
        page_match("song", 0, "token-b")
            .respond_with(ResponseTemplate::new(200).set_body_json({
                let mut songs = vec![serde_json::json!({
                "id": "song-one",
                "albumId": "album-one",
                "title": "Track One",
                "artist": "Track Artist",
                "artistId": "track-artist-one",
                "album": "AAA Rich Album",
                "albumArtist": "Album Artist",
                "albumArtistId": "album-artist-one",
                "libraryId": 1,
                "libraryPath": "/srv/navidrome/audio",
                "path": "Artist/Album/Track.flac",
                "suffix": "flac",
                "duration": "181.4",
                "bpm": 123,
                "rating": {"unexpected": true},
                "playCount": [],
                "rgTrackGain": "-4.25",
                "rgTrackPeak": "invalid",
                "mbzRecordingID": "recording-one",
                "mbzReleaseTrackId": "release-track-one",
                "participants": {
                    "artist": [null, {"name": "Missing ID"}, {
                        "id": "track-artist-one",
                        "name": "Track Artist",
                        "mbzArtistId": "track-artist-mbid"
                    }],
                    "albumartist": [{
                        "id": "album-artist-one",
                        "name": "Album Artist",
                        "mbzArtistId": "album-artist-mbid"
                    }]
                },
                "genres": [null, {"name": 17}, {"name": "Rock"}],
                "tags": {"mood": ["Focused", null, 17]}
                })];
                songs.resize(NAVIDROME_PAGE_SIZE, serde_json::json!({"id": null}));
                songs
            }))
            .expect(1)
            .mount(&server)
            .await;
        page_match("song", NAVIDROME_PAGE_SIZE, "token-b")
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": 42,
                    "title": "ZZZ Last Track",
                    "duration": "invalid",
                    "participants": {"artist": "invalid"}
                }])),
            )
            .expect(1)
            .mount(&server)
            .await;
        page_match("artist", 0, "token-b")
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": "track-artist-one",
                    "name": "Track Artist",
                    "mbzArtistId": "track-artist-mbid"
                }])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let source = navidrome_source(&server.uri());
        let source_owner = crate::Source::new(
            library::SourceId::new("navidrome:test"),
            crate::Implementation::OpenSubsonic(source),
        );
        let crate::Implementation::OpenSubsonic(source) = &source_owner.implementation else {
            unreachable!()
        };
        let directory = tempfile::tempdir().expect("Library directory");
        let database = library::Database::open(directory.path().join("library.sqlite3"))
            .await
            .expect("Library database");
        database
            .reconcile_source(
                &library::SourceId::new("navidrome:test"),
                "navidrome",
                "Navidrome",
            )
            .await
            .unwrap();
        let catalog_source_id = ("navidrome:test").to_string();
        let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
        catalog.start_observation().await.unwrap();

        (async {
            let mut batch = library::CatalogBatch::default();
            let result = batch
                .folders
                .extend_from_slice(&[library::FolderObservation {
                    metadata: library::FolderMetadata {
                        name: Some("Music".to_string()),
                        sort_text: Some("music".to_string()),
                        ..Default::default()
                    },
                    object_id: "navidrome:music-folder:1".to_string(),
                    artwork_binding: None,
                    ..Default::default()
                }]);
            catalog.apply_catalog(&batch, true).await?;
            Ok::<_, library::LibraryError>(result)
        })
        .await
        .expect("Music Folder");

        {
            let result = source
                .acquire_navidrome_library(&mut catalog, &|_| {}, &|| false)
                .await;
            result
        }
        .expect("private Navidrome library");
        {
            database
                .publish_catalog_update(&catalog_source_id, catalog.changes().clone())
                .await;
            Ok::<_, crate::SourceError>(())
        }
        .expect("publish Navidrome Inventory");
        let cancellation = library::ReadCancellation::new();
        let source_key = database
            .cached_source("navidrome:test", &cancellation)
            .await
            .expect("cached source")
            .expect("published source")
            .source;
        let albums = library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::Albums,
            source: Some(source_key),
            sort: library::CatalogSort::Title,
            folder: None,
            filter: ("AAA Rich Album").to_string(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: false,
        }
        .page_albums(&database, library::RouteSeedWindow::top(), &cancellation)
        .await
        .map(|page| page.rows)
        .expect("Album page");
        let album = albums.first().expect("rich Album");
        assert_eq!(
            album.metadata.musicbrainz_release_group_id.as_deref(),
            Some("release-group-one")
        );
        assert_eq!(album.metadata.release_types, ["album", "live"]);
        let binding: Vec<library::ArtworkCandidate> = serde_json::from_slice(
            album
                .artwork_binding
                .as_deref()
                .expect("Album artwork binding"),
        )
        .expect("Album artwork binding");
        assert_eq!(
            binding[0].source_id().unwrap().clone(),
            crate::SourceId::new("navidrome:test")
        );
        assert_eq!(
            match &binding[0].image {
                library::ArtworkImage::Native { tag, .. } => tag.as_deref(),
                _ => panic!("native image"),
            },
            Some("2026-08-27T00:00:00Z")
        );
        let tracks = (&library::MemberRead::new(library::Collection::Source(source_key)))
            .page(&database, library::RouteSeedWindow::top(), &cancellation)
            .await
            .expect("Track page");
        let track_rows = tracks
            .rows
            .into_iter()
            .map(library::MemberRow::into_track)
            .collect::<Vec<_>>();
        let track = track_rows.first().expect("rich Track");
        assert_eq!(track_rows.len(), 2);
        assert_eq!(track_rows[1].metadata.title, "ZZZ Last Track");
        assert_eq!(
            track.metadata.musicbrainz_recording_id.as_deref(),
            Some("recording-one")
        );
        assert_eq!(
            track.metadata.musicbrainz_release_track_id.as_deref(),
            Some("release-track-one")
        );
        assert_eq!(track.metadata.bpm, Some(123));
        let mapping = database
            .mapping_track_page(source_key, None, None, 1, &cancellation)
            .await
            .expect("mapping Track");
        assert_eq!(
            mapping[0].source_path,
            "/srv/navidrome/audio/Artist/Album/Track.flac"
        );
        let artists = library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::Artists {
                album_artist: false,
            },
            source: Some(source_key),
            sort: library::CatalogSort::Title,
            folder: None,
            filter: ("Track Artist").to_string(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: false,
        }
        .page_artists(&database, library::RouteSeedWindow::top(), &cancellation)
        .await
        .map(|page| page.rows)
        .expect("Track Artist page");
        assert_eq!(
            artists[0].metadata.musicbrainz_artist_id.as_deref(),
            Some("track-artist-mbid")
        );
    }

    #[tokio::test]
    async fn rotated_token_is_used_by_the_next_navidrome_page() {
        let server = MockServer::start().await;
        page_match("song", 0, "token-a")
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-nd-authorization", "token-b")
                    .set_body_json(serde_json::json!([{
                        "id": "song-one",
                        "libraryPath": "/music",
                        "path": "Artist/Album/Track.flac"
                    }])),
            )
            .expect(1)
            .mount(&server)
            .await;
        page_match("song", NAVIDROME_PAGE_SIZE, "token-b")
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;
        let source = navidrome_source_with_token(&server, "token-a");

        let mut first = source
            .navidrome_page("song", 0)
            .await
            .expect("first Navidrome page");
        source
            .navidrome_page("song", NAVIDROME_PAGE_SIZE)
            .await
            .expect("second Navidrome page");

        assert_eq!(
            track_from_navidrome(&source, "song-one", &first.remove(0))
                .source_path
                .as_deref(),
            Some("/music/Artist/Album/Track.flac")
        );
    }

    #[tokio::test]
    async fn unauthorized_navidrome_request_reauthenticates_and_retries_once() {
        let server = MockServer::start().await;
        page_match("song", 0, "token-a")
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "token-b"
            })))
            .expect(1)
            .mount(&server)
            .await;
        page_match("song", 0, "token-b")
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;
        let source = navidrome_source_with_token(&server, "token-a");

        source
            .navidrome_page("song", 0)
            .await
            .expect("retried Navidrome page");
    }

    #[tokio::test]
    async fn second_navidrome_authentication_failure_is_returned() {
        let server = MockServer::start().await;
        page_match("song", 0, "token-a")
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "token-b"
            })))
            .expect(1)
            .mount(&server)
            .await;
        page_match("song", 0, "token-b")
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        let source = navidrome_source_with_token(&server, "token-a");

        assert!(matches!(
            source.navidrome_page("song", 0).await,
            Err(SourceError::Auth(_))
        ));
    }
}
