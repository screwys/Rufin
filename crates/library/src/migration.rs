//! Brings a Store another version wrote to the current schema: `current` imports released
//! 0.15.0-0.16.6 Stores and rebuilds unknown ones, `steps` upgrades 0.17 and later in place.
pub(crate) mod current;
pub(crate) mod steps;
use crate::LibraryResult;
pub(crate) use current::import as import_current;
use sqlx::{Row, sqlite::SqliteRow};

fn string(row: &SqliteRow, name: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(name).ok().flatten()
}
fn integer(row: &SqliteRow, name: &str) -> Option<i64> {
    row.try_get::<Option<i64>, _>(name).ok().flatten()
}

async fn normalize_stored_artwork(connection: &mut sqlx::SqliteConnection) -> LibraryResult<()> {
    for table in ["playlists", "smart_playlists"] {
        let mut after = 0_i64;
        loop {
            let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT rowid,artwork_binding FROM {table} WHERE rowid>?1 AND json_type(artwork_binding)='object' ORDER BY rowid LIMIT 128"))).bind(after).fetch_all(&mut *connection).await?;
            if rows.is_empty() {
                break;
            }
            for (key, binding) in rows {
                after = key;
                let normalized = crate::normalize_artwork_binding(&binding)?;
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE {table} SET artwork_binding=?1 WHERE rowid=?2"
                )))
                .bind(normalized)
                .bind(key)
                .execute(&mut *connection)
                .await?;
            }
        }
    }
    Ok(())
}
