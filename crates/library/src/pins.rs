//! Sidebar pins: rows read by key, totals from the resident Catalog; smart totals are evaluated
//! separately from row loading.
use crate::{Catalog, LibraryResult, ReadCancellation, SourceId};
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

impl Catalog {
    /// The key of a pinned row, when the Store still holds it.
    async fn pin_key(
        connection: &mut sqlx::SqliteConnection,
        pin: &SidebarPin,
    ) -> LibraryResult<Option<i64>> {
        let (sql, source, object) = match pin {
            SidebarPin::Album {
                source_id,
                album_id,
            } => (
                "SELECT item.album_key FROM albums item JOIN sources source USING(source_key) WHERE source.object_id=?1 AND item.object_id=?2",
                Some(source_id.as_str()),
                album_id,
            ),
            SidebarPin::Artist {
                source_id,
                artist_id,
                ..
            } => (
                "SELECT item.artist_key FROM artists item JOIN sources source USING(source_key) WHERE source.object_id=?1 AND item.object_id=?2",
                Some(source_id.as_str()),
                artist_id,
            ),
            SidebarPin::Genre {
                source_id,
                genre_id,
            } => (
                "SELECT item.tag_key FROM tags item JOIN sources source USING(source_key) WHERE item.kind=0 AND source.object_id=?1 AND item.object_id=?2",
                Some(source_id.as_str()),
                genre_id,
            ),
            SidebarPin::Playlist {
                source_id,
                playlist_id,
            } => (
                "SELECT item.playlist_key FROM playlists item LEFT JOIN sources source USING(source_key) WHERE source.object_id IS ?1 AND (?1 IS NULL OR item.source_key IS NOT NULL) AND item.object_id=?2 AND item.name IS NOT NULL",
                source_id.as_ref().map(SourceId::as_str),
                playlist_id,
            ),
            SidebarPin::SmartPlaylist { playlist_id } => (
                "SELECT smart_playlist_key FROM smart_playlists WHERE ?1 IS NULL AND object_id=?2",
                None,
                playlist_id,
            ),
            SidebarPin::PlaylistFolder { folder_id, .. } => (
                "SELECT folder_key FROM playlist_folders WHERE ?1 IS NULL AND object_id=?2",
                None,
                folder_id,
            ),
        };
        Ok(sqlx::query_scalar(AssertSqlSafe(sql))
            .bind(source)
            .bind(object)
            .fetch_optional(connection)
            .await?)
    }

    pub async fn sidebar_pin_rows(
        &self,
        pins: &[SidebarPin],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SidebarPinRow>> {
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let mut rows = Vec::with_capacity(pins.len());
        for pin in pins {
            let Some(key) = Self::pin_key(&mut connection, pin).await? else {
                continue;
            };
            let row = match pin {
                SidebarPin::Album { .. } => crate::collections::load_album_rows(
                    self,
                    &mut connection,
                    &[crate::AlbumKey::from_raw(key)],
                    false,
                )
                .await?
                .pop()
                .map(|album| {
                    (
                        album.metadata.title,
                        Some((album.summary.track_count, album.summary.duration_millis)),
                        album.artwork_binding.into_iter().collect(),
                    )
                }),
                SidebarPin::Artist { album_artist, .. } => crate::collections::load_artist_rows(
                    self,
                    &mut connection,
                    &[crate::ArtistKey::from_raw(key)],
                    *album_artist,
                    false,
                )
                .await?
                .pop()
                .map(|artist| {
                    (
                        artist.metadata.name,
                        Some((artist.summary.track_count, artist.summary.duration_millis)),
                        artist.artwork_binding.into_iter().collect(),
                    )
                }),
                SidebarPin::Genre { .. } => crate::collections::load_genre_rows(
                    self,
                    &mut connection,
                    &[crate::GenreKey::from_raw(key)],
                    None,
                )
                .await?
                .pop()
                .map(|genre| {
                    let artwork = match genre.artwork_binding {
                        Some(binding) => vec![binding],
                        None => genre.representative_artwork,
                    };
                    (
                        genre.metadata.name,
                        Some((genre.summary.track_count, genre.summary.duration_millis)),
                        artwork,
                    )
                }),
                SidebarPin::Playlist { .. } => crate::playlists::load_playlist_rows(
                    self,
                    &mut connection,
                    &[crate::PlaylistKey::from_raw(key)],
                )
                .await?
                .pop()
                .map(|playlist| {
                    (
                        playlist.name,
                        Some((playlist.track_count, playlist.duration_millis)),
                        playlist.artwork_binding.into_iter().collect(),
                    )
                }),
                SidebarPin::SmartPlaylist { playlist_id } => {
                    let (name, binding): (String, Option<Vec<u8>>) =
                        sqlx::query_as(AssertSqlSafe(format!(
                            "SELECT name,{} FROM smart_playlists WHERE smart_playlist_key=?1",
                            crate::artwork::preview_sql("artwork_binding")
                        )))
                        .bind(key)
                        .fetch_one(&mut *connection)
                        .await?;
                    let artwork = crate::artwork::playlist_artwork(
                        binding,
                        (true, None, playlist_id.clone()),
                        (None, None),
                    )?;
                    Some((name, None, vec![artwork]))
                }
                SidebarPin::PlaylistFolder { smart, .. } => {
                    #[derive(FromRow)]
                    struct Folder {
                        name: String,
                        binding: Option<Vec<u8>>,
                        cover_id: Option<String>,
                        cover_source: Option<String>,
                    }
                    let folder: Folder = sqlx::query_as(AssertSqlSafe(format!(
                        "SELECT item.name,{} binding,coalesce(smart.object_id,regular.object_id) cover_id,source.object_id cover_source
                        FROM playlist_folders item
                        LEFT JOIN playlists regular ON NOT item.smart AND regular.playlist_key=(SELECT min(child.playlist_key) FROM playlists child WHERE child.playlist_folder_key=item.folder_key AND child.name IS NOT NULL AND (child.owner='rufin' OR child.indexed))
                        LEFT JOIN smart_playlists smart ON item.smart AND smart.smart_playlist_key=(SELECT min(child.smart_playlist_key) FROM smart_playlists child WHERE child.playlist_folder_key=item.folder_key)
                        LEFT JOIN sources source ON source.source_key=regular.source_key
                        WHERE item.folder_key=?1",
                        crate::artwork::preview_sql("CASE WHEN item.smart THEN smart.artwork_binding ELSE regular.artwork_binding END")
                    )))
                    .bind(key)
                    .fetch_one(&mut *connection)
                    .await?;
                    let artwork = folder
                        .cover_id
                        .map(|id| {
                            crate::artwork::playlist_artwork(
                                folder.binding,
                                (*smart, folder.cover_source, id),
                                (None, None),
                            )
                        })
                        .transpose()?;
                    Some((folder.name, None, artwork.into_iter().collect()))
                }
            };
            if let Some((name, totals, artwork)) = row {
                rows.push(SidebarPinRow {
                    pin: pin.clone(),
                    key,
                    name,
                    totals,
                    artwork,
                });
            }
        }
        Ok(rows)
    }

    /// Totals of pinned smart playlists and playlist folders, which take an evaluation.
    pub async fn sidebar_pin_totals(
        &self,
        mut rows: Vec<SidebarPinRow>,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SidebarPinRow>> {
        if !rows.iter().any(|row| {
            matches!(
                row.pin,
                SidebarPin::SmartPlaylist { .. } | SidebarPin::PlaylistFolder { .. }
            )
        }) {
            return Ok(rows);
        }
        self.loaded().await;
        let smart = self
            .snapshot()
            .smart_playlists
            .values()
            .map(|smart| (smart.key, smart.folder))
            .collect::<Vec<_>>();
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let totals = crate::smart_playlists::smart_totals(
            self,
            &mut connection,
            &smart.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            now,
        )
        .await?;
        let snapshot = self.snapshot();
        let context = crate::catalog::views::Context::new(&snapshot, false);
        let mut folders = HashMap::<i64, (i64, i64)>::new();
        for (_, playlist) in snapshot.playlists.iter() {
            if playlist.folder != 0 && context.listed_playlist(playlist) {
                let total = context.playlist_totals(playlist, None);
                let folder = folders.entry(playlist.folder).or_default();
                folder.0 += i64::from(total.tracks);
                folder.1 += total.duration as i64;
            }
        }
        for (key, folder) in &smart {
            if let (Some(folder), Some(total)) = (folder, totals.get(key)) {
                let folder = folders.entry(*folder).or_default();
                folder.0 += i64::from(total.tracks);
                folder.1 += total.duration as i64;
            }
        }
        for row in &mut rows {
            row.totals = match row.pin {
                SidebarPin::SmartPlaylist { .. } => Some(
                    totals
                        .get(&row.key)
                        .map(|total| (i64::from(total.tracks), total.duration as i64))
                        .unwrap_or_default(),
                ),
                SidebarPin::PlaylistFolder { .. } => {
                    Some(folders.get(&row.key).copied().unwrap_or_default())
                }
                _ => row.totals,
            };
        }
        Ok(rows)
    }
}
