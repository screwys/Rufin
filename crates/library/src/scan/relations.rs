use std::collections::{BTreeMap, HashSet};

use super::*;

impl Scan {
    pub(super) async fn replace_track_folders(
        &mut self,
        object_ids: &[&str],
        links: &[(&str, &str)],
    ) -> LibraryResult<()> {
        if object_ids.is_empty() {
            return Ok(());
        }
        let ids = serde_json::to_string(object_ids)?;
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
        self.write_relations("track", "folder", links).await
    }

    pub(super) async fn write_relations(
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
                    "JOIN {target}s target ON target.source_key=?1 AND target.object_id=input.target_id"
                ),
            )
        };
        let incoming = format!(
            "WITH input AS (
                SELECT json_extract(value,'$[0]') owner_id,json_extract(value,'$[1]') target_id,key ordinal
                FROM json_each(?2)
             ), resolved AS (
                SELECT owner.{owner_key} owner_key,CAST({target_value} AS TEXT) target_key,min(input.ordinal) ordinal
                FROM input JOIN {owner}s owner ON owner.source_key=?1 AND owner.object_id=input.owner_id
                {target_join} GROUP BY owner.{owner_key},{target_value})"
        );
        let read = format!(
            "{incoming}
             SELECT current.{owner_key},CAST(current.{target_key} AS TEXT),current.position,
                    seen.target_key IS NOT NULL,0 incoming
             FROM {relation} current LEFT JOIN temp.scan_relation_seen seen
                ON seen.kind='{relation}' AND seen.owner_key=current.{owner_key}
                AND seen.target_key=CAST(current.{target_key} AS TEXT)
             WHERE current.{owner_key} IN (SELECT owner_key FROM resolved)
             UNION ALL SELECT owner_key,target_key,ordinal,1,1 FROM resolved
             ORDER BY incoming,3"
        );
        for page in links.chunks(128) {
            let owns_batch = self.batch_writer.is_none();
            if owns_batch {
                self.begin_batch().await?;
            }
            let result = async {
                let input = serde_json::to_string(page)?;
                let rows = sqlx::query_as::<_, (i64, String, i64, bool, bool)>(
                    sqlx::AssertSqlSafe(read.as_str()),
                )
                    .bind(self.source)
                    .bind(&input)
                    .fetch_all(
                        self.batch_writer.as_mut().unwrap().as_mut()
                            .ok_or(LibraryError::WriterUnavailable)?,
                    )
                    .await?;
                let mut owners = BTreeMap::<i64, (Vec<(String, i64, bool)>, Vec<String>)>::new();
                for (owner, target, position, observed, incoming) in rows {
                    let (current, additions) = owners.entry(owner).or_default();
                    if incoming {
                        additions.push(target);
                    } else {
                        current.push((target, position, observed));
                    }
                }
                let mut removals = Vec::new();
                let mut insertions = Vec::new();
                for (owner, (current, additions)) in owners {
                    let mut seen = HashSet::new();
                    // Earlier observations keep their order. Unread links remain until completion.
                    let desired = current.iter()
                        .filter(|row| row.2)
                        .map(|row| &row.0)
                        .chain(&additions)
                        .chain(current.iter().filter(|row| !row.2).map(|row| &row.0))
                        .filter(|target| seen.insert(*target))
                        .collect::<Vec<_>>();
                    for (target, position, _) in &current {
                        if desired.get(*position as usize).copied() != Some(target) {
                            removals.push((owner, *position));
                        }
                    }
                    for (position, target) in desired.into_iter().enumerate() {
                        if !current.binary_search_by_key(&(position as i64), |row| row.1)
                            .is_ok_and(|index| current[index].0 == *target) {
                            insertions.push((owner, target.clone(), position as i64));
                        }
                    }
                }
                if !removals.is_empty() {
                    self.execute(
                        sqlx::query(sqlx::AssertSqlSafe(format!(
                            "DELETE FROM {relation} WHERE ({owner_key},position) IN (
                                SELECT json_extract(value,'$[0]'),json_extract(value,'$[1]') FROM json_each(?1))"
                        )))
                        .bind(serde_json::to_string(&removals)?),
                    ).await?;
                }
                if !insertions.is_empty() {
                    for page in insertions.chunks(128) {
                        let mut insert = QueryBuilder::<Sqlite>::new(format!(
                            "INSERT INTO {relation}({owner_key},{target_key},position) "
                        ));
                        insert.push_values(page, |mut row, (owner, target, position)| {
                            row.push_bind(owner).push_bind(target).push_bind(position);
                        });
                        self.execute(insert.build()).await?;
                    }
                }
                self.changes.metadata |= !removals.is_empty() || !insertions.is_empty();
                self.execute(
                    sqlx::query(sqlx::AssertSqlSafe(format!(
                        "{incoming} INSERT OR IGNORE INTO temp.scan_relation_seen(kind,owner_key,target_key)
                         SELECT '{relation}',owner_key,target_key FROM resolved"
                    )))
                    .bind(self.source).bind(&input),
                ).await
            }.await;
            if result.is_err() {
                self.failed = true;
            }
            if owns_batch {
                self.finish_batch().await?;
            }
            result?;
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
