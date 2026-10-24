//! Translates supported released formats into the ordinary user-state writers.
pub(crate) mod current;
pub(crate) mod snapshots;
use crate::LibraryResult;
pub(crate) use current::import as import_current;
use sqlx::{Row, sqlite::SqliteRow};
use std::path::Path;

fn string(row: &SqliteRow, name: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(name).ok().flatten()
}
fn integer(row: &SqliteRow, name: &str) -> Option<i64> {
    row.try_get::<Option<i64>, _>(name).ok().flatten()
}

fn install(pending: &Path, path: &Path) -> LibraryResult<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(pending)?
        .sync_all()?;
    crate::db::preserve_store(path)?;
    std::fs::rename(pending, path)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

async fn normalize_stored_artwork(connection: &mut sqlx::SqliteConnection) -> LibraryResult<()> {
    for table in ["tracks", "playlists", "smart_playlists"] {
        let column = if table != "tracks" {
            "artwork_binding"
        } else {
            "native_artwork"
        };
        let mut after = 0_i64;
        loop {
            let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT rowid,{column} FROM {table} WHERE rowid>?1 AND json_type({column})='object' ORDER BY rowid LIMIT 128"))).bind(after).fetch_all(&mut *connection).await?;
            if rows.is_empty() {
                break;
            }
            for (key, binding) in rows {
                after = key;
                let normalized = crate::normalize_artwork_binding(&binding)?;
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE {table} SET {column}=?1 WHERE rowid=?2"
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
