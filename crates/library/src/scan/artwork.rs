use sqlx::SqliteConnection;

use super::{LocalArtworkCandidate, Scan};
use crate::{ArtistArtworkBinding, LibraryError, LibraryResult};

impl Scan {
    pub(super) async fn remember_artworks(
        &mut self,
        kind: &str,
        rows: &[(&str, Option<&[u8]>, bool)],
    ) -> LibraryResult<()> {
        for page in rows.chunks(128) {
            let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                "WITH input(kind,object_id,binding,first) AS (",
            );
            query.push_values(page, |mut row, (id, binding, first)| {
                row.push_bind(kind)
                    .push_bind(*id)
                    .push_bind(*binding)
                    .push_bind(*first);
            });
            query.push(
                ") INSERT INTO temp.scan_artwork(kind,object_id,source,binding)
                SELECT kind,object_id,binding,binding FROM input WHERE true
                ON CONFLICT(kind,object_id) DO UPDATE SET
                  source=CASE WHEN (SELECT first FROM input WHERE object_id=excluded.object_id)
                    THEN excluded.source ELSE COALESCE(excluded.source,scan_artwork.source) END,
                  binding=CASE WHEN (SELECT first FROM input WHERE object_id=excluded.object_id)
                    THEN excluded.binding ELSE COALESCE(excluded.binding,scan_artwork.binding) END",
            );
            self.execute(query.build()).await?;
        }
        Ok(())
    }

    pub async fn clear_local_artwork_candidates(&mut self) -> LibraryResult<()> {
        self.local_point_update = self.point_update;
        for (kind, table) in [
            ("album", "albums"),
            ("track", "tracks"),
            ("artist", "artists"),
        ] {
            self.execute(sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT OR IGNORE INTO temp.scan_artwork(kind,object_id,source,binding)
                 SELECT ?2,entity.object_id,entity.artwork_binding,entity.artwork_binding
                 FROM {table} entity JOIN temp.scan_seen seen ON seen.kind=?2 AND seen.object_id=entity.object_id
                 WHERE entity.source_key=?1"
            ))).bind(self.source).bind(kind)).await?;
        }
        if self.point_update {
            // Revisit albums that borrowed an image from an artist whose files changed.
            let mut after = String::new();
            loop {
                let rows: Vec<(String, Option<Vec<u8>>)> = {
                    let mut writer = self.database.writer().await?;
                    let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
                    sqlx::query_as("SELECT artist.object_id,artist.artwork_binding FROM artists artist
                        JOIN temp.scan_artwork image ON image.kind='artist' AND image.object_id=artist.object_id
                        WHERE artist.source_key=?1 AND artist.object_id>?2 ORDER BY artist.object_id LIMIT 128")
                        .bind(self.source).bind(&after).fetch_all(connection).await?
                };
                if rows.is_empty() {
                    break;
                }
                for (id, binding) in rows {
                    after = id.clone();
                    let (source, fallback) = match binding.as_deref().and_then(|bytes| {
                        serde_json::from_slice::<ArtistArtworkBinding>(bytes).ok()
                    }) {
                        Some(binding) => (binding.source, binding.fallback),
                        None => (binding, None),
                    };
                    self.execute(sqlx::query("UPDATE temp.scan_artwork SET source=?2,binding=?3 WHERE kind='artist' AND object_id=?1")
                        .bind(id).bind(source).bind(fallback)).await?;
                }
            }
            self.execute(sqlx::query("INSERT OR IGNORE INTO temp.scan_artwork(kind,object_id)
                SELECT 'album',album.object_id FROM albums album WHERE album.source_key=?1
                  AND album.artwork_binding IN (SELECT source FROM temp.scan_artwork WHERE kind='artist'
                      UNION SELECT binding FROM temp.scan_artwork WHERE kind='artist')")
                .bind(self.source)).await?;
            self.execute(
                sqlx::query(
                    "INSERT OR IGNORE INTO temp.scan_artwork(kind,object_id)
                SELECT 'track',track.object_id FROM temp.scan_artwork image
                JOIN albums album ON album.source_key=?1 AND album.object_id=image.object_id
                JOIN tracks track USING(album_key) WHERE image.kind='album'",
                )
                .bind(self.source),
            )
            .await?;
            self.execute(
                sqlx::query(
                    "INSERT OR IGNORE INTO temp.scan_artwork(kind,object_id)
                SELECT 'artist',artist.object_id FROM artists artist WHERE artist.artist_key IN (
                    SELECT relation.artist_key FROM temp.scan_artwork image
                    JOIN albums album ON album.source_key=?1 AND album.object_id=image.object_id
                    JOIN album_artists relation USING(album_key) WHERE image.kind='album'
                    UNION SELECT relation.artist_key FROM temp.scan_artwork image
                    JOIN tracks track ON track.source_key=?1 AND track.object_id=image.object_id
                    JOIN track_artists relation USING(track_key) WHERE image.kind='track')",
                )
                .bind(self.source),
            )
            .await?;
        }
        self.execute(sqlx::query("UPDATE temp.scan_artwork SET source=NULL,binding=NULL WHERE kind IN ('album','track','artist')")).await
    }

    pub async fn local_track_file(
        &self,
        media_uri: &str,
    ) -> LibraryResult<Option<(String, String)>> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query_as("SELECT object_id,source_path FROM tracks WHERE media_uri=?1 AND source_path IS NOT NULL")
            .bind(media_uri).fetch_optional(connection).await?)
    }

    pub async fn local_artwork_album_page(&self, after: &str) -> LibraryResult<Vec<String>> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query_scalar(
            "SELECT image.object_id FROM temp.scan_artwork image
            JOIN albums album ON album.source_key=?1 AND album.object_id=image.object_id
            WHERE image.kind='album' AND image.object_id>?2 ORDER BY image.object_id LIMIT 128",
        )
        .bind(self.source)
        .bind(after)
        .fetch_all(connection)
        .await?)
    }

    pub async fn local_artwork_artist_page(
        &self,
        after: &str,
    ) -> LibraryResult<Vec<(String, String)>> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query_as(
            "SELECT artist.object_id,artist.name FROM temp.scan_artwork image
            JOIN artists artist ON artist.source_key=?1 AND artist.object_id=image.object_id
            WHERE image.kind='artist' AND artist.object_id>?2 ORDER BY artist.object_id LIMIT 128",
        )
        .bind(self.source)
        .bind(after)
        .fetch_all(connection)
        .await?)
    }

    pub async fn local_artwork_directory_has_artist(
        &self,
        prefix: &str,
        artist: &str,
    ) -> LibraryResult<bool> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query_scalar("WITH directory_tracks AS (
                SELECT track.album_key FROM tracks track WHERE track.source_key=?3
                  AND track.source_path>=?1 AND track.source_path<?1||char(1114111)
                  AND (EXISTS(SELECT 1 FROM temp.scan_seen WHERE kind='track' AND object_id=track.object_id)
                  OR (?4 AND track.indexed AND NOT EXISTS(SELECT 1 FROM temp.scan_removals WHERE entity_kind='track' AND object_id=track.object_id)
                      AND NOT EXISTS(SELECT 1 FROM temp.scan_local_file_removals WHERE path=track.source_path)))
             ) SELECT EXISTS(SELECT 1 FROM directory_tracks) AND NOT EXISTS(
                SELECT 1 FROM directory_tracks track WHERE NOT EXISTS(
                    SELECT 1 FROM album_artists relation JOIN artists artist USING(artist_key)
                    WHERE relation.album_key=track.album_key AND artist.object_id=?2
                      AND relation.position=(SELECT min(position) FROM album_artists WHERE album_key=track.album_key)))")
            .bind(prefix).bind(artist).bind(self.source).bind(self.point_update).fetch_one(connection).await?)
    }

    pub async fn local_artwork_artist_track_page(
        &self,
        artist: &str,
        after: &str,
    ) -> LibraryResult<Vec<LocalArtworkCandidate>> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query_as("SELECT track.object_id,track.source_path path,track.disc_number,track.track_number,track.sort_text,
                COALESCE((SELECT artist.object_id FROM album_artists relation JOIN artists artist USING(artist_key)
                    WHERE relation.album_key=track.album_key ORDER BY relation.position LIMIT 1)=?1,0) primary_artist,
                COALESCE(seen.root,current.root) root,current.picture_index,current.artist_pictures,
                COALESCE(current.revision,CAST(current.size_bytes AS TEXT)||'-'||CAST(current.mtime_ns AS TEXT)) revision
             FROM tracks track
             LEFT JOIN temp.scan_local_seen seen ON seen.path=track.source_path
             LEFT JOIN local_files current ON current.source_key=?3 AND current.path=track.source_path
             WHERE track.source_key=?3 AND track.object_id>?2 AND track.source_path IS NOT NULL
               AND track.track_key IN (
                    SELECT relation.track_key FROM artists artist JOIN track_artists relation USING(artist_key) WHERE artist.source_key=?3 AND artist.object_id=?1
                    UNION SELECT track.track_key FROM artists artist JOIN album_artists relation USING(artist_key) JOIN tracks track USING(album_key) WHERE artist.source_key=?3 AND artist.object_id=?1)
               AND (EXISTS(SELECT 1 FROM temp.scan_seen WHERE kind='track' AND object_id=track.object_id)
               OR (?4 AND track.indexed AND NOT EXISTS(SELECT 1 FROM temp.scan_removals WHERE entity_kind='track' AND object_id=track.object_id)
                   AND NOT EXISTS(SELECT 1 FROM temp.scan_local_file_removals WHERE path=track.source_path)))
             ORDER BY track.object_id LIMIT 128")
            .bind(artist).bind(after).bind(self.source).bind(self.point_update).fetch_all(connection).await?)
    }

    pub async fn write_local_artist_artworks(
        &mut self,
        rows: &[(&str, &[u8])],
    ) -> LibraryResult<()> {
        self.remember_artworks(
            "artist",
            &rows
                .iter()
                .map(|(id, binding)| (*id, Some(*binding), true))
                .collect::<Vec<_>>(),
        )
        .await
    }

    pub async fn record_artist_pictures(&mut self, facts: &[(&str, &str)]) -> LibraryResult<()> {
        for page in facts.chunks(128) {
            let mut query =
                sqlx::QueryBuilder::<sqlx::Sqlite>::new("WITH input(path,pictures) AS (");
            query.push_values(page, |mut row, (path, pictures)| {
                row.push_bind(*path).push_bind(*pictures);
            });
            query.push(") UPDATE local_files SET artist_pictures=input.pictures FROM input
                WHERE local_files.source_key=").push_bind(self.source)
                .push(" AND local_files.path=input.path AND local_files.artist_pictures IS NOT input.pictures");
            self.execute(query.build()).await?;
        }
        Ok(())
    }

    pub async fn file_artwork_image_page(
        &self,
        prefix: &str,
        after: &str,
    ) -> LibraryResult<Vec<(String, Option<String>)>> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query_as("SELECT path,revision FROM local_files file
            WHERE source_key=?3 AND kind='image' AND path>=?1 AND path<?1||char(1114111) AND path>?2
              AND (EXISTS(SELECT 1 FROM temp.scan_local_seen seen WHERE seen.path=file.path)
                OR (?4 AND NOT EXISTS(SELECT 1 FROM temp.scan_local_file_removals removed WHERE removed.path=file.path)))
              AND instr(substr(path,length(?1)+1),'/')=0 ORDER BY path LIMIT 128")
            .bind(prefix).bind(after).bind(self.source).bind(self.point_update).fetch_all(connection).await?)
    }

    pub async fn local_artwork_track_page(
        &self,
        album: &str,
        after: Option<&LocalArtworkCandidate>,
    ) -> LibraryResult<Vec<LocalArtworkCandidate>> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query_as("SELECT track.object_id,track.source_path path,track.disc_number,track.track_number,track.sort_text,
               0 primary_artist,
               COALESCE(seen.root,current.root) root,current.picture_index,current.artist_pictures,
               COALESCE(current.revision,CAST(current.size_bytes AS TEXT)||'-'||CAST(current.mtime_ns AS TEXT)) revision
             FROM albums album JOIN tracks track USING(album_key)
             LEFT JOIN temp.scan_local_seen seen ON seen.path=track.source_path
             LEFT JOIN local_files current ON current.source_key=?6 AND current.path=track.source_path
             WHERE album.source_key=?6 AND album.object_id=?1 AND track.source_path IS NOT NULL
               AND (EXISTS(SELECT 1 FROM temp.scan_seen WHERE kind='track' AND object_id=track.object_id)
               OR (?7 AND track.indexed AND NOT EXISTS(SELECT 1 FROM temp.scan_removals WHERE entity_kind='track' AND object_id=track.object_id)
                   AND NOT EXISTS(SELECT 1 FROM temp.scan_local_file_removals WHERE path=track.source_path)))
               AND (?2 IS NULL OR (track.disc_number,track.track_number,track.sort_text,track.object_id)>(?3,?4,?5,?2))
             ORDER BY track.disc_number,track.track_number,track.sort_text,track.object_id LIMIT 128")
            .bind(album).bind(after.map(|row| &row.object_id)).bind(after.map(|row| row.disc_number))
            .bind(after.map(|row| row.track_number)).bind(after.map(|row| &row.sort_text))
            .bind(self.source).bind(self.point_update).fetch_all(connection).await?)
    }

    pub async fn write_local_artwork_candidates(
        &mut self,
        album: &str,
        candidates: &[(&str, Option<&[u8]>, Option<&[u8]>)],
    ) -> LibraryResult<()> {
        for page in candidates.chunks(128) {
            if let Some(binding) = page.iter().find_map(|(_, group, _)| *group) {
                self.execute(sqlx::query("UPDATE temp.scan_artwork SET source=COALESCE(source,?2),binding=COALESCE(binding,?2) WHERE kind='album' AND object_id=?1")
                    .bind(album).bind(binding)).await?;
            }
            self.remember_artworks(
                "track",
                &page
                    .iter()
                    .map(|(track, _, binding)| (*track, *binding, true))
                    .collect::<Vec<_>>(),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn local_artwork_directory_is_single_album(
        &self,
        prefix: &str,
    ) -> LibraryResult<bool> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let albums: i64 = sqlx::query_scalar("SELECT count(*) FROM (SELECT DISTINCT album_key FROM tracks track
            WHERE source_key=?3 AND source_path>=?1 AND source_path<?1||char(1114111)
              AND (EXISTS(SELECT 1 FROM temp.scan_seen WHERE kind='track' AND object_id=track.object_id)
              OR (?2 AND track.indexed AND NOT EXISTS(SELECT 1 FROM temp.scan_removals WHERE entity_kind='track' AND object_id=track.object_id)
                  AND NOT EXISTS(SELECT 1 FROM temp.scan_local_file_removals WHERE path=track.source_path))))")
            .bind(prefix).bind(self.point_update).bind(self.source).fetch_one(connection).await?;
        Ok(albums == 1)
    }
}

pub(super) async fn update_artwork(
    connection: &mut SqliteConnection,
    source: i64,
    distinct: bool,
    point: bool,
    local: bool,
    authoritative: bool,
) -> LibraryResult<bool> {
    if !authoritative || (point && !local) {
        for (kind, table) in [("artist", "artists"), ("genre", "genres")] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE temp.scan_artwork AS image SET source=(SELECT artwork_binding FROM {table} WHERE source_key=?1 AND object_id=image.object_id),
                    binding=(SELECT artwork_binding FROM {table} WHERE source_key=?1 AND object_id=image.object_id)
                 WHERE kind=?2 AND source IS NULL"
            ))).bind(source).bind(kind).execute(&mut *connection).await?;
        }
    }
    sqlx::query("UPDATE temp.scan_artwork AS image SET binding=COALESCE(source,(
            SELECT incoming.source
            FROM albums album JOIN tracks track USING(album_key)
            JOIN temp.scan_artwork incoming ON incoming.kind='track' AND incoming.object_id=track.object_id
            WHERE album.source_key=?1 AND album.object_id=image.object_id
              AND incoming.source IS NOT NULL
            ORDER BY track.disc_number,track.track_number,track.sort_text,track.object_id LIMIT 1)) WHERE kind='album'")
        .bind(source).execute(&mut *connection).await?;

    let mut after = String::new();
    loop {
        let rows: Vec<(String, String, Option<String>, Option<Vec<u8>>, Option<Vec<u8>>)> = sqlx::query_as(
            "SELECT artist.object_id,artist.name,artist.musicbrainz_artist_id,image.source,COALESCE(
                (SELECT CASE WHEN incoming.binding IS NOT NULL OR (?3 AND incoming.object_id IS NOT NULL) THEN incoming.binding ELSE album.artwork_binding END
                 FROM albums album LEFT JOIN temp.scan_artwork incoming ON incoming.kind='album' AND incoming.object_id=album.object_id
                 WHERE album.album_key IN (SELECT album_key FROM album_artists WHERE artist_key=artist.artist_key
                    UNION SELECT track.album_key FROM track_artists relation JOIN tracks track USING(track_key) WHERE relation.artist_key=artist.artist_key)
                   AND CASE WHEN incoming.binding IS NOT NULL OR (?3 AND incoming.object_id IS NOT NULL) THEN incoming.binding ELSE album.artwork_binding END IS NOT NULL
                 ORDER BY album.sort_text,album.object_id LIMIT 1),
                (SELECT CASE WHEN incoming.binding IS NOT NULL OR (?3 AND incoming.object_id IS NOT NULL) THEN incoming.binding ELSE track.artwork_binding END
                 FROM track_artists relation JOIN tracks track USING(track_key)
                 LEFT JOIN temp.scan_artwork incoming ON incoming.kind='track' AND incoming.object_id=track.object_id
                 WHERE relation.artist_key=artist.artist_key
                   AND CASE WHEN incoming.binding IS NOT NULL OR (?3 AND incoming.object_id IS NOT NULL) THEN incoming.binding ELSE track.artwork_binding END IS NOT NULL
                 ORDER BY track.sort_text,track.object_id LIMIT 1))
             FROM temp.scan_artwork image JOIN artists artist ON artist.source_key=?1 AND artist.object_id=image.object_id
             WHERE image.kind='artist' AND artist.object_id>?2 ORDER BY artist.object_id LIMIT 128"
        ).bind(source).bind(&after).bind(authoritative).fetch_all(&mut *connection).await?;
        if rows.is_empty() {
            break;
        }
        for (id, artist_name, musicbrainz_artist_id, source, fallback) in rows {
            after.clone_from(&id);
            let source = source
                .as_deref()
                .and_then(|bytes| serde_json::from_slice::<ArtistArtworkBinding>(bytes).ok())
                .map_or(source, |binding| binding.source);
            let effective = source.as_ref().or(fallback.as_ref());
            let binding = serde_json::to_vec(&ArtistArtworkBinding {
                artist_name,
                musicbrainz_artist_id,
                source: source.clone(),
                fallback: fallback.clone(),
            })?;
            sqlx::query("UPDATE temp.scan_artwork SET source=?2,binding=?3 WHERE kind='artist' AND object_id=?1")
                .bind(id).bind(effective).bind(binding).execute(&mut *connection).await?;
        }
    }
    sqlx::query("UPDATE temp.scan_artwork AS image SET binding=(
        SELECT artist_image.source FROM albums album JOIN album_artists relation USING(album_key)
        JOIN artists artist USING(artist_key) JOIN temp.scan_artwork artist_image ON artist_image.kind='artist' AND artist_image.object_id=artist.object_id
        WHERE album.source_key=?1 AND album.object_id=image.object_id AND artist_image.source IS NOT NULL
        ORDER BY relation.position LIMIT 1) WHERE kind='album' AND binding IS NULL")
        .bind(source).execute(&mut *connection).await?;
    sqlx::query("UPDATE temp.scan_artwork AS image SET binding=(
        SELECT artist_image.source FROM tracks track JOIN track_artists relation USING(track_key)
        JOIN artists artist USING(artist_key) JOIN temp.scan_artwork artist_image ON artist_image.kind='artist' AND artist_image.object_id=artist.object_id
        WHERE track.source_key=?1 AND track.object_id=image.object_id AND track.album_key IS NULL AND artist_image.source IS NOT NULL
        ORDER BY relation.position LIMIT 1) WHERE kind='track' AND binding IS NULL")
        .bind(source).execute(&mut *connection).await?;
    sqlx::query("UPDATE temp.scan_artwork AS image SET binding=(
        SELECT CASE WHEN incoming.object_id IS NOT NULL THEN incoming.binding ELSE COALESCE(album.artwork_binding,image.binding) END
        FROM tracks track JOIN albums album USING(album_key)
        LEFT JOIN temp.scan_artwork incoming ON incoming.kind='album' AND incoming.object_id=album.object_id
        WHERE track.source_key=?1 AND track.object_id=image.object_id)
        WHERE kind='track' AND (?2=0 OR binding IS NULL)
          AND EXISTS(SELECT 1 FROM tracks WHERE source_key=?1 AND object_id=image.object_id AND album_key IS NOT NULL)")
        .bind(source).bind(distinct).execute(&mut *connection).await?;

    let mut changed = false;
    for (kind, table) in [
        ("track", "tracks"),
        ("album", "albums"),
        ("artist", "artists"),
        ("genre", "genres"),
        ("folder", "folders"),
        ("playlist", "playlists"),
    ] {
        changed |= sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} AS entity SET artwork_binding=image.binding FROM temp.scan_artwork image
             WHERE entity.indexed AND entity.source_key=?1 AND image.kind=?2 AND image.object_id=entity.object_id
               AND (?3 OR image.binding IS NOT NULL) AND entity.artwork_binding IS NOT image.binding"
        ))).bind(source).bind(kind).bind(authoritative).execute(&mut *connection).await?.rows_affected() > 0;
    }
    if !distinct {
        changed |= sqlx::query("UPDATE tracks SET artwork_binding=album.artwork_binding FROM albums album
            JOIN temp.scan_artwork image ON image.kind='album' AND image.object_id=album.object_id
            WHERE album.source_key=?1 AND tracks.album_key=album.album_key AND tracks.artwork_binding IS NOT album.artwork_binding")
            .bind(source).execute(&mut *connection).await?.rows_affected() > 0;
    }
    Ok(changed)
}
