use super::*;

impl Scan {
    pub async fn begin_file_import(database: &Database, source_id: &str) -> LibraryResult<Self> {
        let mut scan = Self::begin_local_items(database, source_id).await?;
        scan.import_files = true;
        Ok(scan)
    }

    pub async fn local_file_reuse_candidates(
        &mut self,
        observations: &[crate::LocalFileWrite],
    ) -> LibraryResult<Vec<crate::LocalFileRow>> {
        if observations.is_empty() {
            return Ok(Vec::new());
        }
        if observations.len() > 128 {
            return Err(LibraryError::InvalidScan(
                "Local reuse batch exceeds 128 files".into(),
            ));
        }
        let source = self.source;
        let mut acquired;
        let connection = if let Some(writer) = self.batch_writer.as_mut() {
            writer.as_mut().ok_or(LibraryError::WriterUnavailable)?
        } else {
            acquired = self.database.writer().await?;
            acquired.as_mut().ok_or(LibraryError::WriterUnavailable)?
        };
        let mut rows = crate::local::local_file_reuse_candidates_on(
            connection,
            SourceKey::from_raw(source),
            observations,
        )
        .await?;
        let aliases = rows
            .iter()
            .filter(|row| row.track_object_id.is_some())
            .map(|row| {
                (
                    row.path.clone(),
                    row.native_id.clone(),
                    row.device_id,
                    row.inode,
                )
            })
            .collect::<Vec<_>>();
        rows.retain(|row| {
            row.state != crate::LocalFileState::Observed
                || row.track_object_id.is_some()
                || !aliases.iter().any(|(path, native, device, inode)| {
                    *path != row.path
                        && ((native.is_some() && *native == row.native_id)
                            || (device.is_some()
                                && inode.is_some()
                                && *device == row.device_id
                                && *inode == row.inode))
                })
        });
        Ok(rows)
    }

    /// Save the selected files as playlist entries without retaining a folder inventory.
    pub async fn create_local_playlist(&mut self, name: &str) -> LibraryResult<crate::PlaylistKey> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let (playlist, _) =
            crate::playlists::create_playlist_identity(&mut transaction, None, name).await?;
        sqlx::query("INSERT INTO playlist_entries(playlist_key,object_id,track_key,position)
            SELECT ?1,'rufin:entry:'||lower(hex(randomblob(16))),track.track_key,
                row_number() OVER (ORDER BY coalesce(track.cue_path,track.source_path),track.disc_number,track.track_number,track.object_id)-1
            FROM tracks track
            WHERE track.source_key=?2 AND track.indexed AND EXISTS(SELECT 1 FROM temp.scan_local_seen file WHERE file.path=coalesce(track.cue_path,track.source_path))")
            .bind(playlist).bind(self.source).execute(&mut *transaction).await?;
        sqlx::raw_sql(
            "DELETE FROM temp.scan_local_seen; DELETE FROM temp.scan_local_dependency_paths;",
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(playlist)
    }

    pub async fn write_local_dependency_paths(&mut self, paths: &[String]) -> LibraryResult<()> {
        if paths.len() > 128 {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "Local dependency batch exceeds 128 paths".to_string(),
            ));
        }
        if paths.is_empty() {
            return Ok(());
        }
        for path in paths {
            self.require_id("Local dependency path", path)?;
            self.require_row_bytes(&[path.as_bytes()])?;
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT OR IGNORE INTO temp.scan_local_dependency_paths(path) ",
        );
        query.push_values(paths, |mut row, path| {
            row.push_bind(path);
        });
        self.execute(query.build()).await?;
        let documents = paths
            .iter()
            .filter(|path| path.starts_with("rufin-document://"))
            .collect::<Vec<_>>();
        if !documents.is_empty() {
            let mut query = QueryBuilder::<Sqlite>::new(
                "INSERT OR IGNORE INTO temp.scan_local_dependency_paths(path)
                 SELECT alias.path FROM local_files original
                 JOIN local_files alias ON alias.source_key=original.source_key AND alias.native_id=original.native_id
                 JOIN temp.scan_local_seen seen ON seen.path=alias.path
                 WHERE original.source_key=",
            );
            query
                .push_bind(self.source)
                .push(" AND original.native_id IS NOT NULL AND original.path IN (");
            let mut values = query.separated(",");
            for path in documents {
                values.push_bind(path);
            }
            values.push_unseparated(")");
            self.execute(query.build()).await?;
        }
        Ok(())
    }

    pub async fn write_local_files(
        &mut self,
        files: &[(crate::LocalFileWrite, Vec<String>)],
    ) -> LibraryResult<()> {
        if files.len() > 128 {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "Local observation batch exceeds 128 files".to_string(),
            ));
        }
        if files.is_empty() {
            return Ok(());
        }
        self.local_point_update = self.point_update;
        for (file, dependencies) in files {
            if file.path.is_empty()
                || file.root.is_empty()
                || !file.path.starts_with(&file.root)
                || file.mtime_ns < 0
                || dependencies.iter().any(String::is_empty)
            {
                self.failed = true;
                return Err(LibraryError::InvalidScan(
                    "invalid Local observation".to_string(),
                ));
            }
            self.require_row_bytes(&[
                file.path.as_bytes(),
                file.root.as_bytes(),
                file.relative_path.as_bytes(),
                file.native_id.as_deref().unwrap_or_default().as_bytes(),
                file.revision.as_deref().unwrap_or_default().as_bytes(),
            ])?;
        }
        if self.import_files {
            let mut paths = QueryBuilder::<Sqlite>::new(
                "INSERT OR IGNORE INTO temp.scan_local_seen(path,root,kind,changed) ",
            );
            paths.push_values(files, |mut row, (file, _)| {
                row.push_bind(&file.path)
                    .push_bind(&file.root)
                    .push_bind(file.kind)
                    .push_bind(false);
            });
            return self.execute(paths.build()).await;
        }
        let mut seen = QueryBuilder::<Sqlite>::new(
            "WITH incoming(path,root,kind,size_bytes,mtime_ns,revision,pending_directory) AS (",
        );
        seen.push_values(files, |mut row, (file, _)| {
            row.push_bind(&file.path)
                .push_bind(&file.root)
                .push_bind(file.kind)
                .push_bind(file.size_bytes)
                .push_bind(file.mtime_ns)
                .push_bind(&file.revision)
                .push_bind(
                    file.kind == crate::LocalFileKind::Directory
                        && file.state == crate::LocalFileState::Observed,
                );
        });
        seen.push(") INSERT INTO temp.scan_local_seen(path,root,kind,changed,pending_directory,previous_revision,previous_parse_version,previous_accepted)
            SELECT incoming.path,incoming.root,incoming.kind,file.path IS NULL OR file.size_bytes IS NOT incoming.size_bytes
                OR file.mtime_ns<>incoming.mtime_ns OR file.revision IS NOT incoming.revision,
                incoming.pending_directory,CASE WHEN incoming.kind='directory' THEN file.revision END,
                CASE WHEN incoming.kind='directory' THEN file.parse_version END,
                incoming.kind='directory' AND COALESCE(file.state='accepted',0)
            FROM incoming LEFT JOIN local_files file ON file.source_key=")
            .push_bind(self.source)
            .push(" AND file.path=incoming.path WHERE true
            ON CONFLICT(path) DO UPDATE SET root=excluded.root,kind=excluded.kind,changed=max(changed,excluded.changed),pending_directory=excluded.pending_directory");
        self.execute(seen.build()).await?;
        let mut observations = QueryBuilder::<Sqlite>::new(
            "INSERT INTO local_files(source_key,path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,native_id,revision,picture_index,artist_pictures,parse_version,state) ",
        );
        observations.push_values(files, |mut row, (file, _)| {
            row.push_bind(self.source)
                .push_bind(&file.path)
                .push_bind(&file.root)
                .push_bind(&file.relative_path)
                .push_bind(file.kind)
                .push_bind(file.size_bytes)
                .push_bind(file.mtime_ns)
                .push_bind(file.device_id)
                .push_bind(file.inode)
                .push_bind(&file.native_id)
                .push_bind(&file.revision)
                .push_bind(file.picture_index)
                .push_bind(&file.artist_pictures)
                .push_bind(
                    (file.state != crate::LocalFileState::Observed)
                        .then_some(file.parse_version)
                        .flatten(),
                )
                .push_bind(
                    if file.kind == crate::LocalFileKind::Directory
                        && file.state == crate::LocalFileState::Accepted
                    {
                        crate::LocalFileState::Observed
                    } else {
                        file.state
                    },
                );
        });
        let unchanged = "local_files.size_bytes IS excluded.size_bytes AND local_files.mtime_ns=excluded.mtime_ns AND local_files.revision IS excluded.revision";
        // A completed directory listing waits for pruning before its token is reusable.
        let observed = "excluded.state='observed' AND (excluded.kind<>'directory' OR (SELECT pending_directory FROM temp.scan_local_seen WHERE path=excluded.path))";
        observations.push(format!(" ON CONFLICT(source_key,path) DO UPDATE SET
            root=excluded.root,relative_path=excluded.relative_path,kind=excluded.kind,
            size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,device_id=excluded.device_id,
            inode=excluded.inode,native_id=excluded.native_id,revision=excluded.revision,
            picture_index=CASE WHEN {observed} AND {unchanged} THEN local_files.picture_index ELSE excluded.picture_index END,
            artist_pictures=CASE WHEN {unchanged} THEN COALESCE(excluded.artist_pictures,local_files.artist_pictures) ELSE excluded.artist_pictures END,
            parse_version=CASE WHEN {observed} AND {unchanged} THEN local_files.parse_version ELSE excluded.parse_version END,
            state=CASE WHEN {observed} AND {unchanged} THEN local_files.state ELSE excluded.state END
            WHERE NOT ({unchanged}) OR local_files.root IS NOT excluded.root
               OR local_files.relative_path IS NOT excluded.relative_path OR local_files.kind IS NOT excluded.kind
               OR local_files.device_id IS NOT excluded.device_id OR local_files.inode IS NOT excluded.inode
               OR local_files.native_id IS NOT excluded.native_id
               OR (NOT ({observed}) AND (local_files.picture_index IS NOT excluded.picture_index
                   OR local_files.parse_version IS NOT excluded.parse_version OR local_files.state IS NOT excluded.state))
               OR (excluded.artist_pictures IS NOT NULL AND local_files.artist_pictures IS NOT excluded.artist_pictures)"));
        self.execute(observations.build()).await?;

        let parsed = files
            .iter()
            .filter(|(file, _)| {
                matches!(
                    file.state,
                    crate::LocalFileState::Accepted | crate::LocalFileState::Rejected
                )
            })
            .collect::<Vec<_>>();
        if parsed.is_empty() {
            return Ok(());
        }
        let mut clear = QueryBuilder::<Sqlite>::new(
            "DELETE FROM local_file_dependencies WHERE local_file_key IN (SELECT local_file_key FROM local_files WHERE source_key=",
        );
        clear.push_bind(self.source).push(" AND path IN (");
        let mut separated = clear.separated(",");
        for (file, _) in &parsed {
            separated.push_bind(&file.path);
        }
        separated.push_unseparated("))");
        self.execute(clear.build()).await?;
        if parsed
            .iter()
            .any(|(_, dependencies)| !dependencies.is_empty())
        {
            let mut dependencies =
                QueryBuilder::<Sqlite>::new("WITH incoming(path,dependency_path,position) AS (");
            dependencies.push_values(
                parsed.iter().flat_map(|(file, values)| {
                    values
                        .iter()
                        .enumerate()
                        .map(move |(position, dependency)| (&file.path, dependency, position))
                }),
                |mut row, (path, dependency, position)| {
                    row.push_bind(path)
                        .push_bind(dependency)
                        .push_bind(position as i64);
                },
            );
            dependencies
                .push(
                    ") INSERT INTO local_file_dependencies(local_file_key,dependency_path,position)
                SELECT file.local_file_key,incoming.dependency_path,incoming.position FROM incoming
                JOIN local_files file ON file.path=incoming.path AND file.source_key=",
                )
                .push_bind(self.source);
            self.execute(dependencies.build()).await?;
        }
        Ok(())
    }

    pub async fn remove_local_file_paths(&mut self, paths: &[String]) -> LibraryResult<()> {
        if paths.len() > 128 {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "Local removal batch exceeds 128 paths".to_string(),
            ));
        }
        if paths.is_empty() {
            return Ok(());
        }
        if paths.iter().any(String::is_empty) {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "invalid Local removal path".to_string(),
            ));
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT OR IGNORE INTO temp.scan_local_file_removals(path) ",
        );
        query.push_values(paths, |mut row, path| {
            row.push_bind(path);
        });
        self.execute(query.build()).await
    }

    pub async fn local_dependency_paths(&mut self, paths: &[String]) -> LibraryResult<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        if paths.len() > 128 {
            return Err(LibraryError::InvalidScan(
                "Local dependency lookup exceeds 128 paths".to_string(),
            ));
        }
        if let Some(writer) = self.batch_writer.as_mut() {
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            return fetch_local_dependency_paths(connection, paths).await;
        }
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        fetch_local_dependency_paths(connection, paths).await
    }

    /// Reuse a directory token known by the source to cover descendants.
    pub async fn retain_file_tree(
        &mut self,
        directory: &crate::LocalFileWrite,
    ) -> LibraryResult<bool> {
        let previous = self
            .local_file_reuse_candidates(std::slice::from_ref(directory))
            .await?;
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let fresh: bool =
            sqlx::query_scalar("SELECT freshness IS NOT NULL FROM sources WHERE source_key=?1")
                .bind(self.source)
                .fetch_one(&mut *connection)
                .await?;
        if !fresh
            || directory.revision.is_none()
            || !previous.iter().any(|old| {
                old.path == directory.path
                    && old.state == crate::LocalFileState::Accepted
                    && old.revision == directory.revision
                    && old.parse_version == directory.parse_version
            })
        {
            return Ok(false);
        }
        sqlx::query("INSERT OR IGNORE INTO temp.scan_local_seen(path,root,kind,changed,pending_directory,previous_revision,previous_parse_version,previous_accepted)
            SELECT path,root,kind,0,kind='directory' AND (state<>'accepted' OR parse_version IS NOT ?3),
                CASE WHEN kind='directory' THEN revision END,CASE WHEN kind='directory' THEN parse_version END,
                kind='directory' AND state='accepted'
            FROM local_files WHERE source_key=?1 AND path>=?2 AND path<?2||char(1114111)")
            .bind(self.source).bind(format!("{}/",directory.path.trim_end_matches('/')))
            .bind(directory.parse_version).execute(&mut *connection).await?;
        sqlx::query("UPDATE temp.scan_local_seen SET pending_directory=0 WHERE path=?1")
            .bind(&directory.path)
            .execute(&mut *connection)
            .await?;
        sqlx::query(
            "UPDATE local_files SET parse_version=?3
            WHERE source_key=?1 AND path=?2 AND parse_version IS NOT ?3",
        )
        .bind(self.source)
        .bind(&directory.path)
        .bind(directory.parse_version)
        .execute(&mut *connection)
        .await?;
        Ok(true)
    }

    /// A depth-one DAV token covers this listing; child tokens cover their descendants.
    pub async fn retain_file_directory_children(&mut self, path: &str) -> LibraryResult<()> {
        self.execute(sqlx::query("INSERT OR IGNORE INTO temp.scan_local_seen(path,root,kind,changed,pending_directory,previous_revision,previous_parse_version,previous_accepted)
            SELECT path,root,kind,0,kind='directory',
                CASE WHEN kind='directory' THEN revision END,CASE WHEN kind='directory' THEN parse_version END,
                kind='directory' AND state='accepted' FROM local_files
            WHERE source_key=?1 AND path>=?2 AND path<?2||char(1114111)
              AND instr(substr(rtrim(path,'/'),length(?2)+1),'/')=0")
            .bind(self.source).bind(format!("{}/",path.trim_end_matches('/')))).await?;
        self.execute(
            sqlx::query("UPDATE temp.scan_local_seen SET pending_directory=0 WHERE path=?1")
                .bind(path),
        )
        .await
    }

    pub async fn local_inventory_path_page(
        &mut self,
        kind: crate::LocalFileKind,
        after: Option<&str>,
        exclude_cue_dependencies: bool,
        limit: usize,
    ) -> LibraryResult<Vec<String>> {
        self.local_inventory_path_page_in_root(kind, after, exclude_cue_dependencies, limit, None)
            .await
    }

    pub async fn local_inventory_path_page_in_root(
        &mut self,
        kind: crate::LocalFileKind,
        after: Option<&str>,
        exclude_cue_dependencies: bool,
        limit: usize,
        root: Option<&str>,
    ) -> LibraryResult<Vec<String>> {
        let query = || {
            sqlx::query_scalar::<_, String>(
                "SELECT file.path FROM temp.scan_local_seen file
             WHERE file.kind=?1 AND file.path>?2 AND (?5 IS NULL OR file.root=?5)
               AND (file.kind<>'directory' OR file.pending_directory)
               AND (?3=0 OR NOT EXISTS (
                 SELECT 1 FROM temp.scan_local_dependency_paths dependency
                 WHERE dependency.path=file.path
               ))
             ORDER BY file.path LIMIT ?4",
            )
            .bind(kind)
            .bind(after.unwrap_or(""))
            .bind(exclude_cue_dependencies)
            .bind(limit.clamp(1, 128) as i64)
            .bind(root)
        };
        if let Some(writer) = self.batch_writer.as_mut() {
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            return Ok(query().fetch_all(&mut *connection).await?);
        }
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(query().fetch_all(&mut *connection).await?)
    }

    pub async fn local_inventory_file_page(
        &mut self,
        kind: crate::LocalFileKind,
        after: Option<&str>,
    ) -> LibraryResult<Vec<crate::LocalFileWrite>> {
        self.local_inventory_file_page_in_root(kind, after, None)
            .await
    }

    pub async fn local_inventory_file_page_in_root(
        &mut self,
        kind: crate::LocalFileKind,
        after: Option<&str>,
        root: Option<&str>,
    ) -> LibraryResult<Vec<crate::LocalFileWrite>> {
        let query = || {
            sqlx::query_as::<_, crate::LocalFileWrite>("SELECT file.path,file.root,file.relative_path,file.kind,file.size_bytes,file.mtime_ns,file.device_id,file.inode,file.native_id,file.revision,file.picture_index,file.artist_pictures,file.parse_version,file.state FROM local_files file JOIN temp.scan_local_seen seen USING(path) WHERE source_key=?4 AND file.kind=?1 AND path>?2 AND (?3 IS NULL OR file.root=?3) ORDER BY path LIMIT 128")
            .bind(kind).bind(after.unwrap_or("")).bind(root).bind(self.source)
        };
        let rows = if let Some(writer) = self.batch_writer.as_mut() {
            query()
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?
        } else {
            let mut writer = self.database.writer().await?;
            query()
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?
        };
        Ok(rows)
    }

    pub async fn local_inventory_files(
        &mut self,
        paths: &[String],
    ) -> LibraryResult<Vec<crate::LocalFileWrite>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        if paths.len() > 128 {
            return Err(LibraryError::InvalidScan(
                "File inventory lookup exceeds 128 paths".into(),
            ));
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT file.path,file.root,file.relative_path,file.kind,file.size_bytes,file.mtime_ns,file.device_id,file.inode,file.native_id,file.revision,file.picture_index,file.artist_pictures,file.parse_version,file.state FROM local_files file JOIN temp.scan_local_seen seen USING(path) WHERE source_key=",
        );
        query.push_bind(self.source).push(" AND path IN (");
        let mut values = query.separated(",");
        for path in paths {
            values.push_bind(path);
        }
        values.push_unseparated(")");
        let rows = if let Some(writer) = self.batch_writer.as_mut() {
            query
                .build_query_as::<crate::LocalFileWrite>()
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?
        } else {
            let mut writer = self.database.writer().await?;
            query
                .build_query_as::<crate::LocalFileWrite>()
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?
        };
        Ok(rows)
    }

    /// At most two paths are needed to distinguish a unique native ID from aliases.
    pub async fn local_inventory_native_paths(
        &mut self,
        native_id: &str,
    ) -> LibraryResult<Vec<String>> {
        let query = || {
            sqlx::query_scalar(
                "SELECT path FROM local_files JOIN temp.scan_local_seen USING(path) WHERE source_key=?2 AND native_id=?1 ORDER BY path LIMIT 2",
            )
            .bind(native_id).bind(self.source)
        };
        if let Some(writer) = self.batch_writer.as_mut() {
            return Ok(query()
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?);
        }
        let mut writer = self.database.writer().await?;
        Ok(query()
            .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
            .await?)
    }
}

async fn fetch_local_dependency_paths(
    connection: &mut sqlx::SqliteConnection,
    paths: &[String],
) -> LibraryResult<Vec<String>> {
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT path FROM temp.scan_local_dependency_paths WHERE path IN (",
    );
    let mut separated = query.separated(",");
    for path in paths {
        separated.push_bind(path);
    }
    separated.push_unseparated(")");
    Ok(query
        .build_query_scalar()
        .persistent(false)
        .fetch_all(connection)
        .await?)
}

pub(super) async fn prune_local_files(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
    full: bool,
) -> LibraryResult<()> {
    sqlx::query(
        "DELETE FROM local_files WHERE source_key=?1 AND
         ((?2 AND path NOT IN (SELECT path FROM temp.scan_local_seen))
          OR path IN (SELECT path FROM temp.scan_local_file_removals))",
    )
    .bind(source_key)
    .bind(full)
    .execute(&mut **transaction)
    .await?;
    if full {
        sqlx::query("UPDATE local_files SET state='accepted' WHERE source_key=?1 AND kind='directory' AND state='observed'
            AND path IN (SELECT path FROM temp.scan_local_seen WHERE kind='directory' AND pending_directory=0)")
            .bind(source_key).execute(&mut **transaction).await?;
    } else {
        sqlx::query("UPDATE local_files SET revision=NULL,parse_version=NULL
            WHERE source_key=?1 AND kind='directory' AND path IN (SELECT path FROM temp.scan_local_seen)")
            .bind(source_key).execute(&mut **transaction).await?;
    }
    Ok(())
}
