//! Radio stations. A server's stations are kept as a copy, so they list and play while the
//! server is away; a source that keeps none shows the ones Rufin keeps.
use crate::{Catalog, LibraryResult, Scope, SidebarPin, SourceKey};

#[derive(Clone, Debug, Eq, PartialEq, sqlx::FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StationRow {
    pub station_key: i64,
    /// The server that keeps it; None when Rufin does.
    pub source_key: Option<SourceKey>,
    pub object_id: String,
    pub name: String,
    pub stream_url: String,
    pub homepage: String,
    /// The server's picture id.
    pub cover: Option<String>,
    /// The address of its picture, where the search that found it gave one.
    pub logo: Option<String>,
    /// The revision of the picture a person chose for it.
    pub picture: Option<String>,
}

/// A station as a server lists it or a person entered it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StationWrite {
    pub object_id: String,
    pub name: String,
    pub stream_url: String,
    pub homepage: String,
    pub cover: Option<String>,
}

const COLUMNS: &str =
    "station_key,source_key,object_id,name,stream_url,homepage,cover,logo,artwork_revision picture";

impl Catalog {
    /// Every station: each server's and the ones Rufin keeps, in the order a person gave them.
    pub async fn stations(&self) -> LibraryResult<Vec<StationRow>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM stations ORDER BY position,name COLLATE NOCASE,station_key"
        )))
        .fetch_all(&mut *reader)
        .await?)
    }

    pub async fn station(&self, key: i64) -> LibraryResult<Option<StationRow>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM stations WHERE station_key=?1"
        )))
        .bind(key)
        .fetch_optional(&mut *reader)
        .await?)
    }

    /// The stations `pins` name that the Store still holds, each with its pin.
    pub async fn pinned_stations(
        &self,
        pins: &[SidebarPin],
    ) -> LibraryResult<Vec<(SidebarPin, StationRow)>> {
        let wanted = pins
            .iter()
            .filter_map(|pin| {
                let SidebarPin::Station {
                    source_id,
                    station_id,
                    ..
                } = pin
                else {
                    return None;
                };
                let source = match source_id {
                    Some(id) => Some(self.source_key(id)?),
                    None => None,
                };
                Some((pin, source, station_id))
            })
            .collect::<Vec<_>>();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let mut reader = self.acquire_reader().await?;
        let mut stations = Vec::new();
        for (pin, source, station_id) in wanted {
            let row = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT {COLUMNS} FROM stations WHERE source_key IS ?1 AND object_id=?2"
            )))
            .bind(source)
            .bind(station_id)
            .fetch_optional(&mut *reader)
            .await?;
            if let Some(row) = row {
                stations.push((pin.clone(), row));
            }
        }
        Ok(stations)
    }

    /// A station's sidebar pin: the server that keeps it, or none for Rufin's own, and its id
    /// there. None when the Catalog no longer holds its server.
    pub fn station_pin(&self, row: &StationRow) -> Option<SidebarPin> {
        Some(SidebarPin::Station {
            source_id: match row.source_key {
                Some(key) => Some(self.source_id(key)?),
                None => None,
            },
            station_id: row.object_id.clone(),
            channel: None,
        })
    }

    /// Makes the copy of a server's stations what the server listed. A station it still lists
    /// keeps what Rufin holds for it: its picture and its place.
    pub async fn replace_stations(
        &self,
        source: SourceKey,
        listed: &[StationWrite],
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let connection = write.connection().await?;
        let before: Vec<(String, String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT object_id,name,stream_url,homepage,cover FROM stations WHERE source_key=?1 ORDER BY object_id",
        )
        .bind(source)
        .fetch_all(&mut *connection)
        .await?;
        let mut after = listed
            .iter()
            .map(|station| {
                (
                    station.object_id.clone(),
                    station.name.clone(),
                    station.stream_url.clone(),
                    station.homepage.clone(),
                    station.cover.clone(),
                )
            })
            .collect::<Vec<_>>();
        after.sort();
        if before == after {
            return Ok(());
        }
        let ids = serde_json::to_string(
            &listed
                .iter()
                .map(|station| &station.object_id)
                .collect::<Vec<_>>(),
        )?;
        sqlx::query("DELETE FROM stations WHERE source_key=?1 AND object_id NOT IN(SELECT value FROM json_each(?2))")
            .bind(source)
            .bind(ids)
            .execute(&mut *connection)
            .await?;
        for station in listed {
            upsert(connection, Some(source), station).await?;
        }
        write.changed(Scope::Stations);
        write.commit().await?;
        Ok(())
    }

    /// Writes one station, by its object id, and answers its row.
    pub async fn put_station(
        &self,
        source: Option<SourceKey>,
        station: &StationWrite,
    ) -> LibraryResult<StationRow> {
        let mut write = self.write().await?;
        let connection = write.connection().await?;
        let key = upsert(connection, source, station).await?;
        let row = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM stations WHERE station_key=?1"
        )))
        .bind(key)
        .fetch_one(&mut *connection)
        .await?;
        write.changed(Scope::Stations);
        write.commit().await?;
        Ok(row)
    }

    /// The address of a station's picture.
    pub async fn set_station_logo(&self, key: i64, logo: Option<&str>) -> LibraryResult<()> {
        let mut write = self.write().await?;
        sqlx::query("UPDATE stations SET logo=?2 WHERE station_key=?1")
            .bind(key)
            .bind(logo.filter(|logo| !logo.is_empty()))
            .execute(write.connection().await?)
            .await?;
        write.changed(Scope::Stations);
        write.commit().await?;
        Ok(())
    }

    /// Sets or clears the picture a person chose for a station: its bytes and content type.
    pub async fn set_station_picture(
        &self,
        key: i64,
        picture: Option<(&[u8], Option<&str>)>,
    ) -> LibraryResult<()> {
        let (bytes, mime) = picture.unzip();
        let mut write = self.write().await?;
        sqlx::query(
            "UPDATE stations SET artwork_bytes=?2,artwork_mime=?3,artwork_revision=?4 WHERE station_key=?1",
        )
        .bind(key)
        .bind(bytes)
        .bind(mime.flatten())
        .bind(bytes.map(|bytes| blake3::hash(bytes).to_hex().to_string()))
        .execute(write.connection().await?)
        .await?;
        write.changed(Scope::Stations);
        write.commit().await?;
        Ok(())
    }

    /// The bytes of the picture a station's binding names.
    pub async fn station_picture(
        &self,
        image: &crate::ArtworkImage,
    ) -> LibraryResult<Option<Vec<u8>>> {
        let crate::ArtworkImage::Station { key, revision } = image else {
            return Ok(None);
        };
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT artwork_bytes FROM stations WHERE station_key=?1 AND artwork_revision=?2 AND artwork_bytes IS NOT NULL",
        )
        .bind(key)
        .bind(revision)
        .fetch_optional(&mut *reader)
        .await?)
    }

    /// Orders stations as `keys` lists them.
    pub async fn order_stations(&self, keys: &[i64]) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let connection = write.connection().await?;
        for (position, key) in keys.iter().enumerate() {
            sqlx::query("UPDATE stations SET position=?2 WHERE station_key=?1")
                .bind(key)
                .bind(position as i64 + 1)
                .execute(&mut *connection)
                .await?;
        }
        write.changed(Scope::Stations);
        write.commit().await?;
        Ok(())
    }

    pub async fn delete_station(&self, key: i64) -> LibraryResult<()> {
        let mut write = self.write().await?;
        sqlx::query("DELETE FROM stations WHERE station_key=?1")
            .bind(key)
            .execute(write.connection().await?)
            .await?;
        write.changed(Scope::Stations);
        write.commit().await?;
        Ok(())
    }
}

/// A station Rufin keeps, as a backup holds it.
#[derive(serde::Deserialize, serde::Serialize, sqlx::FromRow)]
struct Kept {
    object_id: String,
    name: String,
    stream_url: String,
    homepage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    logo: Option<String>,
}

/// Writes the stations Rufin keeps. A server's stations are the server's to keep.
pub(crate) async fn export_on(
    connection: &mut sqlx::SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<()> {
    output.write_all(b"{\"version\":1}\n")?;
    let rows: Vec<Kept> = sqlx::query_as(
        "SELECT object_id,name,stream_url,homepage,logo FROM stations WHERE source_key IS NULL ORDER BY position,station_key",
    )
    .fetch_all(connection)
    .await?;
    for row in rows {
        serde_json::to_writer(&mut output, &row)?;
        output.write_all(b"\n")?;
    }
    Ok(())
}

pub(crate) async fn import_on(
    connection: &mut sqlx::SqliteConnection,
    input: impl std::io::BufRead,
) -> LibraryResult<()> {
    let mut lines = input.lines();
    let header: serde_json::Value =
        serde_json::from_str(&lines.next().transpose()?.unwrap_or_default())?;
    if header.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(crate::LibraryError::InvalidRequest(
            "unsupported radio station version".into(),
        ));
    }
    for line in lines {
        let kept: Kept = serde_json::from_str(&line?)?;
        let key = upsert(
            connection,
            None,
            &StationWrite {
                object_id: kept.object_id,
                name: kept.name,
                stream_url: kept.stream_url,
                homepage: kept.homepage,
                cover: None,
            },
        )
        .await?;
        sqlx::query("UPDATE stations SET logo=?2 WHERE station_key=?1")
            .bind(key)
            .bind(kept.logo)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

async fn upsert(
    connection: &mut sqlx::SqliteConnection,
    source: Option<SourceKey>,
    station: &StationWrite,
) -> LibraryResult<i64> {
    // SQLite's UNIQUE holds NULL source keys apart, so Rufin's own are matched by hand.
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT station_key FROM stations WHERE source_key IS ?1 AND object_id=?2",
    )
    .bind(source)
    .bind(&station.object_id)
    .fetch_optional(&mut *connection)
    .await?;
    match existing {
        Some(key) => {
            sqlx::query(
                "UPDATE stations SET name=?2,stream_url=?3,homepage=?4,cover=?5 WHERE station_key=?1",
            )
            .bind(key)
            .bind(&station.name)
            .bind(&station.stream_url)
            .bind(&station.homepage)
            .bind(&station.cover)
            .execute(&mut *connection)
            .await?;
            Ok(key)
        }
        None => Ok(sqlx::query_scalar(
            "INSERT INTO stations(source_key,object_id,name,stream_url,homepage,cover,position)
             VALUES(?1,?2,?3,?4,?5,?6,(SELECT coalesce(max(position),0)+1 FROM stations))
             RETURNING station_key",
        )
        .bind(source)
        .bind(&station.object_id)
        .bind(&station.name)
        .bind(&station.stream_url)
        .bind(&station.homepage)
        .bind(&station.cover)
        .fetch_one(&mut *connection)
        .await?),
    }
}
