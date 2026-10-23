//! Indexed sidebar facts; smart totals are evaluated separately from row loading.
use crate::{Database, LibraryResult, ReadCancellation, SmartPlaylistKey, SourceId};
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, FromRow};
use std::collections::HashMap;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarPin {
    Album {
        source_id: SourceId,
        album_id: String,
    },
    Artist {
        source_id: SourceId,
        artist_id: String,
        #[serde(default, skip_serializing_if = "is_false")]
        album_artist: bool,
    },
    Genre {
        source_id: SourceId,
        genre_id: String,
    },
    Playlist {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<SourceId>,
        playlist_id: String,
    },
    SmartPlaylist {
        playlist_id: String,
    },
    PlaylistFolder {
        folder_id: String,
        smart: bool,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq)]
pub struct SidebarPinRow {
    pub pin: SidebarPin,
    pub key: i64,
    pub name: String,
    pub totals: Option<(i64, i64)>,
    pub artwork: Vec<Vec<u8>>,
}

const REQUESTED: &str = "WITH requested AS (
    SELECT CAST(pin.key AS INTEGER) ordinal,kind.key kind,
    json_extract(kind.value,'$.source_id') source_id,
    coalesce(json_extract(kind.value,'$.album_id'),json_extract(kind.value,'$.artist_id'),
        json_extract(kind.value,'$.genre_id'),json_extract(kind.value,'$.playlist_id'),
        json_extract(kind.value,'$.folder_id')) object_id,
    coalesce(json_extract(kind.value,'$.album_artist'),0) role
    FROM json_each(?1) pin CROSS JOIN json_each(pin.value) kind)";

impl Database {
    pub async fn sidebar_pin_rows(
        &self,
        pins: &[SidebarPin],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SidebarPinRow>> {
        if pins.is_empty() {
            return Ok(Vec::new());
        }
        #[derive(FromRow)]
        struct Row {
            ordinal: i64,
            entity_key: i64,
            name: String,
            track_count: Option<i64>,
            duration_millis: Option<i64>,
            artwork_binding: Option<Vec<u8>>,
            gallery: String,
            cover_id: Option<String>,
            cover_source: Option<String>,
        }
        let mut parts = Vec::new();
        for (kind, table, key, name, totals_kind) in [
            ("Album", "albums", "album_key", "title", "album"),
            ("Artist", "artists", "artist_key", "name", "artist"),
            ("Genre", "genres", "genre_key", "name", "genre"),
            ("Playlist", "playlists", "playlist_key", "name", "playlist"),
            (
                "SmartPlaylist",
                "smart_playlists",
                "smart_playlist_key",
                "name",
                "smart",
            ),
        ] {
            let (scope, totals, count, duration) = match kind {
                "SmartPlaylist" => (String::new(), String::new(), "NULL".to_string(), "NULL".to_string()),
                "Album" => ("AND item.source_key IS source.source_key AND source.source_key IS NOT NULL".into(), String::new(), "item.track_count".into(), "item.duration_millis".into()),
                _ => (
                    "AND item.source_key IS source.source_key AND (requested.source_id IS NULL OR source.source_key IS NOT NULL)".into(),
                    format!("LEFT JOIN collection_totals totals ON totals.kind='{totals_kind}' AND totals.entity_key=item.{key} AND totals.role=requested.role AND coalesce(totals.folder_key,'')=''"),
                    "coalesce(totals.track_count,0)".into(), "coalesce(totals.duration_millis,0)".into(),
                ),
            };
            let artwork = crate::artwork::preview_sql(if kind == "Genre" {
                "item.native_artwork"
            } else {
                "item.artwork_binding"
            });
            let gallery = if kind == "Genre" {
                format!(
                    "CASE WHEN item.native_artwork IS NULL THEN {} ELSE '[]' END",
                    crate::collections::gallery_sql("genre", None)
                        .replace("genre.genre_key", "item.genre_key")
                )
            } else {
                "'[]'".into()
            };
            parts.push(format!("SELECT requested.ordinal,item.{key} entity_key,item.{name} name,{count} track_count,{duration} duration_millis,{artwork} artwork_binding,{gallery} gallery,NULL cover_id,NULL cover_source
                FROM requested LEFT JOIN sources source ON source.object_id=requested.source_id
                CROSS JOIN {table} item ON item.object_id=requested.object_id {scope} {totals}
                WHERE requested.kind='{kind}' AND item.{name} IS NOT NULL"));
        }
        let binding = crate::artwork::preview_sql(
            "CASE WHEN item.smart THEN smart.artwork_binding ELSE regular.artwork_binding END",
        );
        parts.push(format!(
            "SELECT requested.ordinal,item.folder_key,item.name,NULL,NULL,
            {binding},'[]',coalesce(smart.object_id,regular.object_id),source.object_id
            FROM requested CROSS JOIN playlist_folders item ON item.object_id=requested.object_id
            LEFT JOIN playlists regular ON regular.playlist_key=item.playlist_cover_key
            LEFT JOIN smart_playlists smart ON smart.smart_playlist_key=item.smart_cover_key
            LEFT JOIN sources source ON source.source_key=regular.source_key
            WHERE requested.kind='PlaylistFolder'"
        ));
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let rows = sqlx::query_as::<_, Row>(AssertSqlSafe(format!(
            "{REQUESTED} SELECT * FROM ({}) ORDER BY ordinal",
            parts.join(" UNION ALL ")
        )))
        .bind(serde_json::to_string(pins)?)
        .fetch_all(&mut *connection)
        .await?;
        rows.into_iter()
            .map(|row| {
                let pin = pins[row.ordinal as usize].clone();
                let owner = match &pin {
                    SidebarPin::Playlist {
                        source_id,
                        playlist_id,
                    } => Some((
                        false,
                        source_id.as_ref().map(ToString::to_string),
                        playlist_id.clone(),
                    )),
                    SidebarPin::SmartPlaylist { playlist_id } => {
                        Some((true, None, playlist_id.clone()))
                    }
                    SidebarPin::PlaylistFolder { smart, .. } => {
                        row.cover_id.map(|id| (*smart, row.cover_source, id))
                    }
                    _ => None,
                };
                let artwork = if let Some(owner) = owner {
                    Some(crate::artwork::playlist_artwork(
                        row.artwork_binding,
                        owner,
                        (None, None),
                    )?)
                } else {
                    row.artwork_binding
                };
                Ok(SidebarPinRow {
                    pin,
                    key: row.entity_key,
                    name: row.name,
                    totals: row.track_count.zip(row.duration_millis),
                    artwork: if let Some(artwork) = artwork {
                        vec![artwork]
                    } else {
                        serde_json::from_str::<Vec<String>>(&row.gallery)?
                            .into_iter()
                            .map(String::into_bytes)
                            .collect()
                    },
                })
            })
            .collect()
    }

    pub async fn sidebar_pin_totals(
        &self,
        mut rows: Vec<SidebarPinRow>,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SidebarPinRow>> {
        let mut keys = rows
            .iter()
            .filter(|row| matches!(row.pin, SidebarPin::SmartPlaylist { .. }))
            .map(|row| SmartPlaylistKey::from_raw(row.key))
            .collect::<Vec<_>>();
        let folders = rows
            .iter()
            .filter(|row| matches!(row.pin, SidebarPin::PlaylistFolder { smart: true, .. }))
            .map(|row| row.key)
            .collect::<Vec<_>>();
        let regular_folders = rows
            .iter()
            .filter(|row| matches!(row.pin, SidebarPin::PlaylistFolder { smart: false, .. }))
            .map(|row| row.key)
            .collect::<Vec<_>>();
        if keys.is_empty() && folders.is_empty() && regular_folders.is_empty() {
            return Ok(rows);
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut folder_totals = HashMap::<i64, (i64, i64)>::new();
        if !regular_folders.is_empty() {
            folder_totals.extend(sqlx::query_as::<_,(i64,i64,i64)>("SELECT child.playlist_folder_key,sum(totals.track_count),sum(totals.duration_millis)
                FROM json_each(?1) requested CROSS JOIN playlists child ON child.playlist_folder_key=requested.value
                JOIN collection_totals totals ON totals.kind='playlist' AND totals.entity_key=child.playlist_key AND totals.role=0 AND coalesce(totals.folder_key,'')=''
                WHERE child.name IS NOT NULL AND (child.owner='rufin' OR child.indexed) GROUP BY child.playlist_folder_key")
                .bind(serde_json::to_string(&regular_folders)?).fetch_all(&mut *connection).await?.into_iter().map(|(key,count,duration)| (key,(count,duration))));
        }
        let members: Vec<(i64, SmartPlaylistKey)> = if folders.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as("SELECT playlist_folder_key,smart_playlist_key FROM smart_playlists WHERE playlist_folder_key IN(SELECT value FROM json_each(?1))")
                .bind(serde_json::to_string(&folders)?).fetch_all(&mut *connection).await?
        };
        keys.extend(members.iter().map(|(_, key)| *key));
        keys.sort_unstable();
        keys.dedup();
        let mut totals = HashMap::new();
        for batch in keys.chunks(crate::smart_playlists::SMART_PLAYLIST_ROW_LIMIT) {
            let membership =
                crate::smart_playlists::smart_membership_sql(&mut connection, now, Some(batch))
                    .await?;
            totals.extend(sqlx::query_as::<_,(SmartPlaylistKey,i64,i64)>(AssertSqlSafe(format!("{membership}
                SELECT definition_key,count(*),coalesce(sum(track.duration_millis),0) FROM selected
                JOIN tracks track INDEXED BY tracks_duration_idx USING(track_key) GROUP BY definition_key")))
                .fetch_all(&mut *connection).await?.into_iter().map(|(key,count,duration)| (key,(count,duration))));
        }
        for (folder, key) in members {
            if let Some(&(count, duration)) = totals.get(&key) {
                let total = folder_totals.entry(folder).or_default();
                total.0 += count;
                total.1 += duration;
            }
        }
        for row in &mut rows {
            row.totals = match row.pin {
                SidebarPin::SmartPlaylist { .. } => Some(
                    totals
                        .get(&SmartPlaylistKey::from_raw(row.key))
                        .copied()
                        .unwrap_or_default(),
                ),
                SidebarPin::PlaylistFolder { .. } => {
                    Some(folder_totals.get(&row.key).copied().unwrap_or_default())
                }
                _ => row.totals,
            };
        }
        Ok(rows)
    }
}
