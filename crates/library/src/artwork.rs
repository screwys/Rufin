//! Stores accepted artwork bindings and pages them for explicit source downloads.
//! Catalog writes choose references; fetching, disk caching and decoding stay in Artwork.

use md5_digest::{Digest, Md5};
use serde::{Deserialize, Serialize};
use sqlx::{Connection, QueryBuilder, Sqlite};

use crate::catalog::views::{Art, Context, Owner, Picture};
use crate::{Catalog, FolderKey, LibraryError, LibraryResult, ReadCancellation, SourceKey};

const ARTWORK_PAGE_LIMIT: usize = 128;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct AlbumCover {
    artist: Option<String>,
    album: Option<String>,
    release_group_id: Option<String>,
    release_id: Option<String>,
}

impl AlbumCover {
    pub fn new(
        artist: &str,
        album: &str,
        release_group_id: Option<&str>,
        release_id: Option<&str>,
    ) -> Option<Self> {
        let artist = Self::lookup_text(artist).map(ToString::to_string);
        let album = Self::lookup_text(album).map(ToString::to_string);
        let release_group_id = release_group_id
            .and_then(Self::usable_mbid)
            .map(ToString::to_string);
        let release_id = release_id
            .and_then(Self::usable_mbid)
            .map(ToString::to_string);
        (release_group_id.is_some() || release_id.is_some() || artist.is_some() && album.is_some())
            .then_some(Self {
                artist,
                album,
                release_group_id,
                release_id,
            })
    }

    pub fn stable_identity(&self) -> String {
        format!(
            "album\0{}\0{}\0{}\0{}",
            self.artist.as_deref().unwrap_or_default(),
            self.album.as_deref().unwrap_or_default(),
            self.release_group_id.as_deref().unwrap_or_default(),
            self.release_id.as_deref().unwrap_or_default()
        )
    }

    pub fn text(&self) -> Option<(&str, &str)> {
        Some((self.artist.as_deref()?, self.album.as_deref()?))
    }
    pub fn release_group_id(&self) -> Option<&str> {
        self.release_group_id.as_deref()
    }
    pub fn release_id(&self) -> Option<&str> {
        self.release_id.as_deref()
    }

    fn lookup_text(value: &str) -> Option<&str> {
        let value = value.trim();
        if value.is_empty()
            || matches!(
                value.to_ascii_lowercase().as_str(),
                "unknown"
                    | "unknown album"
                    | "unknown artist"
                    | "untitled album"
                    | "untitled track"
            )
        {
            None
        } else {
            Some(value)
        }
    }

    pub fn usable_mbid(value: &str) -> Option<&str> {
        let value = value.trim();
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return None;
        }
        Some(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtworkImage {
    Native {
        source_id: crate::SourceId,
        item_id: String,
        tag: Option<String>,
    },
    Local {
        source_id: crate::SourceId,
        path: String,
        revision: String,
        picture_index: Option<u32>,
    },
    Album(AlbumCover),
    Artist {
        name: String,
        musicbrainz_id: Option<String>,
    },
    Playlist {
        smart: bool,
        source_id: Option<String>,
        object_id: String,
        revision: String,
        /// The track URIs whose covers make the playlist's set-once mosaic.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mosaic: Option<Vec<String>>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtworkCandidate {
    pub key: String,
    pub image: ArtworkImage,
    #[serde(default)]
    pub blur_hash: Option<String>,
}

impl ArtworkCandidate {
    pub fn is_automatic_playlist(&self) -> bool {
        matches!(
            self.image,
            ArtworkImage::Playlist {
                mosaic: Some(_),
                ..
            }
        )
    }

    pub fn new(image: ArtworkImage, blur_hash: Option<String>) -> Self {
        let mut candidate = Self {
            key: String::new(),
            image,
            blur_hash,
        };
        let directory = match &candidate.image {
            ArtworkImage::Native { source_id, .. } | ArtworkImage::Local { source_id, .. } => {
                format!("native/{:x}", Md5::digest(source_id.as_str()))
            }
            ArtworkImage::Playlist { .. } => "playlists".into(),
            _ => "external".into(),
        };
        candidate.key = format!("{directory}/{:x}", Md5::digest(candidate.stable_identity()));
        candidate
    }

    pub fn asset_identity(&self) -> String {
        match &self.image {
            ArtworkImage::Native {
                source_id, item_id, ..
            } => format!("native\0{source_id}\0{item_id}"),
            ArtworkImage::Local {
                source_id,
                path,
                picture_index: None,
                ..
            } => format!("local-file\0{source_id}\0{path}"),
            ArtworkImage::Local {
                source_id,
                path,
                picture_index: Some(index),
                ..
            } => format!("local-embedded\0{source_id}\0{path}\0{index}"),
            ArtworkImage::Album(album) => album.stable_identity(),
            ArtworkImage::Artist {
                name,
                musicbrainz_id,
            } => format!(
                "artist\0{name}\0{}",
                musicbrainz_id.as_deref().unwrap_or_default()
            ),
            ArtworkImage::Playlist {
                smart,
                source_id,
                object_id,
                ..
            } => format!(
                "{}\0{}\0{object_id}",
                if *smart { "smart-playlist" } else { "playlist" },
                source_id.as_deref().unwrap_or_default()
            ),
        }
    }

    pub fn stable_identity(&self) -> String {
        match &self.image {
            ArtworkImage::Native {
                source_id,
                item_id,
                tag,
            } => format!(
                "native\0{source_id}\0{item_id}\0{}",
                tag.as_deref().unwrap_or_default()
            ),
            ArtworkImage::Local {
                source_id,
                path,
                picture_index,
                revision,
            } => {
                let mut identity = match picture_index {
                    Some(index) => format!("local-embedded\0{path}\0{index}\0{revision}"),
                    None => format!("local-file\0{path}\0{revision}"),
                };
                if source_id.as_str() != "local:server:library" {
                    identity.push('\0');
                    identity.push_str(source_id.as_str());
                }
                identity
            }
            ArtworkImage::Artist { .. } => format!("{}\0\0", self.asset_identity()),
            ArtworkImage::Playlist { revision, .. } => {
                format!("{}\0{revision}", self.asset_identity())
            }
            ArtworkImage::Album(_) => self.asset_identity(),
        }
    }

    pub fn is_external(&self) -> bool {
        matches!(
            self.image,
            ArtworkImage::Album(_) | ArtworkImage::Artist { .. }
        )
    }
    pub fn source_id(&self) -> Option<&crate::SourceId> {
        match &self.image {
            ArtworkImage::Native { source_id, .. } | ArtworkImage::Local { source_id, .. } => {
                Some(source_id)
            }
            _ => None,
        }
    }
    pub fn blur_hash(&self) -> Option<&str> {
        self.blur_hash.as_deref()
    }
}

/// Picture bindings of rows, read by key and composed as the Catalog chose them. An artist
/// lists its own pictures, then the one it borrows, then its name lookups. Mosaic recipes are
/// these candidates as stored, without preview hashes, so showing a cover never changes them.
async fn composed_art(
    connection: &mut sqlx::SqliteConnection,
    arts: &[Art],
) -> LibraryResult<Vec<Option<Vec<ArtworkCandidate>>>> {
    let mut keys: [Vec<i64>; 3] = Default::default();
    for art in arts {
        match *art {
            Art::None => {}
            Art::Track(key) => keys[0].push(key),
            Art::Album(key) => keys[1].push(key),
            Art::Artist(key, borrowed) => {
                keys[2].push(key);
                match borrowed {
                    Some(Picture::Track(key)) => keys[0].push(key),
                    Some(Picture::Album(key)) => keys[1].push(key),
                    None => {}
                }
            }
        }
    }
    let mut stored: [std::collections::HashMap<i64, Vec<ArtworkCandidate>>; 3] = Default::default();
    for ((table, column), (keys, stored)) in [
        ("track_details", "track_key"),
        ("albums", "album_key"),
        ("artists", "artist_key"),
    ]
    .into_iter()
    .zip(keys.iter().zip(&mut stored))
    {
        if keys.is_empty() {
            continue;
        }
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {column},native_artwork FROM {table} WHERE {column} IN (SELECT value FROM json_each(?1)) AND native_artwork IS NOT NULL"
        )))
        .bind(serde_json::to_string(keys)?)
        .fetch_all(&mut *connection)
        .await?;
        for (key, binding) in rows {
            stored.insert(key, serde_json::from_slice(&binding)?);
        }
    }
    let picture = |picture: Picture| match picture {
        Picture::Track(key) => stored[0].get(&key),
        Picture::Album(key) => stored[1].get(&key),
    };
    let composed = arts
        .iter()
        .map(|art| match *art {
            Art::None => None,
            Art::Track(key) => stored[0].get(&key).cloned(),
            Art::Album(key) => stored[1].get(&key).cloned(),
            Art::Artist(key, borrowed) => {
                let own = stored[2].get(&key).map(Vec::as_slice).unwrap_or_default();
                let lookup = |candidate: &&ArtworkCandidate| {
                    matches!(
                        candidate.image,
                        ArtworkImage::Artist { .. } | ArtworkImage::Album(_)
                    )
                };
                let candidates = own
                    .iter()
                    .filter(|candidate| !lookup(candidate))
                    .chain(borrowed.and_then(picture).into_iter().flatten())
                    .chain(own.iter().filter(lookup))
                    .cloned()
                    .collect::<Vec<_>>();
                (!candidates.is_empty()).then_some(candidates)
            }
        })
        .collect::<Vec<_>>();
    Ok(composed)
}

/// The bindings rows show: the composed candidates with the preview hashes known for them.
pub(crate) async fn art_bindings(
    connection: &mut sqlx::SqliteConnection,
    arts: &[Art],
) -> LibraryResult<Vec<Option<Vec<u8>>>> {
    let mut composed = composed_art(connection, arts).await?;
    let families = composed
        .iter()
        .flatten()
        .flatten()
        .filter(|candidate| candidate.blur_hash.is_none())
        .map(|candidate| candidate.key.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if !families.is_empty() {
        let previews: std::collections::HashMap<String, String> =
            sqlx::query_as("SELECT family,hash FROM artwork_previews WHERE family IN (SELECT value FROM json_each(?1))")
                .bind(serde_json::to_string(&families)?)
                .fetch_all(&mut *connection)
                .await?
                .into_iter()
                .collect();
        for candidate in composed.iter_mut().flatten().flatten() {
            if candidate.blur_hash.is_none() {
                candidate.blur_hash = previews.get(&candidate.key).cloned();
            }
        }
    }
    composed
        .into_iter()
        .map(|candidates| {
            candidates
                .map(|candidates| serde_json::to_vec(&candidates).map_err(Into::into))
                .transpose()
        })
        .collect()
}

pub(crate) fn preview_sql(binding: &str) -> String {
    format!(
        "CASE WHEN {binding} IS NOT NULL THEN CAST((SELECT json_group_array(json_object('key',json_extract(candidate.value,'$.key'),'image',json(json_extract(candidate.value,'$.image')),'blur_hash',COALESCE(json_extract(candidate.value,'$.blur_hash'),preview.hash)) ORDER BY candidate.key) FROM json_each({binding}) candidate LEFT JOIN artwork_previews preview ON preview.family=json_extract(candidate.value,'$.key')) AS BLOB) END"
    )
}

pub(crate) fn playlist_binding(
    smart: bool,
    source_id: Option<String>,
    object_id: String,
    revision: String,
) -> Vec<u8> {
    serde_json::to_vec(&[ArtworkCandidate::new(
        ArtworkImage::Playlist {
            smart,
            source_id,
            object_id,
            revision,
            mosaic: None,
        },
        None,
    )])
    .expect("playlist artwork binding")
}

pub fn normalize_artwork_binding(bytes: &[u8]) -> LibraryResult<Vec<u8>> {
    if let Ok(candidates) = serde_json::from_slice::<Vec<ArtworkCandidate>>(bytes) {
        return Ok(serde_json::to_vec(
            &candidates
                .into_iter()
                .map(|candidate| ArtworkCandidate::new(candidate.image, candidate.blur_hash))
                .collect::<Vec<_>>(),
        )?);
    }
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(_) => return Ok(b"[]".to_vec()),
    };
    let text = |field: &str| value[field].as_str().map(str::to_owned);
    let mut candidates = Vec::new();
    if value["kind"] == "artist" {
        for field in ["source", "fallback"] {
            if field == "fallback" {
                candidates.push(ArtworkCandidate::new(
                    ArtworkImage::Artist {
                        name: text("artist_name").unwrap_or_default(),
                        musicbrainz_id: text("musicbrainz_artist_id"),
                    },
                    text("blur_hash"),
                ));
            }
            if let Ok(bytes) = serde_json::from_value::<Vec<u8>>(value[field].clone()) {
                candidates.extend(serde_json::from_slice::<Vec<ArtworkCandidate>>(
                    &normalize_artwork_binding(&bytes)?,
                )?);
            }
        }
    } else if value["kind"] == "playlist" {
        candidates.push(ArtworkCandidate::new(
            ArtworkImage::Playlist {
                smart: value["smart"].as_bool().unwrap_or_default(),
                source_id: text("source_id"),
                object_id: text("object_id").unwrap_or_default(),
                revision: text("revision").unwrap_or_default(),
                mosaic: None,
            },
            text("blur_hash"),
        ));
    } else if let Some(image) = value.get("image") {
        candidates.push(ArtworkCandidate::new(
            ArtworkImage::Native {
                source_id: crate::SourceId::new(text("source_id").unwrap_or_default()),
                item_id: image["item_id"].as_str().unwrap_or_default().into(),
                tag: image["tag"].as_str().map(str::to_owned),
            },
            image["blur_hash"].as_str().map(str::to_owned),
        ));
    } else if let Some(image) = value.get("File").or_else(|| value.get("Embedded")) {
        candidates.push(ArtworkCandidate::new(
            ArtworkImage::Local {
                source_id: crate::SourceId::new(
                    image["source_id"]
                        .as_str()
                        .unwrap_or("local:server:library"),
                ),
                path: image["path"].as_str().unwrap_or_default().into(),
                revision: image["revision"].as_str().unwrap_or_default().into(),
                picture_index: image["picture_index"].as_u64().map(|index| index as u32),
            },
            image["blur_hash"].as_str().map(str::to_owned),
        ));
    } else if let Some(album) = AlbumCover::new(
        value["artist"].as_str().unwrap_or_default(),
        value["album"].as_str().unwrap_or_default(),
        value["musicbrainz_release_group_id"].as_str(),
        value["musicbrainz_release_id"].as_str(),
    ) {
        candidates.push(ArtworkCandidate::new(
            ArtworkImage::Album(album),
            text("blur_hash"),
        ));
    }

    Ok(serde_json::to_vec(&candidates)?)
}

pub fn automatic_playlist_artwork(binding: &[u8]) -> LibraryResult<Option<Vec<u8>>> {
    let candidates: Vec<ArtworkCandidate> = serde_json::from_slice(binding)?;
    candidates
        .into_iter()
        .find(ArtworkCandidate::is_automatic_playlist)
        .map(|candidate| serde_json::to_vec(&[candidate]))
        .transpose()
        .map_err(Into::into)
}

pub fn authored_playlist_artwork(binding: &[u8]) -> LibraryResult<Option<Vec<u8>>> {
    let mut candidates: Vec<ArtworkCandidate> = serde_json::from_slice(binding)?;
    candidates.retain(|candidate| !candidate.is_automatic_playlist());
    if candidates.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_vec(&candidates)?))
}

/// Pictures of tracks by URI, as their rows show them. A URI the Store does not hold has none.
pub(crate) async fn raw_playlist_artwork_on(
    catalog: &Catalog,
    connection: &mut sqlx::SqliteConnection,
    uris: &[String],
) -> LibraryResult<Vec<Vec<ArtworkCandidate>>> {
    let keys: Vec<Option<i64>> = sqlx::query_scalar("SELECT track.track_key FROM json_each(?1) requested LEFT JOIN tracks track ON track.media_uri=requested.value ORDER BY requested.key")
        .bind(serde_json::to_string(uris)?)
        .fetch_all(&mut *connection)
        .await?;
    let arts = catalog.arts(keys.into_iter().map(|key| key.map(Owner::Track)));
    Ok(composed_art(connection, &arts)
        .await?
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect())
}

/// A playlist row's binding: its own picture, then its mosaic. None when it has neither.
pub(crate) fn playlist_artwork(
    binding: Option<Vec<u8>>,
    owner: (bool, Option<String>, String),
    mosaic: Option<&str>,
) -> LibraryResult<Option<Vec<u8>>> {
    let mut candidates: Vec<ArtworkCandidate> = binding
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or_default();
    let picks = mosaic
        .map(serde_json::from_str::<Vec<String>>)
        .transpose()?
        .unwrap_or_default();
    if !picks.is_empty() {
        let (smart, source_id, object_id) = owner;
        candidates.push(ArtworkCandidate::new(
            ArtworkImage::Playlist {
                smart,
                source_id,
                object_id,
                revision: format!("mosaic:{:x}", Md5::digest(picks.join("\0"))),
                mosaic: Some(picks),
            },
            None,
        ));
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_vec(&candidates)?))
}

/// Up to four member URIs whose pictures differ, one per album: the chosen ones first, then
/// albums in URI order, or shuffled.
pub(crate) async fn playlist_artwork_choices_on(
    catalog: &Catalog,
    connection: &mut sqlx::SqliteConnection,
    members: &[crate::TrackKey],
    chosen: &[String],
    random: bool,
) -> LibraryResult<Vec<String>> {
    let mut result = chosen.to_vec();
    let mut assets: std::collections::HashSet<String> =
        raw_playlist_artwork_on(catalog, connection, chosen)
            .await?
            .iter()
            .filter_map(|recipe| recipe.first().map(ArtworkCandidate::asset_identity))
            .collect();
    let rows: Vec<(i64, String, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT track.track_key,track.media_uri,track.album_key,album.media_uri FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value LEFT JOIN albums album ON album.album_key=track.album_key",
    )
    .bind(serde_json::to_string(members)?)
    .fetch_all(&mut *connection)
    .await?;
    let arts = catalog.arts(rows.iter().map(|(key, ..)| Some(Owner::Track(*key))));
    let pictured = rows
        .iter()
        .zip(&arts)
        .filter(|(_, art)| **art != Art::None)
        .map(|((key, uri, album, _), _)| (*key, uri.as_str(), *album))
        .collect::<Vec<_>>();
    // Each album, or each track without one, is a group; its cover is its first member with a
    // picture.
    let mut groups = std::collections::BTreeMap::<&str, (&str, Option<i64>)>::new();
    for (_, uri, album, album_uri) in &rows {
        let group = groups
            .entry(album_uri.as_deref().unwrap_or(uri))
            .or_insert((uri, *album));
        group.0 = group.0.min(uri);
    }
    let mut covers = groups
        .into_values()
        .filter_map(|(first, album)| {
            pictured
                .iter()
                .find(|(_, uri, _)| *uri == first)
                .or_else(|| {
                    pictured
                        .iter()
                        .filter(|(_, _, member)| album.is_some() && *member == album)
                        .min_by_key(|(_, uri, _)| *uri)
                })
                .map(|(key, uri, _)| (*key, uri.to_string()))
        })
        .collect::<Vec<_>>();
    if random {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        crate::shuffle_order(&mut covers, seed);
    }
    for batch in covers.chunks(16) {
        if result.len() >= 4 {
            break;
        }
        let arts = catalog.arts(batch.iter().map(|(key, _)| Some(Owner::Track(*key))));
        for ((_, uri), recipe) in batch.iter().zip(composed_art(connection, &arts).await?) {
            let Some(recipe) = recipe else { continue };
            if result.len() < 4
                && let Some(asset) = recipe.first().map(ArtworkCandidate::asset_identity)
                && assets.insert(asset)
            {
                result.push(uri.clone());
            }
        }
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepresentativeArtworkScope {
    AllTracks,
    FavoriteTracks,
    PlaylistTracks,
    LatestAlbums(usize),
}

pub struct ArtworkCacheRetention<'a> {
    _permit: tokio::sync::MutexGuard<'a, ()>,
    catalog: &'a Catalog,
    connection: sqlx::pool::PoolConnection<Sqlite>,
    /// Downloaded and active tracks not paged yet, last first.
    tracks: Vec<i64>,
}

impl ArtworkCacheRetention<'_> {
    /// Pictures of the next downloaded or active tracks.
    pub async fn next_bindings(&mut self) -> LibraryResult<Vec<Vec<u8>>> {
        let page = self
            .tracks
            .split_off(self.tracks.len().saturating_sub(ARTWORK_PAGE_LIMIT));
        let arts = self
            .catalog
            .arts(page.into_iter().rev().map(|key| Some(Owner::Track(key))));
        Ok(art_bindings(&mut self.connection, &arts)
            .await?
            .into_iter()
            .flatten()
            .collect())
    }

    pub async fn retain(&mut self, families: &[String]) -> LibraryResult<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO temp.retained_artwork_families SELECT value FROM json_each(?1)",
        )
        .bind(serde_json::to_string(families)?)
        .execute(&mut *self.connection)
        .await?;
        Ok(())
    }

    pub async fn unretained(&mut self, families: &[String]) -> LibraryResult<Vec<bool>> {
        Ok(sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM temp.retained_artwork_families retained WHERE retained.family=requested.value) FROM json_each(?1) requested ORDER BY requested.key")
            .bind(serde_json::to_string(families)?)
            .fetch_all(&mut *self.connection).await?)
    }

    pub async fn finish(self) -> LibraryResult<()> {
        self.connection.close().await?;
        Ok(())
    }
}

impl Catalog {
    pub async fn artwork_preview(&self, family: &str) -> LibraryResult<Option<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(
            sqlx::query_scalar("SELECT hash FROM artwork_previews WHERE family=?1")
                .bind(family)
                .fetch_optional(&mut *connection)
                .await?,
        )
    }

    pub async fn write_artwork_preview(
        &self,
        family: &str,
        hash: &str,
        refresh: bool,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let written = sqlx::query("INSERT INTO artwork_previews(family,hash) VALUES(?1,?2) ON CONFLICT(family) DO UPDATE SET hash=excluded.hash WHERE ?3 AND hash IS NOT excluded.hash")
            .bind(family)
            .bind(hash)
            .bind(refresh)
            .execute(write.connection().await?)
            .await?
            .rows_affected();
        // Previews ride on artwork reads; showing one changes nothing a client lists.
        if written != 0 {
            write.keep();
        }
        write.commit().await?;
        Ok(())
    }

    pub async fn artwork_cache_retention(
        &self,
        active_media_uris: &[String],
    ) -> LibraryResult<ArtworkCacheRetention<'_>> {
        let (permit, mut connection) = self.acquire_export().await?;
        // The main Store stays read-only. Temporary rows live only on this connection.
        connection.close_on_drop();
        sqlx::raw_sql("PRAGMA query_only=OFF; CREATE TEMP TABLE retained_artwork_families(family TEXT PRIMARY KEY) WITHOUT ROWID;")
            .execute(&mut *connection).await?;
        let active: Vec<i64> = sqlx::query_scalar("SELECT track.track_key FROM json_each(?1) requested JOIN tracks track ON track.media_uri=requested.value")
            .bind(serde_json::to_string(active_media_uris)?)
            .fetch_all(&mut *connection)
            .await?;
        self.loaded().await;
        let mut tracks = self
            .snapshot()
            .tracks
            .iter()
            .filter(|(_, record)| record.flags & crate::catalog::snapshot::DOWNLOADED != 0)
            .map(|(_, record)| record.key)
            .chain(active)
            .collect::<Vec<_>>();
        tracks.sort_unstable_by(|left, right| right.cmp(left));
        tracks.dedup();
        Ok(ArtworkCacheRetention {
            _permit: permit,
            catalog: self,
            connection,
            tracks,
        })
    }

    /// Pictures of tracks, albums and artists by URI, as their rows show them.
    pub async fn artwork_bindings(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Option<Vec<u8>>>> {
        if media_uris.len() > ARTWORK_PAGE_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "artwork URI window exceeds 128".into(),
            ));
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let owners: Vec<(Option<i64>, Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT track.track_key,album.album_key,artist.artist_key FROM json_each(?1) requested
             LEFT JOIN tracks track ON track.media_uri=requested.value
             LEFT JOIN albums album ON album.media_uri=requested.value
             LEFT JOIN artists artist ON artist.media_uri=requested.value ORDER BY requested.key",
        )
        .bind(serde_json::to_string(media_uris)?)
        .fetch_all(&mut *connection)
        .await?;
        let arts = self.arts(owners.into_iter().map(|(track, album, artist)| {
            track
                .map(Owner::Track)
                .or(album.map(Owner::Album))
                .or(artist.map(Owner::Artist))
        }));
        art_bindings(&mut connection, &arts).await
    }

    pub async fn playlist_artwork_binding(
        &self,
        playlist: crate::PlaylistKey,
    ) -> LibraryResult<Option<Vec<u8>>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT {} FROM playlists WHERE playlist_key=?1",
            preview_sql("artwork_binding")
        )))
        .bind(playlist)
        .fetch_optional(&mut *connection)
        .await?
        .flatten())
    }
    pub async fn playlist_image(&self, image: &ArtworkImage) -> LibraryResult<Option<Vec<u8>>> {
        let ArtworkImage::Playlist {
            smart,
            source_id,
            object_id,
            revision,
            ..
        } = image
        else {
            return Ok(None);
        };
        let mut connection = self.acquire_reader().await?;
        if *smart {
            return Ok(sqlx::query_scalar("SELECT artwork_bytes FROM main.smart_playlists WHERE object_id=?1 AND artwork_revision=?2 AND artwork_bytes IS NOT NULL")
                .bind(object_id).bind(revision).fetch_optional(&mut *connection).await?);
        }
        Ok(sqlx::query_scalar("SELECT playlist.artwork_bytes FROM main.playlists playlist LEFT JOIN sources source USING(source_key) WHERE playlist.object_id=?1 AND source.object_id IS ?2 AND playlist.artwork_revision=?3 AND playlist.artwork_bytes IS NOT NULL")
            .bind(object_id).bind(source_id).bind(revision).fetch_optional(&mut *connection).await?)
    }

    pub async fn random_playlist_artwork(
        &self,
        playlist: crate::PlaylistKey,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        self.loaded().await;
        let members = self
            .snapshot()
            .playlist(playlist.raw())
            .map(|playlist| {
                playlist
                    .entries
                    .into_iter()
                    .map(|entry| entry.track)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut connection = self.acquire_reader().await?;
        let choices =
            playlist_artwork_choices_on(self, &mut connection, &members, &[], true).await?;
        raw_playlist_artwork_on(self, &mut connection, &choices)
            .await?
            .iter()
            .map(|recipe| Ok(serde_json::to_vec(recipe)?))
            .collect()
    }

    /// Pictures the rows of these tracks show, skipping tracks without one.
    pub async fn track_key_artwork_bindings(
        &self,
        keys: &[crate::TrackKey],
    ) -> LibraryResult<Vec<Vec<u8>>> {
        let arts = self.arts(keys.iter().map(|key| Some(Owner::Track(key.raw()))));
        let mut connection = self.acquire_reader().await?;
        Ok(art_bindings(&mut connection, &arts)
            .await?
            .into_iter()
            .flatten()
            .collect())
    }

    /// Pictures of tracks by URI, skipping tracks without one.
    pub async fn track_artwork_bindings(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        if media_uris.len() > ARTWORK_PAGE_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "artwork URI window exceeds 128".into(),
            ));
        }
        let mut connection = tokio::select! {
            result = self.acquire_reader() => result?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let keys: Vec<i64> = sqlx::query_scalar("SELECT track.track_key FROM json_each(?1) requested JOIN tracks track ON track.media_uri=requested.value ORDER BY requested.key")
            .bind(serde_json::to_string(media_uris)?)
            .fetch_all(&mut *connection)
            .await?;
        let arts = self.arts(keys.into_iter().map(|key| Some(Owner::Track(key))));
        Ok(art_bindings(&mut connection, &arts)
            .await?
            .into_iter()
            .flatten()
            .collect())
    }

    /// Distinct pictures of a source's tracks in a scope, in track sort order.
    pub async fn representative_artwork_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        scope: RepresentativeArtworkScope,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        let limit = limit.clamp(1, ARTWORK_PAGE_LIMIT);
        self.loaded().await;
        let arts = {
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot);
            let derived = self.covers(&context);
            let Some(owner) = snapshot.source_slot(source) else {
                return Ok(Vec::new());
            };
            let folder = match folder {
                Some(folder) => match snapshot.folders.slot(folder.raw()) {
                    Some(slot) => Some(slot),
                    None => return Ok(Vec::new()),
                },
                None => None,
            };
            let latest = match scope {
                RepresentativeArtworkScope::LatestAlbums(albums) => {
                    let mut latest = snapshot
                        .albums
                        .iter()
                        .filter(|(_, album)| album.source == owner)
                        .map(|(slot, album)| (album.added, album.key, slot))
                        .collect::<Vec<_>>();
                    latest.sort_by(|left, right| {
                        (left.0 == u32::MAX)
                            .cmp(&(right.0 == u32::MAX))
                            .then(right.0.cmp(&left.0))
                            .then(left.1.cmp(&right.1))
                    });
                    latest.truncate(albums.clamp(1, 100));
                    Some(
                        latest
                            .into_iter()
                            .map(|(.., slot)| slot)
                            .collect::<std::collections::HashSet<_>>(),
                    )
                }
                _ => None,
            };
            let held = if scope == RepresentativeArtworkScope::PlaylistTracks {
                snapshot
                    .playlists
                    .iter()
                    .flat_map(|(_, playlist)| playlist.entries.iter().map(|entry| entry.track))
                    .collect::<std::collections::HashSet<_>>()
            } else {
                Default::default()
            };
            let mut tracks = snapshot
                .tracks
                .iter()
                .filter(|(slot, record)| {
                    record.source == owner
                        && folder
                            .is_none_or(|folder| snapshot.track_runs(record)[3].contains(&folder))
                        && match scope {
                            RepresentativeArtworkScope::AllTracks => true,
                            RepresentativeArtworkScope::FavoriteTracks => {
                                crate::catalog::views::favorite(record.flags) == Some(true)
                            }
                            RepresentativeArtworkScope::PlaylistTracks => held.contains(slot),
                            RepresentativeArtworkScope::LatestAlbums(_) => latest
                                .as_ref()
                                .is_some_and(|latest| latest.contains(&record.album)),
                        }
                })
                .map(|(_, record)| {
                    (
                        snapshot.track_sort(record),
                        record.key,
                        context.track_art(&derived, record),
                    )
                })
                .filter(|(.., art)| *art != Art::None)
                .collect::<Vec<_>>();
            tracks.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
            let mut seen = std::collections::HashSet::new();
            tracks
                .into_iter()
                .map(|(.., art)| art)
                .filter(|art| seen.insert(*art))
                .take(limit)
                .collect::<Vec<_>>()
        };
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut seen = std::collections::HashSet::new();
        Ok(art_bindings(&mut connection, &arts)
            .await?
            .into_iter()
            .flatten()
            .filter(|binding| seen.insert(binding.clone()))
            .collect())
    }

    pub async fn source_artwork_page(
        &self,
        source: SourceKey,
        after_binding: Option<&[u8]>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        let limit = limit.clamp(1, ARTWORK_PAGE_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mut query = QueryBuilder::<Sqlite>::new("");
        for (index, table) in [
            "track_details JOIN tracks USING(track_key)",
            "albums",
            "artists",
            "tags",
            "folders",
            "playlists",
        ]
        .into_iter()
        .enumerate()
        {
            if index > 0 {
                query.push(" UNION ");
            }
            let column = if table == "playlists" {
                "artwork_binding"
            } else {
                "native_artwork"
            };
            query
                .push(format!("SELECT {column} binding FROM "))
                .push(table)
                .push(" WHERE source_key=")
                .push_bind(source)
                .push(format!(" AND {column} IS NOT NULL"));
            if let Some(after) = after_binding {
                query.push(format!(" AND {column}>")).push_bind(after);
            }
        }
        query.push(" ORDER BY binding LIMIT ").push_bind(limit);
        let bindings = query
            .build_query_scalar()
            .fetch_all(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(bindings)
    }
}
