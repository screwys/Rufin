//! Stores accepted artwork bindings and pages them for explicit source downloads.
//! Catalog writes choose references; fetching, disk caching and decoding stay in Artwork.

use futures_util::TryStreamExt;
use md5_digest::{Digest, Md5};
use serde::{Deserialize, Serialize};
use sqlx::{Connection, QueryBuilder, Sqlite};

use crate::{Database, FolderKey, LibraryError, LibraryResult, ReadCancellation, SourceKey};

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
pub struct PlaylistArtworkManifest {
    pub source_id: Option<String>,
    pub folder_id: Option<String>,
    pub chosen: Vec<String>,
    pub pending: bool,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        automatic: Option<PlaylistArtworkManifest>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved: Option<Vec<Vec<ArtworkCandidate>>>,
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
                automatic: Some(_),
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

pub(crate) fn entity_artwork_sql(kind: &str, alias: &str) -> String {
    preview_sql(&if matches!(kind, "album" | "artist") {
        format!("{alias}.artwork_binding")
    } else {
        format!("{alias}.native_artwork")
    })
}

pub(crate) fn raw_track_artwork_sql(alias: &str) -> String {
    format!(
        "CASE {alias}.artwork_kind WHEN 1 THEN {alias}.native_artwork WHEN 2 THEN(SELECT artwork_binding FROM albums WHERE album_key={alias}.album_key) WHEN 3 THEN(SELECT artwork_binding FROM artists WHERE artist_key={alias}.artwork_artist_key) END"
    )
}

pub(crate) fn track_artwork_sql(alias: &str) -> String {
    preview_sql(&raw_track_artwork_sql(alias))
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
            automatic: None,
            resolved: None,
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
                automatic: None,
                resolved: None,
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

pub fn merge_playlist_artwork(
    previous: Option<&[u8]>,
    incoming: Option<&[u8]>,
) -> LibraryResult<Option<Vec<u8>>> {
    let mut candidates: Vec<ArtworkCandidate> = incoming
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or_default();
    if !candidates
        .iter()
        .any(ArtworkCandidate::is_automatic_playlist)
        && let Some(automatic) = previous
            .map(automatic_playlist_artwork)
            .transpose()?
            .flatten()
    {
        candidates.extend(serde_json::from_slice::<Vec<ArtworkCandidate>>(&automatic)?);
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_vec(&candidates)?))
}

pub(crate) async fn raw_playlist_artwork_on(
    connection: &mut sqlx::SqliteConnection,
    uris: &[String],
) -> LibraryResult<Vec<Vec<ArtworkCandidate>>> {
    let rows: Vec<Option<Vec<u8>>> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM json_each(?1) requested LEFT JOIN tracks track ON track.media_uri=requested.value LEFT JOIN albums album USING(album_key) ORDER BY requested.key", raw_track_artwork_sql("track"))))
        .bind(serde_json::to_string(uris)?).fetch_all(connection).await?;
    rows.into_iter()
        .map(|binding| {
            Ok(binding
                .map(|binding| serde_json::from_slice(&binding))
                .transpose()?
                .unwrap_or_default())
        })
        .collect()
}

pub(crate) async fn refresh_mosaics(
    connection: &mut sqlx::SqliteConnection,
    tracks: &[i64],
    albums: &[i64],
    artists: &[i64],
) -> LibraryResult<()> {
    for table in ["playlists", "smart_playlists"] {
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT owner.rowid,owner.artwork_binding FROM {table} owner WHERE EXISTS(
              SELECT 1 FROM json_each(owner.artwork_binding) candidate
              JOIN json_each(candidate.value,'$.image.automatic.chosen') chosen
              JOIN tracks track ON track.media_uri=chosen.value
              WHERE track.track_key IN(SELECT value FROM json_each(?1)) OR track.album_key IN(SELECT value FROM json_each(?2)) OR track.artwork_artist_key IN(SELECT value FROM json_each(?3)))"
        ))).bind(serde_json::to_string(tracks)?).bind(serde_json::to_string(albums)?).bind(serde_json::to_string(artists)?).fetch_all(&mut *connection).await?;
        for (key, binding) in rows {
            let mut candidates: Vec<ArtworkCandidate> = serde_json::from_slice(&binding)?;
            for candidate in &mut candidates {
                if let ArtworkImage::Playlist {
                    automatic: Some(manifest),
                    resolved,
                    revision,
                    ..
                } = &mut candidate.image
                {
                    let recipes = raw_playlist_artwork_on(connection, &manifest.chosen).await?;
                    if resolved.as_ref() == Some(&recipes) {
                        continue;
                    }
                    manifest.pending |= recipes.iter().any(Vec::is_empty);
                    *revision = format!(
                        "{}:{:x}",
                        if manifest.pending { "pending" } else { "auto" },
                        Md5::digest(serde_json::to_vec(&recipes)?)
                    );
                    *resolved = (!manifest.pending).then_some(recipes);
                    *candidate = ArtworkCandidate::new(candidate.image.clone(), None);
                }
            }
            let next = serde_json::to_vec(&candidates)?;
            if next != binding {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE {table} SET artwork_binding=?2 WHERE rowid=?1"
                )))
                .bind(key)
                .bind(next)
                .execute(&mut *connection)
                .await?;
            }
        }
    }
    Ok(())
}

pub(crate) fn playlist_artwork(
    binding: Option<Vec<u8>>,
    owner: (bool, Option<String>, String),
    context: (Option<String>, Option<String>),
) -> LibraryResult<Vec<u8>> {
    let mut candidates: Vec<ArtworkCandidate> = binding
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or_default();
    if !candidates
        .iter()
        .any(ArtworkCandidate::is_automatic_playlist)
    {
        let (source_id, folder_id) = context;
        candidates.push(ArtworkCandidate::new(
            ArtworkImage::Playlist {
                smart: owner.0,
                source_id: owner.1,
                object_id: owner.2,
                revision: "auto".into(),
                automatic: Some(PlaylistArtworkManifest {
                    source_id,
                    folder_id,
                    chosen: Vec::new(),
                    pending: true,
                }),
                resolved: None,
            },
            None,
        ));
    }
    Ok(serde_json::to_vec(&candidates)?)
}

pub(crate) async fn playlist_artwork_choices_on(
    connection: &mut sqlx::SqliteConnection,
    prefix: &str,
    chosen: &[String],
    random: bool,
) -> LibraryResult<Vec<String>> {
    let mut result = chosen.to_vec();
    let mut assets: std::collections::HashSet<String> = raw_playlist_artwork_on(connection, chosen)
        .await?
        .iter()
        .filter_map(|recipe| recipe.first().map(ArtworkCandidate::asset_identity))
        .collect();
    let order = if random { "random()" } else { "cover_group" };
    let sql = format!("{prefix},cover_groups AS MATERIALIZED (
        SELECT coalesce(album.media_uri,track.media_uri) cover_group,min(track.media_uri) first_uri,min(track.album_key) album_key
        FROM selected JOIN tracks track USING(track_key) LEFT JOIN albums album USING(album_key)
        GROUP BY cover_group ORDER BY {order}
    ) SELECT cover.media_uri,{} binding FROM cover_groups groups
    CROSS JOIN tracks first ON first.media_uri=groups.first_uri
    CROSS JOIN tracks cover ON cover.media_uri=coalesce(CASE WHEN {} IS NOT NULL THEN groups.first_uri END,
        (SELECT candidate.media_uri FROM selected remaining JOIN tracks candidate USING(track_key)
         WHERE candidate.album_key=groups.album_key AND {} IS NOT NULL ORDER BY candidate.media_uri LIMIT 1))",
        raw_track_artwork_sql("cover"),raw_track_artwork_sql("first"),raw_track_artwork_sql("candidate"));
    let mut rows =
        sqlx::query_as::<_, (String, Vec<u8>)>(sqlx::AssertSqlSafe(sql)).fetch(connection);
    while result.len() < 4
        && let Some((uri, binding)) = rows.try_next().await?
    {
        let recipe: Vec<ArtworkCandidate> = serde_json::from_slice(&binding)?;
        if let Some(asset) = recipe.first().map(ArtworkCandidate::asset_identity)
            && assets.insert(asset)
        {
            result.push(uri);
        }
    }
    Ok(result)
}

async fn refresh_automatic_playlist_artwork_on(
    connection: &mut sqlx::SqliteConnection,
    key: i64,
    candidate: &mut ArtworkCandidate,
) -> LibraryResult<()> {
    let previous = candidate.image.clone();
    if let ArtworkImage::Playlist {
        smart,
        automatic: Some(manifest),
        resolved,
        revision,
        ..
    } = &mut candidate.image
    {
        if !*smart
            && !manifest.chosen.is_empty()
            && !sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM playlist_entries WHERE playlist_key=?1)",
            )
            .bind(key)
            .fetch_one(&mut *connection)
            .await?
        {
            manifest.chosen.clear();
        }
        let recipes = raw_playlist_artwork_on(connection, &manifest.chosen).await?;
        manifest.chosen = manifest
            .chosen
            .iter()
            .zip(recipes)
            .filter_map(|(uri, recipe)| (!recipe.is_empty()).then(|| uri.clone()))
            .collect();
        let prefix = if *smart {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            crate::smart_playlists::smart_membership_sql(
                connection,
                now,
                Some(&[crate::SmartPlaylistKey::from_raw(key)]),
            )
            .await?
        } else {
            format!(
                "WITH selected AS (SELECT track_key FROM playlist_entries WHERE playlist_key={key})"
            )
        };
        manifest.chosen =
            playlist_artwork_choices_on(connection, &prefix, &manifest.chosen, false).await?;
        let recipes = raw_playlist_artwork_on(connection, &manifest.chosen).await?;
        manifest.pending = false;
        *revision = format!("auto:{:x}", Md5::digest(serde_json::to_vec(&recipes)?));
        *resolved = Some(recipes);
    }
    if candidate.image != previous {
        *candidate = ArtworkCandidate::new(candidate.image.clone(), None);
    }
    Ok(())
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
    connection: sqlx::pool::PoolConnection<Sqlite>,
    active: String,
    after: i64,
}

impl ArtworkCacheRetention<'_> {
    pub async fn next_bindings(&mut self) -> LibraryResult<Vec<Vec<u8>>> {
        let page: Vec<(i64, Vec<u8>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT track.track_key,{}
             FROM tracks track LEFT JOIN albums album USING(album_key)
             WHERE track.track_key>?1 AND ({}) IS NOT NULL
               AND (EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=track.media_uri AND locator.origin='download')
                    OR track.media_uri IN (SELECT value FROM json_each(?2)))
             ORDER BY track.track_key LIMIT 128",raw_track_artwork_sql("track"),raw_track_artwork_sql("track"))))
        .bind(self.after)
        .bind(&self.active)
        .fetch_all(&mut *self.connection)
        .await?;
        if let Some((key, _)) = page.last() {
            self.after = *key;
        }
        Ok(page.into_iter().map(|(_, binding)| binding).collect())
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

impl Database {
    pub async fn refresh_incomplete_playlist_artwork(
        &self,
        playlist: Option<crate::PlaylistKey>,
        activity_only: bool,
    ) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        for table in ["playlists", "smart_playlists"] {
            if (activity_only && table == "playlists")
                || (playlist.is_some() && table == "smart_playlists")
            {
                continue;
            }
            let has_members = if table == "playlists" {
                "AND (EXISTS(SELECT 1 FROM playlist_entries entry WHERE entry.playlist_key=playlists.rowid) OR EXISTS(SELECT 1 FROM json_each(artwork_binding) candidate WHERE json_array_length(json_extract(candidate.value,'$.image.automatic.chosen'))>0))"
            } else {
                ""
            };
            let definition = if table == "smart_playlists" {
                "definition_json"
            } else {
                "NULL"
            };
            let emptied = if table == "playlists" {
                "OR NOT EXISTS(SELECT 1 FROM playlist_entries entry WHERE entry.playlist_key=playlists.rowid)"
            } else {
                ""
            };
            let mut after = 0;
            loop {
                let rows: Vec<(i64,Vec<u8>,Option<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT rowid,artwork_binding,{definition} FROM {table} WHERE rowid>?2 AND(?1 IS NULL OR rowid=?1) {has_members} AND EXISTS(SELECT 1 FROM json_each(artwork_binding) candidate WHERE json_extract(candidate.value,'$.image.automatic.pending')=0 AND (json_array_length(json_extract(candidate.value,'$.image.automatic.chosen'))<4 {emptied})) ORDER BY rowid LIMIT 128")))
                    .bind(playlist).bind(after).fetch_all(&mut *connection).await?;
                let Some(last) = rows.last() else { break };
                after = last.0;
                for (key, binding, definition) in rows {
                    if activity_only && let Some(definition) = definition {
                        use crate::{SmartPlaylistRuleField as Field, SmartPlaylistSort as Sort};
                        let definition: crate::SmartPlaylistDefinition =
                            serde_json::from_str(&definition)?;
                        if !matches!(
                            definition.sort_field,
                            Sort::PlayCount | Sort::SkipCount | Sort::LastPlayed
                        ) && !definition
                            .match_all
                            .iter()
                            .chain(&definition.match_any)
                            .any(|rule| {
                                matches!(
                                    rule.field,
                                    Field::Played
                                        | Field::PlayCount
                                        | Field::SkipCount
                                        | Field::LastPlayed
                                )
                            })
                        {
                            continue;
                        }
                    }
                    let mut candidates: Vec<ArtworkCandidate> = serde_json::from_slice(&binding)?;
                    for candidate in &mut candidates {
                        if candidate.is_automatic_playlist() {
                            refresh_automatic_playlist_artwork_on(connection, key, candidate)
                                .await?;
                        }
                    }
                    let next = serde_json::to_vec(&candidates)?;
                    if next == binding {
                        continue;
                    }
                    let mut transaction = connection.begin().await?;
                    sqlx::query(sqlx::AssertSqlSafe(format!(
                        "UPDATE {table} SET artwork_binding=?2 WHERE rowid=?1"
                    )))
                    .bind(key)
                    .bind(next)
                    .execute(&mut *transaction)
                    .await?;
                    if table == "smart_playlists" {
                        crate::smart_playlists::sync_smart_on(
                            &mut transaction,
                            crate::SmartPlaylistKey::from_raw(key),
                        )
                        .await?;
                    } else {
                        crate::playlists::sync_playlist_header_on(
                            &mut transaction,
                            crate::PlaylistKey::from_raw(key),
                        )
                        .await?;
                    }
                    transaction.commit().await?;
                    self.cache_written();
                }
            }
        }
        Ok(())
    }

    pub async fn import_artwork_previews(&self, path: &std::path::Path) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        crate::migration::current::attach(connection, path, "old_artwork_previews").await?;
        let result = async {
            let mut transaction = connection.begin().await?;
            sqlx::query("INSERT OR IGNORE INTO main.artwork_previews SELECT family,hash FROM old_artwork_previews.previews").execute(&mut *transaction).await?;
            transaction.commit().await
        }.await;
        sqlx::query("DETACH DATABASE old_artwork_previews")
            .execute(&mut *connection)
            .await?;
        result?;
        Ok(())
    }

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
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("INSERT INTO artwork_previews(family,hash) VALUES(?1,?2) ON CONFLICT(family) DO UPDATE SET hash=excluded.hash WHERE ?3 AND hash IS NOT excluded.hash")
            .bind(family).bind(hash).bind(refresh).execute(connection).await?;
        Ok(())
    }

    pub async fn artwork_cache_retention(
        &self,
        active_media_uris: &[String],
    ) -> LibraryResult<ArtworkCacheRetention<'_>> {
        let (permit, mut connection) = self.acquire_export().await?;
        // The main database stays read-only. Temporary rows live only on this connection.
        connection.close_on_drop();
        sqlx::raw_sql("PRAGMA query_only=OFF; CREATE TEMP TABLE retained_artwork_families(family TEXT PRIMARY KEY) WITHOUT ROWID;")
            .execute(&mut *connection).await?;
        Ok(ArtworkCacheRetention {
            _permit: permit,
            connection,
            active: serde_json::to_string(active_media_uris)?,
            after: 0,
        })
    }

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
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT {} FROM json_each(?1) requested
             LEFT JOIN tracks track ON track.media_uri=requested.value
             LEFT JOIN albums album ON album.media_uri=requested.value
             LEFT JOIN artists artist ON artist.media_uri=requested.value ORDER BY requested.key",
            preview_sql(&format!(
                "coalesce({}, {}, {})",
                raw_track_artwork_sql("track"),
                "album.artwork_binding",
                "artist.artwork_binding"
            ))
        )))
        .bind(serde_json::to_string(media_uris)?)
        .fetch_all(&mut *connection)
        .await?)
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

    pub async fn resolve_playlist_artwork(
        &self,
        candidate: ArtworkCandidate,
    ) -> LibraryResult<Option<ArtworkCandidate>> {
        let ArtworkImage::Playlist {
            smart,
            source_id,
            object_id,
            ..
        } = &candidate.image
        else {
            return Ok(None);
        };
        let smart = *smart;
        let source_id = source_id.clone();
        let object_id = object_id.clone();
        let (table, from, scope) = if smart {
            ("smart_playlists", "smart_playlists owner", "?2 IS NULL")
        } else {
            (
                "playlists",
                "playlists owner LEFT JOIN sources source USING(source_key)",
                "source.object_id IS ?2",
            )
        };
        let automatic = "json_extract(value,'$.image.automatic') IS NOT NULL";
        let select = format!(
            "SELECT owner.rowid,(SELECT value FROM json_each(owner.artwork_binding) WHERE {automatic} LIMIT 1) FROM {from} WHERE owner.object_id=?1 AND {scope}"
        );
        let mut connection = self.acquire_reader().await?;
        let Some((key, previous)) =
            sqlx::query_as::<_, (i64, Option<String>)>(sqlx::AssertSqlSafe(select))
                .bind(&object_id)
                .bind(&source_id)
                .fetch_optional(&mut *connection)
                .await?
        else {
            return Ok(None);
        };
        let mut selected = previous
            .as_deref()
            .map(serde_json::from_str::<ArtworkCandidate>)
            .transpose()?
            .unwrap_or(candidate);
        if let ArtworkImage::Playlist {
            automatic: Some(manifest),
            resolved,
            ..
        } = &selected.image
        {
            if !manifest.pending && resolved.is_some() {
                return Ok(Some(selected));
            }
            refresh_automatic_playlist_artwork_on(&mut connection, key, &mut selected).await?;
            drop(connection);
            {
                let mut writer = self.writer().await?;
                let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
                let mut transaction = connection.begin().await?;
                let path = format!(
                    "(SELECT '$['||key||']' FROM json_each(artwork_binding) WHERE {automatic} LIMIT 1)"
                );
                let changed = sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET artwork_binding=CAST(CASE WHEN {path} IS NULL THEN json_insert(coalesce(artwork_binding,'[]'),'$[#]',json(?1)) ELSE json_set(artwork_binding,{path},json(?1)) END AS BLOB) WHERE rowid=?2 AND (SELECT json(value) FROM json_each(artwork_binding) WHERE {automatic} LIMIT 1) IS json(?3)")))
                    .bind(serde_json::to_string(&selected)?).bind(key).bind(previous).execute(&mut *transaction).await?.rows_affected()>0;
                if changed {
                    if smart {
                        crate::smart_playlists::sync_smart_on(
                            &mut transaction,
                            crate::SmartPlaylistKey::from_raw(key),
                        )
                        .await?;
                    } else {
                        crate::playlists::sync_playlist_header_on(
                            &mut transaction,
                            crate::PlaylistKey::from_raw(key),
                        )
                        .await?;
                    }
                }
                transaction.commit().await?;
                if changed {
                    self.cache_written();
                }
            }
            connection = self.acquire_reader().await?;
            let winner: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT (SELECT value FROM json_each(artwork_binding) WHERE {automatic} LIMIT 1) FROM {table} WHERE rowid=?1"))).bind(key).fetch_optional(&mut *connection).await?.flatten();
            return winner
                .map(|winner| serde_json::from_str(&winner).map_err(Into::into))
                .transpose();
        }
        Ok(Some(selected))
    }

    pub async fn random_playlist_artwork(
        &self,
        playlist: crate::PlaylistKey,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        let mut connection = self.acquire_reader().await?;
        let prefix = format!(
            "WITH selected AS (SELECT track_key FROM playlist_entries WHERE playlist_key={})",
            playlist.raw()
        );
        let choices = playlist_artwork_choices_on(&mut connection, &prefix, &[], true).await?;
        raw_playlist_artwork_on(&mut connection, &choices)
            .await?
            .iter()
            .map(|recipe| Ok(serde_json::to_vec(recipe)?))
            .collect()
    }

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
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT {} FROM json_each(?1) requested
             JOIN tracks track ON track.media_uri=requested.value
             LEFT JOIN albums album USING(album_key)
             WHERE ({}) IS NOT NULL ORDER BY requested.key",
            track_artwork_sql("track"),
            raw_track_artwork_sql("track")
        )))
        .bind(serde_json::to_string(media_uris)?)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn representative_artwork_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        scope: RepresentativeArtworkScope,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        let limit = limit.clamp(1, ARTWORK_PAGE_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query =
            QueryBuilder::<Sqlite>::new(format!("SELECT {} FROM (", preview_sql("binding")));
        query.push(format!(
            "SELECT {} binding FROM tracks track WHERE track.source_key=",
            raw_track_artwork_sql("track")
        ));
        query.push_bind(source);
        match scope {
            RepresentativeArtworkScope::AllTracks => {}
            RepresentativeArtworkScope::FavoriteTracks => {
                query.push(" AND COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=track.media_uri),(SELECT favorite FROM native_media_state WHERE media_uri=track.media_uri))=1");
            }
            RepresentativeArtworkScope::PlaylistTracks => {
                query.push(" AND EXISTS (SELECT 1 FROM playlist_entries entry WHERE entry.track_key=track.track_key)");
            }
            RepresentativeArtworkScope::LatestAlbums(album_limit) => {
                query.push(" AND track.album_key IN (SELECT latest.album_key FROM albums latest WHERE latest.source_key=").push_bind(source).push(" ORDER BY ").push(crate::tracks::added_at!("latest")).push(" DESC NULLS LAST,latest.album_key LIMIT ").push_bind(album_limit.clamp(1, 100) as i64).push(")");
            }
        };
        query.push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders folder_scope WHERE folder_scope.track_key=track.track_key AND folder_scope.folder_key=").push_bind(folder).push(")) AND binding IS NOT NULL GROUP BY binding ORDER BY min(track.sort_text),min(track.track_key) LIMIT ").push_bind(limit).push(")");
        Ok(query
            .build_query_scalar::<Vec<u8>>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?)
    }

    pub async fn source_artwork_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        after_binding: Option<&[u8]>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        let limit = limit.clamp(1, ARTWORK_PAGE_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mut query = QueryBuilder::<Sqlite>::new("");
        for (index, table) in [
            "tracks",
            "albums",
            "artists",
            "genres",
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
            if let Some(folder) = folder {
                let members = format!(
                    "SELECT track_key FROM track_folders WHERE folder_key={}",
                    folder.raw()
                );
                let predicate = match table {
                    "tracks" => format!("track_key IN ({members})"),
                    "albums" => format!(
                        "album_key IN (SELECT album_key FROM tracks WHERE track_key IN ({members}))"
                    ),
                    "artists" => format!(
                        "artist_key IN (SELECT artist_key FROM track_artists WHERE track_key IN ({members}) UNION SELECT artist_key FROM album_artists WHERE album_key IN (SELECT album_key FROM tracks WHERE track_key IN ({members})))"
                    ),
                    "genres" => format!(
                        "genre_key IN (SELECT genre_key FROM track_genres WHERE track_key IN ({members}) UNION SELECT genre_key FROM album_genres WHERE album_key IN (SELECT album_key FROM tracks WHERE track_key IN ({members})))"
                    ),
                    "folders" => format!(
                        "folder_key={} OR folder_key IN (SELECT folder_key FROM track_folders WHERE track_key IN ({members}))",
                        folder.raw()
                    ),
                    // Native playlists can span the server's music folders.
                    "playlists" => "1".into(),
                    _ => unreachable!(),
                };
                query.push(" AND (").push(predicate).push(")");
            }
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
