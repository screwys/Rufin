use super::*;

const HOME_ROWS: &str = "WITH expected AS MATERIALIZED (
    SELECT json_extract(value,'$[0]') position,json_extract(value,'$[1]') entity_kind,
        CASE json_extract(value,'$[1]')
          WHEN 'track' THEN (SELECT track_key FROM tracks WHERE source_key=?1 AND object_id=json_extract(value,'$[2]'))
          WHEN 'album' THEN (SELECT album_key FROM albums WHERE source_key=?1 AND object_id=json_extract(value,'$[2]'))
          WHEN 'artist' THEN (SELECT artist_key FROM artists WHERE source_key=?1 AND object_id=json_extract(value,'$[2]'))
          WHEN 'playlist' THEN (SELECT playlist_key FROM playlists WHERE owner='source' AND source_key=?1 AND object_id=json_extract(value,'$[2]'))
        END entity_key,json_extract(value,'$[3]') title,json_extract(value,'$[4]') subtitle
    FROM json_each(?3)) ";

impl Scan {
    pub async fn retain_home_section(&mut self, section: &str) -> LibraryResult<()> {
        self.execute(
            sqlx::query(
                "INSERT OR IGNORE INTO temp.scan_seen(kind,object_id,written) VALUES('home',?1,0)",
            )
            .bind(section),
        )
        .await
    }

    pub async fn retain_home_sections_with_prefix(&mut self, prefix: &str) -> LibraryResult<()> {
        self.execute(sqlx::query("INSERT OR IGNORE INTO temp.scan_seen(kind,object_id,written)
            SELECT 'home',section_id,0 FROM home_entries WHERE source_key=?1 AND substr(section_id,1,length(?2))=?2")
            .bind(self.source).bind(prefix)).await
    }

    pub async fn replace_home_section(
        &mut self,
        section: &str,
        title: Option<&str>,
        entries: &[HomeEntryInput],
    ) -> LibraryResult<()> {
        self.require_id("Home section", section)?;
        self.require_row_bytes(&[section.as_bytes(), title.unwrap_or_default().as_bytes()])?;
        for entry in entries {
            self.require_id("Home entity", &entry.entity_object_id)?;
            if entry.position < 0 {
                self.failed = true;
                return Err(LibraryError::InvalidScan(
                    "Home position cannot be negative".into(),
                ));
            }
            self.require_row_bytes(&[
                entry.entity_object_id.as_bytes(),
                entry.title.as_bytes(),
                entry.subtitle.as_bytes(),
            ])?;
        }
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
        let own_batch = self.batch_writer.is_none();
        if own_batch {
            self.begin_batch().await?;
        }
        let result = async {
            let delete = format!("{HOME_ROWS} DELETE FROM home_entries AS current WHERE source_key=?1 AND section_id=?2
                AND NOT EXISTS(SELECT 1 FROM expected WHERE expected.position=current.position AND expected.entity_key IS NOT NULL)");
            self.changes.home |= self.execute_result(sqlx::query(sqlx::AssertSqlSafe(delete))
                .bind(self.source).bind(section).bind(&rows)).await?.rows_affected() > 0;
            let insert = format!("{HOME_ROWS} INSERT INTO home_entries(source_key,section_id,position,entity_kind,entity_key,title,subtitle,section_title)
                SELECT ?1,?2,position,entity_kind,entity_key,title,subtitle,?4 FROM expected WHERE entity_key IS NOT NULL
                ON CONFLICT(source_key,section_id,position) DO UPDATE SET
                    entity_kind=excluded.entity_kind,entity_key=excluded.entity_key,title=excluded.title,subtitle=excluded.subtitle,section_title=excluded.section_title
                WHERE (home_entries.entity_kind,home_entries.entity_key,home_entries.title,home_entries.subtitle,home_entries.section_title)
                    IS NOT (excluded.entity_kind,excluded.entity_key,excluded.title,excluded.subtitle,excluded.section_title)");
            self.changes.home |= self.execute_result(sqlx::query(sqlx::AssertSqlSafe(insert))
                .bind(self.source).bind(section).bind(&rows).bind(title)).await?.rows_affected() > 0;
            self.retain_home_section(section).await
        }.await;
        if own_batch {
            self.finish_batch().await?;
        }
        result
    }
}

pub(super) async fn prune_sections(
    transaction: &mut Transaction<'_, Sqlite>,
    source: i64,
    full: bool,
) -> LibraryResult<bool> {
    if !full {
        return Ok(false);
    }
    Ok(sqlx::query(
        "DELETE FROM home_entries WHERE source_key=?1 AND section_id NOT IN
        (SELECT object_id FROM temp.scan_seen WHERE kind='home')",
    )
    .bind(source)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
        > 0)
}
