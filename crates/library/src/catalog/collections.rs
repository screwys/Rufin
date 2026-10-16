use super::*;
use crate::{HomeEntryInput, NativeUserState, PlaylistEntryObservation};

pub(super) async fn headers(
    connection: &mut SqliteConnection,
    source: i64,
    source_id: &str,
    albums: &[&AlbumObservation],
    folders: &[FolderObservation],
    observed: Option<i64>,
) -> LibraryResult<bool> {
    let mut changed = false;
    super::cache::index_credits(
        connection,
        source,
        source_id,
        albums
            .iter()
            .flat_map(|album| &album.metadata.album_artists),
    )
    .await?;
    super::cache::index_tags(
        connection,
        source,
        source_id,
        "genre",
        albums.iter().flat_map(|album| &album.metadata.genres),
    )
    .await?;
    let input = serde_json::to_string(
        &albums
            .iter()
            .map(|album| [&album.object_id])
            .collect::<Vec<_>>(),
    )?;
    for (relation, kind, array) in [
        ("album_artists", "artist", "album_artists"),
        ("album_genres", "genre", "genres"),
    ] {
        let (table, key) = super::cache::table(kind);
        let expected = format!(
            "SELECT album.album_key,item.{key},CAST(edge.key AS INTEGER) position FROM json_each(?2) input CROSS JOIN albums album ON album.source_key=?1 AND album.object_id=json_extract(input.value,'$[0]') CROSS JOIN json_each(album.metadata,'$.{array}') edge CROSS JOIN {table} item ON item.source_key=?1 AND item.object_id=json_extract(edge.value,'$.object_id')"
        );
        changed |= super::cache::replace_edges(
            connection,
            source,
            relation,
            "album_key",
            key,
            &expected,
            &input,
            None,
        )
        .await?;
    }
    let expected = "WITH expected AS(SELECT album.album_key,edge.value release_type,CAST(edge.key AS INTEGER) position FROM json_each(?2) input CROSS JOIN albums album ON album.source_key=?1 AND album.object_id=json_extract(input.value,'$[0]') CROSS JOIN json_each(album.metadata,'$.release_types') edge) ";
    sqlx::query(sqlx::AssertSqlSafe(format!("{expected} DELETE FROM album_release_types WHERE album_key IN(SELECT album_key FROM albums WHERE source_key=?1 AND object_id IN(SELECT json_extract(value,'$[0]') FROM json_each(?2))) AND NOT EXISTS(SELECT 1 FROM expected WHERE expected.album_key=album_release_types.album_key AND expected.release_type=album_release_types.release_type AND expected.position=album_release_types.position)"))).bind(source).bind(&input).execute(&mut *connection).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("{expected} INSERT OR IGNORE INTO album_release_types SELECT album_key,release_type,position FROM expected"))).bind(source).bind(input).execute(&mut *connection).await?;
    for folder in folders {
        let rows = serde_json::to_string(&folder.tracks)?;
        changed|=sqlx::query("INSERT OR IGNORE INTO track_folders(track_key,folder_key,position,observed_stamp) SELECT track.track_key,folder.folder_key,coalesce((SELECT max(position)+1 FROM track_folders WHERE track_key=track.track_key),0),coalesce(?4,0) FROM json_each(?3) item CROSS JOIN tracks track ON track.source_key=?1 AND track.object_id=item.value CROSS JOIN folders folder ON folder.source_key=?1 AND folder.object_id=?2 WHERE folder.observed_stamp<=coalesce(?4,9223372036854775807)").bind(source).bind(&folder.object_id).bind(&rows).bind(observed).execute(&mut *connection).await?.rows_affected()>0;
        sqlx::query("UPDATE track_folders SET observed_stamp=?4 WHERE folder_key=(SELECT folder_key FROM folders WHERE source_key=?1 AND object_id=?2 AND observed_stamp<=coalesce(?4,9223372036854775807)) AND track_key IN(SELECT track.track_key FROM json_each(?3) item CROSS JOIN tracks track ON track.source_key=?1 AND track.object_id=item.value) AND observed_stamp<?4").bind(source).bind(&folder.object_id).bind(rows).bind(observed).execute(&mut *connection).await?;
    }
    Ok(changed)
}

pub(super) async fn playlists(
    connection: &mut SqliteConnection,
    source: i64,
    inputs: &[PlaylistObservation],
    observed: Option<i64>,
    admit: bool,
    reference: bool,
) -> LibraryResult<bool> {
    let mut changed = false;
    for input in inputs {
        changed|=sqlx::query("WITH previous AS (SELECT artwork_binding FROM playlists WHERE source_key=?1 AND object_id=?2),
            candidates AS (
                SELECT value FROM json_each(?5)
                UNION ALL SELECT candidate.value FROM previous CROSS JOIN json_each(previous.artwork_binding) candidate
                WHERE json_extract(candidate.value,'$.image.automatic') IS NOT NULL
                  AND NOT EXISTS(SELECT 1 FROM json_each(?5) incoming WHERE json_extract(incoming.value,'$.image.automatic') IS NOT NULL)
            ), binding AS (
                SELECT CASE WHEN ?10 AND ?5 IS NULL THEN (SELECT artwork_binding FROM previous)
                    ELSE CAST(nullif(json_group_array(json(value)),'[]') AS BLOB) END bytes FROM candidates
            ) INSERT INTO playlists(owner,source_key,object_id,name,normalized_name,sort_text,artwork_binding,writable,metadata_writable,position,indexed,observed_stamp)
            VALUES('source',?1,?2,?3,lower(?3),?4,(SELECT bytes FROM binding),coalesce(?6,1),coalesce(?7,1),(SELECT coalesce(max(position),-1)+1 FROM playlists),?8,coalesce(?9,0))
            ON CONFLICT(source_key,object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text,artwork_binding=excluded.artwork_binding,writable=coalesce(?6,playlists.writable),metadata_writable=coalesce(?7,playlists.metadata_writable),indexed=max(playlists.indexed,excluded.indexed),observed_stamp=coalesce(?9,playlists.observed_stamp) WHERE(playlists.name,playlists.sort_text,playlists.artwork_binding,playlists.writable,playlists.metadata_writable,playlists.indexed) IS NOT(excluded.name,excluded.sort_text,excluded.artwork_binding,coalesce(?6,playlists.writable),coalesce(?7,playlists.metadata_writable),max(playlists.indexed,excluded.indexed))")
            .bind(source).bind(&input.object_id).bind(&input.name).bind(&input.sort_text).bind(&input.artwork_binding).bind(input.writable).bind(input.metadata_writable).bind(admit).bind(observed).bind(reference).execute(&mut *connection).await?.rows_affected()>0;
        if let Some(stamp) = observed {
            sqlx::query("UPDATE playlists SET observed_stamp=?3 WHERE source_key=?1 AND object_id=?2 AND observed_stamp<?3").bind(source).bind(&input.object_id).bind(stamp).execute(&mut *connection).await?;
        }
    }
    Ok(changed)
}

impl Database {
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
            super::cache::table(kind)
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

    pub async fn omitted_album_track_page(
        &self,
        source_id: &str,
        id: &str,
        observed: i64,
        after: Option<&str>,
        limit: usize,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT track.object_id FROM tracks track JOIN albums album USING(album_key) JOIN sources ON sources.source_key=track.source_key WHERE sources.object_id=?1 AND album.object_id=?2 AND track.indexed=1 AND track.observed_stamp<?3 AND track.object_id>coalesce(?4,'') ORDER BY track.object_id LIMIT ?5").bind(source_id).bind(id).bind(observed).bind(after).bind(limit.min(128) as i64).fetch_all(&mut *connection).await?)
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

impl super::Catalog<'_> {
    pub async fn replace_source_home_section(
        &mut self,
        section: &str,
        title: Option<&str>,
        entries: &[HomeEntryInput],
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let source = self.source.raw();
        let mut update = CatalogUpdate::default();
        let rows = serde_json::to_string(
            &entries
                .iter()
                .map(|entry| {
                    (
                        entry.position,
                        entry.kind.as_str(),
                        &entry.entity_object_id,
                        &entry.title,
                        &entry.subtitle,
                    )
                })
                .collect::<Vec<_>>(),
        )?;
        let expected = "WITH expected AS(SELECT json_extract(value,'$[0]') position,json_extract(value,'$[1]') entity_kind,CASE json_extract(value,'$[1]') WHEN 'track' THEN(SELECT track_key FROM tracks WHERE source_key=?1 AND object_id=json_extract(value,'$[2]')) WHEN 'album' THEN(SELECT album_key FROM albums WHERE source_key=?1 AND object_id=json_extract(value,'$[2]')) WHEN 'artist' THEN(SELECT artist_key FROM artists WHERE source_key=?1 AND object_id=json_extract(value,'$[2]')) WHEN 'playlist' THEN(SELECT playlist_key FROM playlists WHERE source_key=?1 AND object_id=json_extract(value,'$[2]')) END entity_key,json_extract(value,'$[3]') title,json_extract(value,'$[4]') subtitle FROM json_each(?3)) ";
        update.home|=sqlx::query(sqlx::AssertSqlSafe(format!("{expected} DELETE FROM home_entries WHERE source_key=?1 AND section_id=?2 AND NOT EXISTS(SELECT 1 FROM expected WHERE expected.entity_key IS NOT NULL AND expected.position=home_entries.position AND(expected.entity_kind,expected.entity_key,expected.title,expected.subtitle,?4) IS(home_entries.entity_kind,home_entries.entity_key,home_entries.title,home_entries.subtitle,home_entries.section_title))"))).bind(source).bind(section).bind(&rows).bind(title).execute(&mut *tx).await?.rows_affected()>0;
        update.home|=sqlx::query(sqlx::AssertSqlSafe(format!("{expected} INSERT OR IGNORE INTO home_entries(source_key,section_id,position,entity_kind,entity_key,title,subtitle,section_title) SELECT ?1,?2,position,entity_kind,entity_key,title,subtitle,?4 FROM expected WHERE entity_key IS NOT NULL"))).bind(source).bind(section).bind(rows).bind(title).execute(&mut *tx).await?.rows_affected()>0;
        tx.commit().await?;
        self.changes.merge(update);
        Ok(())
    }
    pub async fn write_source_playlist_page(
        &mut self,
        id: &str,
        offset: usize,
        entries: &[PlaylistEntryObservation],
        complete: bool,
    ) -> LibraryResult<()> {
        let source_id = self.source_id;
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let source = self.source.raw();
        let playlist:i64=sqlx::query_scalar("SELECT playlist_key FROM playlists WHERE source_key=?1 AND object_id=?2 AND owner='source'").bind(source).bind(id).fetch_one(&mut *tx).await?;
        let mut update = CatalogUpdate::default();
        if offset == 0 {
            sqlx::query("DELETE FROM native_playlist_pending WHERE playlist_key=?1")
                .bind(playlist)
                .execute(&mut *tx)
                .await?;
        }
        for (page_index, page) in entries.chunks(128).enumerate() {
            let rows = serde_json::to_string(
                &page
                    .iter()
                    .enumerate()
                    .map(|(position, entry)| {
                        (
                            &entry.object_id,
                            &entry.track_id,
                            offset + page_index * 128 + position,
                            crate::source_entity_uri(
                                &SourceId::new(source_id),
                                "track",
                                &entry.track_id,
                            ),
                        )
                    })
                    .collect::<Vec<_>>(),
            )?;
            sqlx::query("INSERT INTO tracks(source_key,object_id,media_uri,indexed) SELECT ?1,json_extract(value,'$[1]'),json_extract(value,'$[3]'),0 FROM json_each(?2) WHERE true ON CONFLICT(source_key,object_id) DO NOTHING").bind(source).bind(&rows).execute(&mut *tx).await?;
            sqlx::query("INSERT OR REPLACE INTO native_playlist_pending(playlist_key,object_id,track_key,position) SELECT ?1,json_extract(item.value,'$[0]'),track.track_key,json_extract(item.value,'$[2]') FROM json_each(?2) item CROSS JOIN tracks track ON track.source_key=?3 AND track.object_id=json_extract(item.value,'$[1]')").bind(playlist).bind(rows).bind(source).execute(&mut *tx).await?;
        }
        if complete {
            update.playlists=sqlx::query_scalar("SELECT EXISTS(SELECT object_id,track_key,position FROM playlist_entries WHERE playlist_key=?1 EXCEPT SELECT object_id,track_key,position FROM native_playlist_pending WHERE playlist_key=?1) OR EXISTS(SELECT object_id,track_key,position FROM native_playlist_pending WHERE playlist_key=?1 EXCEPT SELECT object_id,track_key,position FROM playlist_entries WHERE playlist_key=?1)").bind(playlist).fetch_one(&mut *tx).await?;
            if update.playlists {
                sqlx::query("DELETE FROM playlist_entries WHERE playlist_key=?1 AND object_id NOT IN(SELECT object_id FROM native_playlist_pending WHERE playlist_key=?1)").bind(playlist).execute(&mut *tx).await?;
                sqlx::query("UPDATE playlist_entries SET position=position+(SELECT coalesce(max(position),0)+1 FROM playlist_entries WHERE playlist_key=?1)+(SELECT coalesce(max(position),0)+1 FROM native_playlist_pending WHERE playlist_key=?1) WHERE playlist_key=?1").bind(playlist).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO playlist_entries(playlist_key,object_id,track_key,position) SELECT playlist_key,object_id,track_key,row_number() OVER(ORDER BY position,object_id)-1 FROM native_playlist_pending WHERE playlist_key=?1 ON CONFLICT(playlist_key,object_id) DO UPDATE SET track_key=excluded.track_key,position=excluded.position").bind(playlist).execute(&mut *tx).await?;
            }
            sqlx::query("DELETE FROM native_playlist_pending WHERE playlist_key=?1")
                .bind(playlist)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        self.changes.merge(update);
        Ok(())
    }
    pub async fn write_native_user_data(
        &mut self,
        kind: &str,
        rows: &[(String, NativeUserState)],
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let source = self.source.raw();
        let update = super::cache::users(
            &mut tx,
            source,
            kind,
            &rows
                .iter()
                .map(|(id, state)| (id.as_str(), state))
                .collect::<Vec<_>>(),
            None,
        )
        .await?;
        tx.commit().await?;
        self.changes.merge(update);
        Ok(())
    }
    pub async fn replace_source_favorites(
        &mut self,
        kind: &str,
        ids: &[String],
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let source = self.source.raw();
        let (table, _) = super::cache::table(kind);
        let mut update = CatalogUpdate::default();
        update.metadata=sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO native_media_state(media_uri,favorite) SELECT media_uri,object_id IN(SELECT value FROM json_each(?2)) FROM {table} WHERE source_key=?1 ON CONFLICT(media_uri) DO UPDATE SET favorite=excluded.favorite WHERE favorite IS NOT excluded.favorite"))).bind(source).bind(serde_json::to_string(ids)?).execute(connection).await?.rows_affected()>0;
        self.changes.merge(update);
        Ok(())
    }
    pub async fn remove_source_items(&mut self, kind: &str, ids: &[String]) -> LibraryResult<()> {
        let (table, key) = if kind == "playlist" {
            ("playlists", "playlist_key")
        } else {
            super::cache::table(kind)
        };
        let mut update = CatalogUpdate::default();
        for page in ids.chunks(128) {
            let mut writer = self.database.writer().await?;
            let mut tx = writer
                .as_mut()
                .ok_or(LibraryError::WriterUnavailable)?
                .begin()
                .await?;
            let source = self.source.raw();
            let result = &mut update;
            let observed:i64=sqlx::query_scalar("UPDATE sources SET observation_stamp=observation_stamp+1 WHERE source_key=?1 RETURNING observation_stamp").bind(source).fetch_one(&mut *tx).await?;
            let keys:Vec<i64>=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {key} FROM {table} WHERE source_key=?1 AND indexed=1 AND object_id IN(SELECT value FROM json_each(?2))"))).bind(source).bind(serde_json::to_string(page)?).fetch_all(&mut *tx).await?;
            let albums: Vec<i64> = if kind == "track" {
                sqlx::query_scalar("SELECT DISTINCT album_key FROM tracks WHERE track_key IN(SELECT value FROM json_each(?1)) AND album_key IS NOT NULL").bind(serde_json::to_string(&keys)?).fetch_all(&mut *tx).await?
            } else {
                Vec::new()
            };
            let changed=sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET indexed=0,observed_stamp=?3 WHERE source_key=?1 AND object_id IN(SELECT value FROM json_each(?2))"))).bind(source).bind(serde_json::to_string(page)?).bind(observed).execute(&mut *tx).await?.rows_affected()>0;
            if kind == "playlist" {
                result.playlists |= !keys.is_empty();
            } else {
                result.metadata |= !keys.is_empty();
            }
            if changed {
                let album_keys = albums
                    .iter()
                    .copied()
                    .map(crate::AlbumKey::from_raw)
                    .collect::<Vec<_>>();
                crate::loudness::invalidate_album_audio(&mut tx, &album_keys, true).await?;
                super::files::album_order(&mut tx, if kind == "album" { &keys } else { &albums })
                    .await?;
                super::artwork::refresh_choices(
                    &mut tx,
                    if kind == "track" { &keys } else { &[] },
                    if kind == "album" { &keys } else { &albums },
                    if kind == "artist" { &keys } else { &[] },
                    false,
                )
                .await?;
            }
            tx.commit().await?;
            self.changes.merge(update.clone());
        }
        self.changes.merge(update);
        Ok(())
    }
    pub async fn complete_album_observation(
        &mut self,
        id: &str,
        observed: i64,
    ) -> LibraryResult<()> {
        let mut update = CatalogUpdate::default();
        loop {
            let mut writer = self.database.writer().await?;
            let mut tx = writer
                .as_mut()
                .ok_or(LibraryError::WriterUnavailable)?
                .begin()
                .await?;
            let source = self.source.raw();
            let result = &mut update;
            let keys:Vec<i64>=sqlx::query_scalar("SELECT track_key FROM tracks WHERE source_key=?1 AND album_key=(SELECT album_key FROM albums WHERE source_key=?1 AND object_id=?2) AND observed_stamp<?3 ORDER BY track_key LIMIT 128").bind(source).bind(id).bind(observed).fetch_all(&mut *tx).await?;
            if keys.is_empty() {
                tx.commit().await?;
                self.changes.merge(update.clone());
                break;
            }
            let albums:Vec<i64>=sqlx::query_scalar("SELECT DISTINCT album_key FROM tracks WHERE track_key IN(SELECT value FROM json_each(?1)) AND album_key IS NOT NULL").bind(serde_json::to_string(&keys)?).fetch_all(&mut *tx).await?;
            sqlx::query("UPDATE tracks SET album_key=NULL,observed_stamp=?2 WHERE track_key IN(SELECT value FROM json_each(?1))").bind(serde_json::to_string(&keys)?).bind(observed).execute(&mut *tx).await?;
            let album_keys = albums
                .iter()
                .copied()
                .map(crate::AlbumKey::from_raw)
                .collect::<Vec<_>>();
            crate::loudness::invalidate_album_audio(&mut tx, &album_keys, true).await?;
            super::files::album_order(&mut tx, &albums).await?;
            super::artwork::refresh_choices(&mut tx, &keys, &albums, &[], false).await?;
            result.metadata = true;
            tx.commit().await?;
            self.changes.merge(update.clone());
        }
        self.changes.merge(update);
        Ok(())
    }
    pub async fn write_source_playlist_freshness(
        &mut self,
        id: &str,
        revision: Option<&str>,
        valid_until: Option<i64>,
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let source = self.source.raw();
        sqlx::query("UPDATE playlists SET provider_revision=?3,valid_until=?4 WHERE source_key=?1 AND object_id=?2 AND(provider_revision,valid_until) IS NOT(?3,?4)").bind(source).bind(id).bind(revision).bind(valid_until).execute(connection).await?;
        Ok(())
    }
    pub async fn create_observed_playlist(
        &mut self,
        observed: i64,
        name: &str,
    ) -> LibraryResult<crate::PlaylistKey> {
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let source = self.source.raw();
        let (key, _) = crate::playlists::create_playlist_identity(&mut tx, None, name).await?;
        sqlx::query("INSERT INTO playlist_entries(playlist_key,object_id,track_key,position) SELECT ?1,'rufin:entry:'||lower(hex(randomblob(16))),track_key,row_number() OVER(ORDER BY coalesce(cue_path,source_path),disc_number,track_number,object_id)-1 FROM tracks WHERE source_key=?2 AND local_file_key IN(SELECT local_file_key FROM local_files WHERE source_key=?2 AND observed_stamp=?3)").bind(key).bind(source).bind(observed).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(key)
    }
    pub async fn complete_source_folder_membership(
        &mut self,
        id: &str,
        observed: i64,
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let source = self.source.raw();
        let mut update = CatalogUpdate::default();
        update.metadata=sqlx::query("DELETE FROM track_folders WHERE folder_key=(SELECT folder_key FROM folders WHERE source_key=?1 AND object_id=?2) AND observed_stamp<?3").bind(source).bind(id).bind(observed).execute(connection).await?.rows_affected()>0;
        self.changes.merge(update);
        Ok(())
    }
}

impl crate::Catalog<'_> {
    pub async fn complete_source_home_sections(
        &mut self,
        prefix: &str,
        sections: &[String],
        retained_prefixes: &[String],
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let connection = writer
            .as_mut()
            .ok_or(crate::LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let source = self.source;
        let changed = sqlx::query("DELETE FROM home_entries WHERE source_key=?1 AND substr(section_id,1,length(?2))=?2 AND section_id NOT IN(SELECT value FROM json_each(?3)) AND NOT EXISTS(SELECT 1 FROM json_each(?4) retained WHERE substr(section_id,1,length(retained.value))=retained.value)")
            .bind(source).bind(prefix).bind(serde_json::to_string(sections)?).bind(serde_json::to_string(retained_prefixes)?)
            .execute(&mut *transaction).await?.rows_affected()>0;
        if changed {
            sqlx::query(
                "UPDATE sources SET catalog_revision=catalog_revision+1 WHERE source_key=?1",
            )
            .bind(source)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        self.changes.home |= changed;
        Ok(())
    }
}
