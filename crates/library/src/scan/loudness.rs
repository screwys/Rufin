use super::*;

pub(super) async fn finish_measurements(
    connection: &mut SqliteConnection,
    source: i64,
    authoritative: bool,
) -> LibraryResult<bool> {
    sqlx::query("INSERT OR IGNORE INTO temp.scan_affected_albums SELECT album.album_key FROM albums album WHERE album.source_key=?1 AND (EXISTS(SELECT 1 FROM temp.scan_seen WHERE kind='album' AND written=1 AND object_id=album.object_id) OR EXISTS(SELECT 1 FROM tracks track JOIN temp.scan_seen seen ON seen.kind='track' AND seen.written=1 AND seen.object_id=track.object_id WHERE track.source_key=?1 AND track.album_key=album.album_key))").bind(source).execute(&mut *connection).await?;
    let mut changed = false;
    let mut after = 0;
    loop {
        let albums = sqlx::query_scalar::<_, crate::AlbumKey>(
            "SELECT album.album_key FROM temp.scan_affected_albums affected
             CROSS JOIN albums album USING(album_key)
             WHERE album.source_key=?1 AND affected.album_key>?2 ORDER BY affected.album_key LIMIT 128",
        )
        .bind(source)
        .bind(after)
        .fetch_all(&mut *connection)
        .await?;
        if albums.is_empty() {
            break;
        }
        for album in albums {
            changed |=
                crate::loudness::recompute_album_loudness_key(connection, album, true).await?;
            after = album.raw();
        }
    }
    for (table, bit, origin) in [
        ("loudness_measurements", 256, "AND origin='source'"),
        ("replay_gain_measurements", 512, ""),
    ] {
        // Only supplied album facts acquire the newly computed membership identity.
        changed |= sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET analysis_key=album.source_loudness_analysis_key
             FROM albums album JOIN temp.scan_seen seen ON seen.kind='album' AND seen.object_id=album.object_id
             WHERE {table}.source_key=?1 AND {table}.entity_kind='album' AND {table}.entity_key=album.album_key
               AND seen.supplied & {bit} != 0 {origin} AND {table}.analysis_key IS NOT album.source_loudness_analysis_key")))
            .bind(source).execute(&mut *connection).await?.rows_affected()>0;
        for kind in ["track", "album"] {
            if authoritative || kind == "track" {
                changed |= sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DELETE FROM {table} WHERE source_key=?1 AND entity_kind=?2 {origin}
                     AND entity_key IN (SELECT item.{kind}_key FROM {kind}s item JOIN temp.scan_seen seen
                        ON seen.kind=?2 AND seen.written=1 AND seen.object_id=item.object_id
                        WHERE item.source_key=?1 AND seen.supplied & {bit}=0)")))
                    .bind(source).bind(kind).execute(&mut *connection).await?.rows_affected()>0;
            }
        }
    }
    Ok(changed)
}
