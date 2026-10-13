use super::*;

impl Scan {
    pub async fn retain_playlists(&mut self) -> LibraryResult<()> {
        self.execute(sqlx::query("INSERT OR IGNORE INTO temp.scan_seen(kind,object_id,written) SELECT 'playlist',object_id,0 FROM playlists WHERE source_key=?1 AND owner='source' AND indexed").bind(self.source)).await?;
        Ok(())
    }

    pub async fn retain_playlist_entries_if_fresh(
        &mut self,
        object_id: &str,
        revision: &str,
        now: i64,
    ) -> LibraryResult<bool> {
        let mut writer = if self.batch_writer.is_none() {
            Some(self.database.writer().await?)
        } else {
            None
        };
        let connection = if let Some(batch) = &mut self.batch_writer {
            batch.as_mut().ok_or(LibraryError::WriterUnavailable)?
        } else {
            writer
                .as_mut()
                .unwrap()
                .as_mut()
                .ok_or(LibraryError::WriterUnavailable)?
        };
        let retained = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM playlists WHERE owner='source' AND source_key=?1 AND object_id=?2 AND provider_revision=?3 AND valid_until>?4)")
            .bind(self.source).bind(object_id).bind(revision).bind(now).fetch_one(connection).await?;
        drop(writer);
        if retained {
            self.execute(sqlx::query("UPDATE temp.scan_artwork AS image SET source=COALESCE(image.source,cached.artwork_binding),binding=COALESCE(image.binding,cached.artwork_binding) FROM playlists cached WHERE cached.owner='source' AND image.kind='playlist' AND image.object_id=?2 AND cached.object_id=image.object_id AND cached.source_key=?1").bind(self.source).bind(object_id)).await?;
        }
        Ok(retained)
    }

    pub async fn write_playlist_freshness(
        &mut self,
        object_id: &str,
        revision: Option<&str>,
        valid_until: Option<i64>,
    ) -> LibraryResult<()> {
        self.execute(sqlx::query("UPDATE playlists SET provider_revision=?3,valid_until=?4 WHERE owner='source' AND source_key=?1 AND object_id=?2 AND (provider_revision,valid_until) IS NOT (?3,?4)").bind(self.source).bind(object_id).bind(revision).bind(valid_until)).await
    }

    pub async fn write_playlist(&mut self, input: &PlaylistInput) -> LibraryResult<()> {
        let object_id = input.object_id.as_str();
        let normalized_name = input.name.to_lowercase();
        self.require_id("playlist", object_id)?;
        let first = self
            .observe_entities("playlist", &[object_id])
            .await?
            .contains(object_id);
        self.changes.playlists |= self.execute_result(sqlx::query("INSERT INTO playlists(source_key,owner,object_id,name,normalized_name,sort_text,position)
            VALUES(?1,'source',?2,?3,?4,?5,(SELECT COALESCE(max(position)+1,0) FROM playlists))
            ON CONFLICT(source_key,object_id) DO UPDATE SET owner='source',indexed=1,name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text
            WHERE (owner,indexed,name,normalized_name,sort_text) IS NOT ('source',1,excluded.name,excluded.normalized_name,excluded.sort_text)")
            .bind(self.source).bind(object_id).bind(&input.name).bind(normalized_name).bind(&input.sort_text)).await?.rows_affected()>0;
        self.remember_artworks(
            "playlist",
            &[(object_id, input.artwork_binding.as_deref(), first)],
        )
        .await?;
        if input.writable.is_some() || input.metadata_writable.is_some() {
            self.changes.playlists |= self.execute_result(sqlx::query("UPDATE playlists SET writable=COALESCE(?3,writable),metadata_writable=COALESCE(?4,metadata_writable) WHERE owner='source' AND source_key=?1 AND object_id=?2 AND ((?3 IS NOT NULL AND writable IS NOT ?3) OR (?4 IS NOT NULL AND metadata_writable IS NOT ?4))").bind(self.source).bind(object_id).bind(input.writable).bind(input.metadata_writable)).await?.rows_affected()>0;
        }
        Ok(())
    }

    pub async fn write_playlist_page(
        &mut self,
        playlist_id: &str,
        offset: usize,
        entries: &[PlaylistEntryInput],
        complete: bool,
    ) -> LibraryResult<()> {
        if self.failed {
            return Err(LibraryError::ScanFailed);
        }
        self.require_id("playlist", playlist_id)?;
        for entry in entries {
            self.require_id("playlist entry", &entry.object_id)?;
            self.require_id("track", &entry.track_id)?;
            self.require_row_bytes(&[entry.object_id.as_bytes(), entry.track_id.as_bytes()])?;
        }
        let owns_batch = self.batch_writer.is_none();
        if owns_batch {
            self.begin_batch().await?;
        }
        let result: LibraryResult<bool> = async {
            let source_id = SourceId::new(&self.source_id);
            if offset == 0 {
                self.execute(sqlx::query("DELETE FROM temp.playlist_read")).await?;
            }
            for (chunk, entries) in entries.chunks(128).enumerate() {
                let rows = entries.iter().enumerate().map(|(position, entry)| (
                    &entry.object_id, &entry.track_id,
                    source_entity_uri(&source_id, "track", &entry.track_id),
                    offset + chunk * 128 + position,
                )).collect::<Vec<_>>();
                let rows = serde_json::to_string(&rows)?;
                self.execute(sqlx::query("INSERT INTO tracks(source_key,object_id,media_uri,indexed,normalized_search,sort_text,sort_album)
                    SELECT DISTINCT ?1,json_extract(value,'$[1]'),json_extract(value,'$[2]'),0,
                        lower(json_extract(value,'$[2]'))||'    ',lower(json_extract(value,'$[2]')),''
                    FROM json_each(?2) request
                    WHERE NOT EXISTS(SELECT 1 FROM tracks WHERE source_key=?1 AND object_id=json_extract(request.value,'$[1]'))
                    ON CONFLICT(media_uri) DO UPDATE SET source_key=COALESCE(tracks.source_key,excluded.source_key),object_id=COALESCE(tracks.object_id,excluded.object_id)
                    WHERE tracks.source_key IS NULL OR tracks.object_id IS NULL")
                    .bind(self.source).bind(&rows)).await?;
                self.execute(sqlx::query("INSERT INTO temp.playlist_read(object_id,track_key,position)
                    SELECT json_extract(request.value,'$[0]'),track.track_key,json_extract(request.value,'$[3]')
                    FROM json_each(?2) request CROSS JOIN tracks track ON track.source_key=?1 AND track.object_id=json_extract(request.value,'$[1]')
                    WHERE true ON CONFLICT(object_id) DO NOTHING")
                    .bind(self.source).bind(rows)).await?;
            }
            let mut changed = false;
            if complete {
                let playlist: i64 = sqlx::query_scalar("SELECT playlist_key FROM playlists WHERE source_key=?1 AND object_id=?2 AND owner='source'")
                    .bind(self.source).bind(playlist_id).fetch_one(self.batch_writer.as_mut().unwrap().as_mut().ok_or(LibraryError::WriterUnavailable)?).await?;
                self.execute(sqlx::query("WITH ranked AS (SELECT object_id,row_number() OVER(ORDER BY position,object_id)-1 position FROM temp.playlist_read)
                    UPDATE temp.playlist_read SET position=ranked.position FROM ranked WHERE playlist_read.object_id=ranked.object_id AND playlist_read.position IS NOT ranked.position")
                    ).await?;
                changed |= self.execute_result(sqlx::query("DELETE FROM playlist_entries WHERE playlist_key=?1 AND NOT EXISTS(SELECT 1 FROM temp.playlist_read WHERE object_id=playlist_entries.object_id)")
                    .bind(playlist)).await?.rows_affected()>0;
                self.execute(sqlx::query("UPDATE playlist_entries SET position=position + (
                        SELECT max(COALESCE((SELECT max(position) FROM playlist_entries WHERE playlist_key=?1),0),
                            COALESCE((SELECT max(position) FROM temp.playlist_read),0))+1)
                    WHERE playlist_key=?1 AND EXISTS(SELECT 1 FROM temp.playlist_read supplied
                        LEFT JOIN playlist_entries current ON current.playlist_key=?1 AND current.object_id=supplied.object_id
                        WHERE supplied.position IS NOT current.position)")
                    .bind(playlist)).await?;
                changed |= self.execute_result(sqlx::query("INSERT INTO playlist_entries(playlist_key,object_id,track_key,position)
                    SELECT ?1,object_id,track_key,position FROM temp.playlist_read WHERE true
                    ON CONFLICT(playlist_key,object_id) DO UPDATE SET track_key=excluded.track_key,position=excluded.position
                    WHERE (playlist_entries.track_key,playlist_entries.position) IS NOT (excluded.track_key,excluded.position)")
                    .bind(playlist)).await?.rows_affected()>0;
                self.execute(sqlx::query("DELETE FROM temp.playlist_read")).await?;
            }
            Ok(changed)
        }.await;
        if result.is_err() {
            self.failed = true;
        }
        if owns_batch {
            self.finish_batch().await?;
        }
        self.changes.playlists |= result?;
        Ok(())
    }
}
