//! Navidrome's private library supplement to the OpenSubsonic source.
//!
//! Standard server operations stay in the OpenSubsonic client. This module
//! owns the password login, rotating UI token, and richer album, track, and
//! artist records used during a Navidrome library refresh.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use library::Scan;
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

use super::item::{stage_album, stage_artist, stage_track};
use super::{
    Album, AlbumRelations, Artist, ArtistCredit, GenreCredit, ImageRef, SubsonicSource, Track,
    TrackRelations, normalize_release_types,
};

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
    pub(super) fn has_navidrome_library(&self) -> bool {
        self.navidrome_library
    }

    pub(super) async fn stage_navidrome_library(
        &self,
        scan: &mut Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        self.stage_navidrome_pages(
            "album",
            SourceReadStage::Albums,
            scan,
            progress,
            cancelled,
            |scan, raw_id, album| {
                Box::pin(stage_album(
                    scan,
                    album_from_navidrome(self, &raw_id, &album),
                ))
            },
        )
        .await?;
        self.stage_navidrome_pages(
            "song",
            SourceReadStage::Tracks,
            scan,
            progress,
            cancelled,
            |scan, raw_id, track| {
                Box::pin(stage_navidrome_track(
                    scan,
                    track_from_navidrome(self, &raw_id, &track),
                ))
            },
        )
        .await?;
        self.stage_navidrome_pages(
            "artist",
            SourceReadStage::Artists,
            scan,
            progress,
            cancelled,
            |scan, raw_id, artist| {
                Box::pin(stage_artist(
                    scan,
                    artist_from_navidrome(self, &raw_id, &artist),
                ))
            },
        )
        .await?;

        Ok(())
    }

    async fn stage_navidrome_pages(
        &self,
        endpoint: &str,
        stage: SourceReadStage,
        scan: &mut Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        mut write: impl for<'scan> FnMut(
            &'scan mut Scan,
            String,
            Value,
        ) -> Pin<
            Box<dyn Future<Output = library::LibraryResult<()>> + Send + 'scan>,
        >,
    ) -> SourceResult<()> {
        progress(SourceReadProgress {
            stage,
            completed: 0,
            total: None,
        });
        let mut offset = 0;
        loop {
            if cancelled() {
                return Err(SourceError::Cancelled);
            }
            let page = self.navidrome_page(endpoint, offset).await?;
            let page_len = page.len();
            if page_len == 0 {
                return Ok(());
            }
            offset = offset.checked_add(page_len).ok_or_else(|| {
                SourceError::Other(format!("Navidrome {endpoint} offset overflowed"))
            })?;
            scan.begin_batch().await?;
            for item in page {
                if let Some(raw_id) = id(&item["id"]) {
                    write(scan, raw_id, item).await?;
                }
            }
            scan.finish_batch().await?;
            progress(SourceReadProgress {
                stage,
                completed: offset,
                total: None,
            });
            if page_len < NAVIDROME_PAGE_SIZE {
                return Ok(());
            }
        }
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
        let mut retried = false;
        loop {
            let token = self.navidrome_token().await?;
            let mut url = navidrome_endpoint(&self.base_url, &format!("api/{endpoint}"))?;
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(key, value)| (*key, value.as_str())));
            let response = remote_http::json_with_header(
                self.client
                    .get(url)
                    .header(&NAVIDROME_AUTH_HEADER, format!("Bearer {token}")),
                NAVIDROME_HTTP,
                BodyLimit {
                    max_bytes: NAVIDROME_JSON_MAX_BYTES,
                    context: "Navidrome JSON response",
                },
                &NAVIDROME_AUTH_HEADER,
            )
            .await;
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
        let login = navidrome_login(&self.client, &self.base_url, &self.username, password).await?;
        let next = required(
            field(&login, "token").unwrap_or_default(),
            "Navidrome session token",
        )?;
        *token = Some(next.clone());
        Ok(next)
    }
}

async fn stage_navidrome_track(scan: &mut Scan, track: Track) -> library::LibraryResult<()> {
    let track_id = track.id.clone();
    let folders = track.relations.music_folders.clone();
    stage_track(scan, track).await?;
    scan.write_track_folders(
        &folders
            .iter()
            .map(|folder| library::ScanLink::new(&track_id, folder, 0))
            .collect::<Vec<_>>(),
    )
    .await
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

fn album_from_navidrome(source: &SubsonicSource, raw_id: &str, album: &Value) -> Album {
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
    let release_date = normalized_date(field(album, "releaseDate"));
    let year = positive_u16(field(album, "maxYear"))
        .or_else(|| {
            release_date
                .as_deref()
                .and_then(|date| date.get(..4))
                .and_then(|year| year.parse().ok())
        })
        .unwrap_or_default();
    Album {
        sort_name: clean_optional(field(album, "sortAlbumName")),
        id: String::from(source.id("album", raw_id)),
        title: clean_optional(field(album, "name")).unwrap_or_else(|| "Untitled Album".to_string()),
        artist,
        year,
        release_date,
        date_added: normalized_date(field(album, "createdAt")),
        last_played: clean_optional(field(album, "playDate")),
        play_count: capped_u32(field(album, "playCount")),
        user_rating: rating(field(album, "rating")),
        favorite: boolean(&album["starred"]).unwrap_or_default(),
        color_seed: super::color_seed(raw_id),
        image_ref: Some(navidrome_image_ref(
            source,
            "al",
            raw_id,
            clean_optional(field(album, "updatedAt")),
        )),
        local_artwork: None,
        release_types: normalize_release_types(tag_values(&album["tags"], "releasetype")),
        is_compilation: Some(boolean(&album["compilation"]).unwrap_or_default()),
        musicbrainz_album_id: clean_optional(field(album, "mbzAlbumId")),
        musicbrainz_release_group_id: clean_optional(field(album, "mbzReleaseGroupId")),
        relations: AlbumRelations {
            album_artists,
            artists: Vec::new(),
            genres: genre_credits(source, &album["genres"]),
        },
    }
}

fn track_from_navidrome(source: &SubsonicSource, raw_id: &str, track: &Value) -> Track {
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
    Track {
        sort_name: clean_optional(field(track, "sortTitle")),
        id: String::from(source.id("track", raw_id)),
        album_id,
        title: clean_optional(field(track, "title"))
            .unwrap_or_else(|| "Untitled Track".to_string()),
        artist,
        album: clean_optional(field(track, "album")).unwrap_or_else(|| "Unknown Album".to_string()),
        year: positive_u16(field(track, "year")).unwrap_or_default(),
        release_date: normalized_date(field(track, "releaseDate")),
        date_added: normalized_date(field(track, "createdAt")),
        last_played: crate::policy::unix_seconds(field(track, "playDate")),
        play_count: capped_u32(field(track, "playCount")),
        user_rating: rating(field(track, "rating")),
        duration_seconds: duration_seconds(field(track, "duration").unwrap_or_default()),
        favorite: boolean(&track["starred"]).unwrap_or_default(),
        disc_number: positive_u16(field(track, "discNumber")).unwrap_or_default(),
        track_number: positive_u16(field(track, "trackNumber")).unwrap_or_default(),
        image_ref: None,
        local_artwork: None,
        musicbrainz_recording_id: clean_optional(
            field(track, "mbzRecordingID").or_else(|| field(track, "mbzRecordingId")),
        ),
        musicbrainz_release_track_id: clean_optional(field(track, "mbzReleaseTrackId")),
        source_path: server_path(
            field::<String>(track, "libraryPath").as_deref(),
            &field::<String>(track, "path").unwrap_or_default(),
        ),
        cue: None,
        source_format: clean_optional(field(track, "suffix")),
        comment: clean_optional(field(track, "comment")),
        skip_count: None,
        bpm: positive_u16(field(track, "bpm")),
        replay_gain_track_db: field::<f64>(track, "rgTrackGain").filter(|value| value.is_finite()),
        replay_gain_track_peak: field::<f64>(track, "rgTrackPeak")
            .filter(|value| value.is_finite() && *value >= 0.0),
        replay_gain_album_db: field::<f64>(track, "rgAlbumGain").filter(|value| value.is_finite()),
        replay_gain_album_peak: field::<f64>(track, "rgAlbumPeak")
            .filter(|value| value.is_finite() && *value >= 0.0),
        relations: TrackRelations {
            artists,
            album_artists,
            genres: genre_credits(source, &track["genres"]),
            moods: super::moods_from_item(source, tag_values(&track["tags"], "mood")),
            music_folders: field::<i64>(track, "libraryId")
                .filter(|library_id| *library_id > 0)
                .map(|library_id| String::from(source.id("music-folder", &library_id.to_string())))
                .into_iter()
                .collect(),
        },
    }
}

fn artist_from_navidrome(source: &SubsonicSource, raw_id: &str, artist: &Value) -> Artist {
    Artist {
        sort_name: clean_optional(field(artist, "sortArtistName")),
        id: String::from(source.id("artist", raw_id)),
        name: clean_optional(field(artist, "name")).unwrap_or_else(|| "Unknown Artist".to_string()),
        favorite: boolean(&artist["starred"]).unwrap_or_default(),
        last_played: clean_optional(field(artist, "playDate")),
        play_count: capped_u32(field(artist, "playCount")),
        user_rating: rating(field(artist, "rating")),
        musicbrainz_artist_id: clean_optional(field(artist, "mbzArtistId")),
        image_ref: Some(navidrome_image_ref(source, "ar", raw_id, None)),
        local_artwork: None,
    }
}

fn artist_credit(
    source: &SubsonicSource,
    raw_id: String,
    name: String,
    musicbrainz_artist_id: Option<String>,
    sort_name: Option<String>,
) -> Option<ArtistCredit> {
    clean(raw_id).map(|raw_id| ArtistCredit {
        sort_name: clean_optional(sort_name),
        id: String::from(source.id("artist", &raw_id)),
        name,
        musicbrainz_artist_id: clean_optional(musicbrainz_artist_id),
    })
}

fn participant_credits(
    source: &SubsonicSource,
    participants: &Value,
    role: &str,
    fallback_id: &str,
    fallback_musicbrainz_id: Option<&str>,
) -> Vec<ArtistCredit> {
    let mut credits = items(&participants[role])
        .iter()
        .filter_map(|participant| {
            let raw_id = id(&participant["id"])?;
            Some(ArtistCredit {
                sort_name: clean_optional(field(participant, "sortArtistName")),
                id: String::from(source.id("artist", &raw_id)),
                name: clean_optional(field(participant, "name"))
                    .unwrap_or_else(|| "Unknown Artist".to_string()),
                musicbrainz_artist_id: clean_optional(field(participant, "mbzArtistId")),
            })
        })
        .collect::<Vec<_>>();
    if let (Some(fallback_id), Some(musicbrainz_id)) = (
        clean(fallback_id.to_string()),
        fallback_musicbrainz_id.and_then(|value| clean(value.to_string())),
    ) && let Some(credit) = credits
        .iter_mut()
        .find(|credit| credit.id.as_str() == source.id("artist", &fallback_id))
        && credit.musicbrainz_artist_id.is_none()
    {
        credit.musicbrainz_artist_id = Some(musicbrainz_id);
    }
    credits
}

fn genre_credits(source: &SubsonicSource, genres: &Value) -> Vec<GenreCredit> {
    items(genres)
        .iter()
        .filter_map(|genre| clean_optional(field(genre, "name")))
        .map(|name| GenreCredit {
            id: String::from(source.id("genre", &name)),
            name,
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
        let album = album_from_navidrome(
            &source,
            "album",
            &serde_json::json!({"name":"The Album", "sortAlbumName":"Album, The", "albumArtistId":"artist", "albumArtist":"The Artist", "sortAlbumArtistName":"Artist, The"}),
        );
        assert_eq!(album.title, "The Album");
        assert_eq!(album.sort_name.as_deref(), Some("Album, The"));
        assert_eq!(
            album.relations.album_artists[0].sort_name.as_deref(),
            Some("Artist, The")
        );
        let track = track_from_navidrome(
            &source,
            "track",
            &serde_json::json!({"title":"The Song", "sortTitle":"Song, The", "participants":{"artist":[{"id":"artist", "name":"The Artist", "sortArtistName":"Artist, The"}]}}),
        );
        assert_eq!(track.title, "The Song");
        assert_eq!(track.sort_name.as_deref(), Some("Song, The"));
        assert_eq!(
            track.relations.artists[0].sort_name.as_deref(),
            Some("Artist, The")
        );
        let artist = artist_from_navidrome(
            &source,
            "artist",
            &serde_json::json!({"name":"The Artist", "sortArtistName":"Artist, The"}),
        );
        assert_eq!(artist.name, "The Artist");
        assert_eq!(artist.sort_name.as_deref(), Some("Artist, The"));

        let value = serde_json::json!({"id":"1", "title":"The Name", "name":"The Name", "sortName":"Name, The", "artists":[{"id":"artist", "name":"The Artist", "sortName":"Artist, The"}]});
        let album = super::super::item::album_from_json(&source, &value).unwrap();
        assert_eq!(album.title, "The Name");
        assert_eq!(album.sort_name.as_deref(), Some("Name, The"));
        assert_eq!(
            album.relations.album_artists[0].sort_name.as_deref(),
            Some("Artist, The")
        );
        let track = super::super::item::track_from_json(&source, &value).unwrap();
        assert_eq!(track.title, "The Name");
        assert_eq!(track.sort_name.as_deref(), Some("Name, The"));
        let artist = super::super::item::artist_from_json(&source, &value).unwrap();
        assert_eq!(artist.name, "The Name");
        assert_eq!(artist.sort_name.as_deref(), Some("Name, The"));
        let plain = serde_json::json!({"id":"1", "name":"Plain", "sortName":""});
        assert!(
            super::super::item::album_from_json(&source, &plain)
                .unwrap()
                .sort_name
                .is_none()
        );
        assert!(
            super::super::item::track_from_json(&source, &plain)
                .unwrap()
                .sort_name
                .is_none()
        );
        assert!(
            super::super::item::artist_from_json(&source, &plain)
                .unwrap()
                .sort_name
                .is_none()
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
        assert_eq!(track.replay_gain_track_db, Some(-4.25));
        assert_eq!(track.replay_gain_track_peak, Some(0.91));
        assert_eq!(track.replay_gain_album_db, Some(-3.5));
        assert_eq!(track.replay_gain_album_peak, Some(0.95));
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
        let album = album_from_navidrome(&source, "album-one", &album);
        assert_eq!(
            album.musicbrainz_album_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(
            album.musicbrainz_release_group_id.as_deref(),
            Some("22222222-2222-2222-2222-222222222222")
        );
        assert_eq!(
            album.relations.album_artists[0]
                .musicbrainz_artist_id
                .as_deref(),
            Some("33333333-3333-3333-3333-333333333333")
        );
        assert_eq!(album.release_date.as_deref(), Some("2025-04-03"));
        assert_eq!(album.release_types, ["album", "live"]);
        assert!(album.relations.artists.is_empty());
        assert_eq!(
            album
                .image_ref
                .as_ref()
                .map(|image| (image.item_id.as_str(), image.tag.as_deref())),
            Some((
                "navidrome:cover:al-album-one_0",
                Some("2025-04-04T12:00:00Z")
            ))
        );

        let artist = serde_json::json!({
            "id": "artist-one",
            "name": "Album Artist",
            "mbzArtistId": "33333333-3333-3333-3333-333333333333"
        });
        let artist = artist_from_navidrome(&source, "artist-one", &artist);
        assert_eq!(
            artist.musicbrainz_artist_id.as_deref(),
            Some("33333333-3333-3333-3333-333333333333")
        );
        assert_eq!(
            artist
                .image_ref
                .as_ref()
                .map(|image| image.item_id.as_str()),
            Some("navidrome:cover:ar-artist-one_0")
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

    fn navidrome_source_with_token(server: &MockServer, token: &str) -> SubsonicSource {
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
        let directory = tempfile::tempdir().expect("Library directory");
        let database = library::Database::open(directory.path().join("library.sqlite3"))
            .await
            .expect("Library database");
        let mut scan =
            library::Scan::begin(&database, "navidrome:test", "Navidrome", "navidrome", None)
                .await
                .expect("Navidrome Scan");
        scan.begin_batch().await.expect("Folder batch");
        scan.write_folder("navidrome:music-folder:1", "Music", "music", "music", None)
            .await
            .expect("Music Folder");
        scan.finish_batch().await.expect("finish Folder batch");
        source
            .stage_navidrome_library(&mut scan, &|_| {}, &|| false)
            .await
            .expect("private Navidrome library");
        scan.finish().await.expect("publish Navidrome Scan");
        let cancellation = library::ReadCancellation::new();
        let source_key = database
            .cached_source("navidrome:test", &cancellation)
            .await
            .expect("cached source")
            .expect("published source")
            .source;
        let (_, _, albums) = database
            .album_route_page(
                source_key,
                None,
                false,
                "AAA Rich Album",
                library::AlbumSort::Title,
                false,
                library::RouteSeedWindow::top(),
                &cancellation,
            )
            .await
            .expect("Album page");
        let album = albums.first().expect("rich Album");
        assert_eq!(
            album.musicbrainz_release_group_id.as_deref(),
            Some("release-group-one")
        );
        assert_eq!(album.release_types, ["album", "live"]);
        let binding: crate::NativeArtworkBinding = serde_json::from_slice(
            album
                .artwork_binding
                .as_deref()
                .expect("Album artwork binding"),
        )
        .expect("Album artwork binding");
        assert_eq!(binding.source_id, crate::SourceId::new("navidrome:test"));
        assert_eq!(binding.image.tag.as_deref(), Some("2026-08-27T00:00:00Z"));
        let tracks = database
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
            .expect("Track page");
        let track = tracks.first_rows.first().expect("rich Track");
        assert_eq!(tracks.first_rows.len(), 2);
        assert_eq!(tracks.first_rows[1].title, "ZZZ Last Track");
        assert_eq!(
            track.musicbrainz_recording_id.as_deref(),
            Some("recording-one")
        );
        assert_eq!(
            track.musicbrainz_release_track_id.as_deref(),
            Some("release-track-one")
        );
        assert_eq!(track.bpm, Some(123));
        let mapping = database
            .mapping_track_page(source_key, None, None, 1, &cancellation)
            .await
            .expect("mapping Track");
        assert_eq!(
            mapping[0].source_path,
            "/srv/navidrome/audio/Artist/Album/Track.flac"
        );
        let (_, _, artists) = database
            .artist_route_page(
                source_key,
                None,
                false,
                false,
                "Track Artist",
                library::ArtistSort::Title,
                false,
                library::RouteSeedWindow::top(),
                &cancellation,
            )
            .await
            .expect("Track Artist page");
        assert_eq!(
            artists[0].musicbrainz_artist_id.as_deref(),
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
