//! Set-once mosaics. A playlist, smart playlist, genre or mood shows four covers of its member
//! tracks when it has no picture of its own. Covers are picked once and kept: while fewer than
//! four are found, a later pick adds to them, and once four are found they are never chosen
//! again.

use crate::catalog::views::Context;
use crate::catalog::write::Scope;
use crate::{Catalog, Collection, LibraryResult, PlaylistKey, SmartPlaylistKey, TrackKey};

const COVERS: usize = 4;

/// What a mosaic belongs to.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MosaicOwner {
    Playlist(PlaylistKey),
    Smart(SmartPlaylistKey),
    /// A genre or mood.
    Tag(i64),
}

impl MosaicOwner {
    fn row(self) -> (&'static str, &'static str, i64) {
        match self {
            Self::Playlist(key) => ("playlists", "playlist_key", key.raw()),
            Self::Smart(key) => ("smart_playlists", "smart_playlist_key", key.raw()),
            Self::Tag(key) => ("tags", "tag_key", key),
        }
    }
}

impl Catalog {
    /// Owners whose mosaic has fewer than four covers, each with whether it was ever picked. An
    /// owner with its own picture needs none.
    pub async fn incomplete_mosaics(&self) -> LibraryResult<Vec<(MosaicOwner, bool)>> {
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<(i64, i64, bool)> = sqlx::query_as(
            "SELECT 0,playlist_key,mosaic IS NOT NULL FROM playlists WHERE artwork_binding IS NULL AND artwork_bytes IS NULL AND coalesce(json_array_length(mosaic),0)<4
             UNION ALL SELECT 1,smart_playlist_key,mosaic IS NOT NULL FROM smart_playlists WHERE artwork_binding IS NULL AND artwork_bytes IS NULL AND coalesce(json_array_length(mosaic),0)<4
             UNION ALL SELECT 2,tag_key,mosaic IS NOT NULL FROM tags WHERE native_artwork IS NULL AND coalesce(json_array_length(mosaic),0)<4",
        )
        .fetch_all(&mut *connection)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(kind, key, picked)| {
                let owner = match kind {
                    0 => MosaicOwner::Playlist(PlaylistKey::from_raw(key)),
                    1 => MosaicOwner::Smart(SmartPlaylistKey::from_raw(key)),
                    _ => MosaicOwner::Tag(key),
                };
                (owner, picked)
            })
            .collect())
    }

    /// The genres and moods of these tracks.
    pub fn track_tags(&self, tracks: &[TrackKey]) -> Vec<i64> {
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot);
        let mut owned = Vec::new();
        let mut tags = std::collections::BTreeSet::new();
        for track in tracks {
            if let Some(record) = snapshot.tracks.get(track.raw()) {
                context.tags_of(record, &mut owned);
                tags.extend(
                    owned
                        .iter()
                        .map(|slot| i64::from(snapshot.tags.records[*slot as usize].key)),
                );
            }
        }
        tags.into_iter().collect()
    }

    /// Adds covers to the mosaics of these owners until each has four, keeping those it has,
    /// and stores them in one Write. A first pick is stored even when it finds none, so the owner
    /// counts as picked. The owners whose mosaic is still incomplete, and the revision of the
    /// Write when one was committed.
    pub async fn pick_mosaics(
        &self,
        owners: &[MosaicOwner],
    ) -> LibraryResult<(Vec<MosaicOwner>, Option<u64>)> {
        self.loaded().await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let mut connection = self.acquire_reader().await?;
        let mut picked = Vec::new();
        let mut incomplete = Vec::new();
        for owner in owners {
            let (table, column, key) = owner.row();
            let Some(current): Option<Option<String>> = sqlx::query_scalar(sqlx::AssertSqlSafe(
                format!("SELECT mosaic FROM {table} WHERE {column}=?1"),
            ))
            .bind(key)
            .fetch_optional(&mut *connection)
            .await?
            else {
                continue;
            };
            let picks: Vec<String> = current
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?
                .unwrap_or_default();
            if picks.len() >= COVERS {
                continue;
            }
            let collection = match *owner {
                MosaicOwner::Playlist(key) => Collection::Playlist(key),
                MosaicOwner::Smart(key) => Collection::Smart { key, now },
                // A mood's tracks hold it as they hold a genre.
                MosaicOwner::Tag(key) => Collection::Genre(crate::GenreKey::from_raw(key)),
            };
            let members = self.members(&mut connection, collection).await?;
            let next = crate::artwork::playlist_artwork_choices_on(
                self,
                &mut connection,
                &members,
                &picks,
                false,
            )
            .await?;
            if next.len() < COVERS {
                incomplete.push(*owner);
            }
            if next.len() > picks.len() || current.is_none() {
                picked.push((*owner, next));
            }
        }
        drop(connection);
        let revision = if picked.is_empty() {
            None
        } else {
            self.set_mosaics(&picked).await?
        };
        Ok((incomplete, revision))
    }

    /// Stores the covers of mosaics in one Write, and returns its revision.
    async fn set_mosaics(
        &self,
        mosaics: &[(MosaicOwner, Vec<String>)],
    ) -> LibraryResult<Option<u64>> {
        let mut write = self.write().await?;
        for (owner, picks) in mosaics {
            let (table, column, key) = owner.row();
            let source = if matches!(owner, MosaicOwner::Tag(_)) {
                "source_key"
            } else {
                "NULL"
            };
            let changed: Option<Option<crate::SourceKey>> =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "UPDATE {table} SET mosaic=?2 WHERE {column}=?1 AND mosaic IS NOT ?2 RETURNING {source}"
                )))
                .bind(key)
                .bind(serde_json::to_string(picks)?)
                .fetch_optional(write.connection().await?)
                .await?;
            match (owner, changed) {
                (_, None) | (MosaicOwner::Tag(_), Some(None)) => {}
                (MosaicOwner::Playlist(key), _) => write.changed(Scope::Playlist {
                    key: *key,
                    entries: false,
                }),
                (MosaicOwner::Smart(key), _) => write.changed(Scope::Smart { key: *key }),
                (MosaicOwner::Tag(_), Some(Some(source))) => {
                    write.changed(Scope::Collections { source })
                }
            }
        }
        Ok(write.commit().await?.map(|change| change.revision))
    }
}
