use super::*;

impl Scan {
    pub async fn write_track_relations(
        &mut self,
        artists: &[(&str, &str)],
        genres: &[(&str, &str)],
        moods: &[(&str, &str)],
    ) -> LibraryResult<()> {
        self.write_relations("track", "artist", artists).await?;
        self.write_relations("track", "genre", genres).await?;
        self.write_relations("track", "mood", moods).await
    }

    pub async fn write_album_relations(
        &mut self,
        artists: &[(&str, &str)],
        genres: &[(&str, &str)],
        release_types: &[(&str, &str)],
    ) -> LibraryResult<()> {
        self.write_relations("album", "artist", artists).await?;
        self.write_relations("album", "genre", genres).await?;
        self.write_relations("album", "release_type", release_types)
            .await
    }

    pub async fn write_track_folders(&mut self, links: &[ScanLink<'_>]) -> LibraryResult<()> {
        if links.iter().any(|link| link.position < 0) {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "link position cannot be negative".to_string(),
            ));
        }
        let mut links = links.to_vec();
        links.sort_by_key(|link| (link.position, link.related_id));
        self.write_relations(
            "track",
            "folder",
            &links
                .iter()
                .map(|link| (link.owner_id, link.related_id))
                .collect::<Vec<_>>(),
        )
        .await
    }

    pub async fn replace_track_folders(
        &mut self,
        object_ids: &[&str],
        links: &[ScanLink<'_>],
    ) -> LibraryResult<()> {
        for page in object_ids.chunks(128) {
            for object_id in page {
                self.require_id("track", object_id)?;
            }
            let ids = serde_json::to_string(page)?;
            self.execute(
                sqlx::query(
                    "INSERT OR IGNORE INTO temp.scan_removals
                SELECT 'track-folders',value FROM json_each(?1)",
                )
                .bind(&ids),
            )
            .await?;
            self.execute(
                sqlx::query(
                    "DELETE FROM temp.scan_relation_seen WHERE kind='track_folders'
                AND owner_key IN (SELECT track_key FROM tracks WHERE source_key=?1
                    AND object_id IN (SELECT value FROM json_each(?2)))",
                )
                .bind(self.source)
                .bind(ids),
            )
            .await?;
        }
        self.write_track_folders(links).await
    }

    async fn write_relations(
        &mut self,
        owner: &'static str,
        target: &'static str,
        links: &[(&str, &str)],
    ) -> LibraryResult<()> {
        if links.is_empty() {
            return Ok(());
        }
        for (owner_id, target_id) in links {
            self.require_id(owner, owner_id)?;
            self.require_id(target, target_id)?;
            self.require_row_bytes(&[owner_id.as_bytes(), target_id.as_bytes()])?;
        }
        let relation = format!("{owner}_{target}s");
        let owner_key = format!("{owner}_key");
        let (target_key, target_value, target_join) = if target == "release_type" {
            (
                target.to_owned(),
                "input.target_id".to_owned(),
                String::new(),
            )
        } else {
            (
                format!("{target}_key"),
                format!("target.{target}_key"),
                format!(
                    "CROSS JOIN {target}s target ON target.source_key=?1 AND target.object_id=input.target_id"
                ),
            )
        };
        let incoming = format!(
            "WITH input AS (
                SELECT json_extract(value,'$[0]') owner_id,json_extract(value,'$[1]') target_id,key ordinal
                FROM json_each(?2)
             ), resolved AS MATERIALIZED (
                SELECT owner.{owner_key} owner_key,{target_value} target_key,min(input.ordinal) ordinal
                FROM input CROSS JOIN {owner}s owner ON owner.source_key=?1 AND owner.object_id=input.owner_id
                {target_join} GROUP BY owner.{owner_key},{target_value}
             ), incoming AS MATERIALIZED (
                SELECT * FROM resolved WHERE NOT EXISTS(
                    SELECT 1 FROM temp.scan_relation_seen seen WHERE seen.kind='{relation}'
                    AND seen.owner_key=resolved.owner_key AND seen.target_key=CAST(resolved.target_key AS TEXT)))"
        );
        let order = format!(
            "{incoming}, desired AS MATERIALIZED (
                SELECT current.{owner_key} owner_key,current.{target_key} target_key,current.position,
                    max(current.position) OVER (PARTITION BY current.{owner_key})+1 displacement,
                    row_number() OVER (PARTITION BY current.{owner_key} ORDER BY
                        CASE WHEN seen.target_key IS NOT NULL THEN 0 WHEN incoming.target_key IS NOT NULL THEN 1 ELSE 2 END,
                        CASE WHEN seen.target_key IS NULL AND incoming.target_key IS NOT NULL THEN incoming.ordinal ELSE current.position END)-1 next_position
                FROM {relation} current
                LEFT JOIN temp.scan_relation_seen seen ON seen.kind='{relation}'
                    AND seen.owner_key=current.{owner_key} AND seen.target_key=CAST(current.{target_key} AS TEXT)
                LEFT JOIN incoming ON incoming.owner_key=current.{owner_key} AND incoming.target_key=current.{target_key}
                WHERE current.{owner_key} IN (SELECT owner_key FROM incoming))"
        );
        let statements = [
            format!(
                "{incoming}, additions AS MATERIALIZED (
                    SELECT incoming.*,COALESCE((SELECT max(position)+1 FROM {relation}
                        WHERE {owner_key}=incoming.owner_key),0) start_position
                    FROM incoming WHERE NOT EXISTS(SELECT 1 FROM {relation}
                        WHERE {owner_key}=incoming.owner_key AND {target_key}=incoming.target_key))
                 INSERT INTO {relation}({owner_key},{target_key},position)
                 SELECT owner_key,target_key,start_position+row_number() OVER(PARTITION BY owner_key ORDER BY ordinal)-1 FROM additions"
            ),
            format!(
                "{order} UPDATE {relation} SET position=desired.position+desired.displacement
                 FROM desired WHERE {relation}.{owner_key}=desired.owner_key AND {relation}.{target_key}=desired.target_key
                 AND desired.owner_key IN (SELECT owner_key FROM desired WHERE position<>next_position)"
            ),
            format!(
                "{order} UPDATE {relation} SET position=desired.next_position
                 FROM desired WHERE {relation}.{owner_key}=desired.owner_key AND {relation}.{target_key}=desired.target_key
                 AND {relation}.position<>desired.next_position"
            ),
        ];
        for page in links.chunks(128) {
            let input = serde_json::to_string(page).expect("relation identifiers serialize");
            for statement in &statements {
                self.changes.metadata |= self
                    .execute_result(
                        sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
                            .bind(self.source)
                            .bind(&input),
                    )
                    .await?
                    .rows_affected()
                    > 0;
            }
            self.execute(
                sqlx::query(sqlx::AssertSqlSafe(format!(
                "{incoming} INSERT OR IGNORE INTO temp.scan_relation_seen(kind,owner_key,target_key)
                 SELECT '{relation}',owner_key,CAST(target_key AS TEXT) FROM resolved"
            )))
                .bind(self.source)
                .bind(&input),
            )
            .await?;
        }
        Ok(())
    }
}

pub(super) async fn prune_relations(
    transaction: &mut Transaction<'_, Sqlite>,
    source: i64,
    authoritative: bool,
) -> LibraryResult<bool> {
    let mut changed = false;
    for (owner, target) in [
        ("album", "artist"),
        ("track", "artist"),
        ("album", "genre"),
        ("track", "genre"),
        ("track", "mood"),
        ("track", "folder"),
        ("album", "release_type"),
    ] {
        if owner == "album" && !authoritative {
            continue;
        }
        let relation = format!("{owner}_{target}s");
        let owner_key = format!("{owner}_key");
        let target_key = if target == "release_type" {
            target.to_owned()
        } else {
            format!("{target}_key")
        };
        let scope = if target == "folder" {
            "AND (track_key IN (SELECT owner_key FROM temp.scan_relation_seen WHERE kind='track_folders')
             OR object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='track-folders'))".to_owned()
        } else {
            format!(
                "AND object_id IN (SELECT object_id FROM temp.scan_seen WHERE kind='{owner}' AND written=1)"
            )
        };
        changed |= sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {relation} AS current
             WHERE {owner_key} IN (SELECT {owner_key} FROM {owner}s WHERE source_key=?1 {scope})
             AND NOT EXISTS(SELECT 1 FROM temp.scan_relation_seen seen WHERE seen.kind='{relation}'
                AND seen.owner_key=current.{owner_key} AND seen.target_key=CAST(current.{target_key} AS TEXT))"
        ))).bind(source).execute(&mut **transaction).await?.rows_affected() > 0;
    }
    Ok(changed)
}
