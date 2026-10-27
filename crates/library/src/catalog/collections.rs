use super::write::{Family, Write};
use super::*;
use crate::PlaylistEntryObservation;

/// Provider playlist headers, written as plain SQL and read back by key.
pub(super) async fn playlists(
    write: &mut Write<'_>,
    source: SourceKey,
    inputs: &[PlaylistObservation],
    admit: bool,
    reference: bool,
) -> LibraryResult<()> {
    for input in inputs {
        let known: Option<i64> = sqlx::query_scalar(
            "SELECT playlist_key FROM playlists WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(&input.object_id)
        .fetch_optional(write.connection().await?)
        .await?;
        if known.is_some_and(|key| write.newer(Family::Playlist, key)) {
            continue;
        }
        let key: Option<i64> = sqlx::query_scalar("WITH binding AS (
                SELECT CASE WHEN ?8 AND ?4 IS NULL THEN (SELECT artwork_binding FROM playlists WHERE source_key=?1 AND object_id=?2) ELSE ?4 END bytes
            ) INSERT INTO playlists(owner,source_key,object_id,name,artwork_binding,writable,metadata_writable,indexed) VALUES ('source',?1,?2,?3,(SELECT bytes FROM binding),coalesce(?5,1),coalesce(?6,1),?7) ON CONFLICT(source_key,object_id) DO UPDATE SET name=excluded.name,artwork_binding=excluded.artwork_binding,writable=coalesce(?5,playlists.writable),metadata_writable=coalesce(?6,playlists.metadata_writable),indexed=max(playlists.indexed,excluded.indexed) WHERE(playlists.name,playlists.artwork_binding,playlists.writable,playlists.metadata_writable,playlists.indexed) IS NOT(excluded.name,excluded.artwork_binding,coalesce(?5,playlists.writable),coalesce(?6,playlists.metadata_writable),max(playlists.indexed,excluded.indexed))
            RETURNING playlist_key")
            .bind(source).bind(&input.object_id).bind(&input.name).bind(&input.artwork_binding).bind(input.writable).bind(input.metadata_writable).bind(admit).bind(reference).fetch_optional(write.connection().await?).await?;
        if let Some(key) = key {
            write.touch_playlist(key);
            // The picture is not resident; the Change names it.
            write.changed(super::write::Scope::Playlist {
                key: crate::PlaylistKey::from_raw(key),
                entries: false,
            });
        }
        if let Some(key) = key.or(known) {
            write.see_playlist(key, source);
        }
    }
    Ok(())
}

impl Catalog {
    pub async fn source_contains_entity(
        &self,
        source_id: &str,
        kind: &str,
        id: &str,
        indexed: bool,
    ) -> LibraryResult<bool> {
        let mut connection = self.acquire_reader().await?;
        let (table, _) = if kind == "playlist" {
            ("playlists", "playlist_key")
        } else {
            super::table(kind)
        };
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT EXISTS(SELECT 1 FROM {table} JOIN sources USING(source_key) WHERE sources.object_id=?1 AND {table}.object_id=?2 AND(?3=0 OR {table}.indexed=1))"))).bind(source_id).bind(id).bind(indexed).fetch_one(&mut *connection).await?)
    }

    pub async fn unresolved_source_artist_ids(
        &self,
        source_id: &str,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT artist.object_id FROM artists artist JOIN sources USING(source_key) WHERE sources.object_id=?1 AND artist.native_metadata=0 ORDER BY artist.object_id LIMIT 100").bind(source_id).fetch_all(&mut *connection).await?)
    }

    pub async fn source_playlist_is_fresh(
        &self,
        source_id: &str,
        id: &str,
        revision: Option<&str>,
        now: i64,
    ) -> LibraryResult<bool> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM playlists JOIN sources USING(source_key) WHERE sources.object_id=?1 AND playlists.object_id=?2 AND provider_revision IS ?3 AND(?3 IS NOT NULL OR playlists.valid_until>=?4))").bind(source_id).bind(id).bind(revision).bind(now).fetch_one(&mut *connection).await?)
    }
}

impl super::Observation<'_> {
    pub async fn write_source_playlist_page(
        &mut self,
        id: &str,
        offset: usize,
        entries: &[PlaylistEntryObservation],
        complete: bool,
    ) -> LibraryResult<()> {
        let source_id = self.source_id;
        let mut write = self.write().await?;
        let source = self.source;
        if offset == 0 {
            self.playlist_entries.clear();
        }
        for page in entries.chunks(128) {
            // Entry tracks the source has not described yet are remembered by identity.
            let mut keys = std::collections::HashMap::new();
            let mut stubs = Vec::new();
            let objects = page
                .iter()
                .map(|entry| entry.track_id.as_str())
                .collect::<Vec<_>>();
            write
                .read_objects::<super::records::Track>(source, None, &objects)
                .await?;
            let uris = page
                .iter()
                .map(|entry| {
                    crate::source_entity_uri(&SourceId::new(source_id), "track", &entry.track_id)
                })
                .collect::<Vec<_>>();
            write
                .read_naturals::<super::records::Track>(
                    &uris.iter().map(String::as_str).collect::<Vec<_>>(),
                )
                .await?;
            for entry in page {
                if keys.contains_key(&entry.track_id) {
                    continue;
                }
                let uri =
                    crate::source_entity_uri(&SourceId::new(source_id), "track", &entry.track_id);
                let known = match write
                    .by_object::<super::records::Track>(source, None, &entry.track_id)
                    .await?
                {
                    Some(track) => Some(track),
                    None => write.by_natural::<super::records::Track>(&uri).await?,
                };
                match known {
                    Some(track) => {
                        keys.insert(&entry.track_id, track.key);
                    }
                    None => stubs.push((
                        super::records::Track::new(Some(source), Some(entry.track_id.clone()), uri),
                        serde_json::Map::new(),
                    )),
                }
            }
            for track in write.insert(stubs).await? {
                if let Some(entry) = page
                    .iter()
                    .find(|entry| Some(entry.track_id.as_str()) == track.object.as_deref())
                {
                    keys.insert(&entry.track_id, track.key);
                }
            }
            self.playlist_entries.extend(
                page.iter()
                    .map(|entry| (entry.object_id.clone(), keys[&entry.track_id])),
            );
        }
        if complete {
            let playlist: i64 = sqlx::query_scalar("SELECT playlist_key FROM playlists WHERE source_key=?1 AND object_id=?2 AND owner='source'").bind(source).bind(id).fetch_one(write.connection().await?).await?;
            // An entry a page repeats, as when pages shift while they are read, counts once.
            let mut seen = std::collections::HashSet::new();
            let mut entries = std::mem::take(&mut self.playlist_entries);
            entries.retain(|(object, _)| seen.insert(object.clone()));
            crate::playlists::write_entries(&mut write, playlist, &entries, false).await?;
        }
        self.commit(write).await?;
        Ok(())
    }
    pub async fn replace_source_favorites(
        &mut self,
        kind: &str,
        ids: &[String],
    ) -> LibraryResult<()> {
        async fn replace<R: super::write::Resident>(
            write: &mut Write<'_>,
            source: SourceKey,
            ids: &[String],
            state: fn(&mut R) -> &mut super::records::NativeState,
        ) -> LibraryResult<()> {
            let (table, key) = (R::TABLE.name, R::TABLE.key);
            let keys: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {key} FROM {table} WHERE source_key=?1 AND native_favorite IS NOT (object_id IN(SELECT value FROM json_each(?2)))")))
                .bind(source).bind(serde_json::to_string(ids)?).fetch_all(write.connection().await?).await?;
            write.read_keys::<R>(&keys).await?;
            for key in keys {
                if let Some(mut record) = write.get::<R>(key).await? {
                    let favorite = record
                        .object()
                        .is_some_and(|(_, _, object)| ids.iter().any(|id| id == object));
                    state(&mut record).favorite = Some(favorite);
                    write.put(record, serde_json::Map::new());
                }
            }
            Ok(())
        }
        let mut write = self.write().await?;
        match kind {
            "track" => {
                replace::<super::records::Track>(&mut write, self.source, ids, |track| {
                    &mut track.state
                })
                .await?
            }
            "album" => {
                replace::<super::records::Album>(&mut write, self.source, ids, |album| {
                    &mut album.state
                })
                .await?
            }
            "artist" => {
                replace::<super::records::Artist>(&mut write, self.source, ids, |artist| {
                    &mut artist.state
                })
                .await?
            }
            _ => {}
        }
        self.commit(write).await?;
        Ok(())
    }
    pub async fn write_source_playlist_freshness(
        &mut self,
        id: &str,
        revision: Option<&str>,
        valid_until: Option<i64>,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let source = self.source.raw();
        sqlx::query("UPDATE playlists SET provider_revision=?3,valid_until=?4 WHERE source_key=?1 AND object_id=?2 AND(provider_revision,valid_until) IS NOT(?3,?4)").bind(source).bind(id).bind(revision).bind(valid_until).execute(write.connection().await?).await?;
        // Freshness is read by key only, so clients hear nothing of it.
        write.keep();
        write.commit().await?;
        Ok(())
    }
    pub async fn create_observed_playlist(
        &mut self,
        observed: i64,
        name: &str,
    ) -> LibraryResult<crate::PlaylistKey> {
        let mut write = self.write().await?;
        let source = self.source.raw();
        let (key, _) =
            crate::playlists::create_playlist_identity(write.connection().await?, name).await?;
        let rank = crate::rank::nth_sql(
            "row_number() OVER(ORDER BY coalesce(details.cue_path,details.source_path),track.disc_number,track.track_number,track.object_id)-1",
        );
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO playlist_entries(playlist_key,object_id,track_key,rank) SELECT ?1,'rufin:entry:'||lower(hex(randomblob(16))),track.track_key,{rank} FROM tracks track LEFT JOIN track_details details USING(track_key) WHERE track.source_key=?2 AND track.local_file_key IN(SELECT local_file_key FROM local_files WHERE source_key=?2 AND observed_stamp=?3)"))).bind(key).bind(source).bind(observed).execute(write.connection().await?).await?;
        write.touch_playlist(key.raw());
        write.commit().await?;
        Ok(key)
    }
}
