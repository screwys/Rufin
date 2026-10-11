use super::*;

impl AndroidBrowseQuery {
    pub(super) async fn folder_sections(&self) -> Result<Vec<AndroidScrollSection>, AndroidError> {
        let source = self.selected().source_key;
        let cancel = ReadCancellation::new();
        let projection = self.folder_projection().await.map_err(error)?;
        let mut folders = match projection {
            FolderProjection::Live { .. } => self
                .database
                .ordered_name_sections(
                    projection
                        .live_folders(&self.filter, self.descending)
                        .map(|folder| folder.name.as_str()),
                    &cancel,
                )
                .await
                .map_err(error)?,
            FolderProjection::Cached(folder) => self
                .database
                .folder_section_positions(source, *folder, &self.filter, self.descending, &cancel)
                .await
                .map_err(error)?,
        };
        let tracks = self
            .database
            .song_sections(
                self.song_query()
                    .await
                    .map_err(error)?
                    .expect("Folder query"),
                &cancel,
            )
            .await
            .map_err(error)?;
        let (count, _) = self.folder_counts().await.map_err(error)?;
        for mut section in tracks {
            section.index += count as u64;
            if let Some(existing) = folders
                .iter_mut()
                .find(|existing| existing.title == section.title)
            {
                existing.index = existing.index.min(section.index);
            } else {
                folders.push(section);
            }
        }
        folders.sort_by_key(|section| section.index);
        Ok(folders
            .into_iter()
            .map(|section| AndroidScrollSection {
                title: section.title,
                index: section.index,
            })
            .collect())
    }
    pub(super) async fn folder_play(
        &self,
        index: u64,
        media_uri: String,
        placement: playback::QueuePlacement,
        shuffled: bool,
        context_title: Option<Arc<library::QueueContextTitle>>,
    ) -> Result<(), AndroidError> {
        let (folders, _) = self.folder_counts().await.map_err(error)?;
        let rank = usize::try_from(index)
            .map_err(error)?
            .saturating_sub(folders);
        let selection = self.folder_selection().await?;
        let mut request = playback::PlayRequest::captured(selection, rank, placement, false)
            .with_context(self.context_id())
            .with_context_title(context_title)
            .shuffled(shuffled);
        if let Some(key) = self
            .database
            .track_key_by_uri(&media_uri, &ReadCancellation::new())
            .await
            .map_err(error)?
        {
            request = request.with_anchor(key, None);
        }
        self.queue.play(request);
        Ok(())
    }

    pub(super) async fn folder_selection(&self) -> Result<SongSelection, AndroidError> {
        Ok(self
            .song_query()
            .await
            .map_err(error)?
            .expect("Folder query")
            .clone()
            .selection())
    }

    pub(super) async fn folder_projection(&self) -> library::LibraryResult<&FolderProjection> {
        self.folder_projection
            .get_or_try_init(|| async {
                let Route::Folders { path } = &self.route else {
                    unreachable!("Folder route")
                };
                rufin_core::source::folders::resolve_folder(
                    self.selected(),
                    path.last().map(|item| item.id.as_str()),
                    &ReadCancellation::new(),
                )
                .await
            })
            .await
    }

    pub(super) async fn folder_counts(&self) -> library::LibraryResult<(usize, i64)> {
        let cancel = ReadCancellation::new();
        let projection = self.folder_projection().await?;
        let folders = match projection {
            FolderProjection::Live { .. } => projection
                .live_folders(&self.filter, self.descending)
                .count(),
            FolderProjection::Cached(folder) => {
                self.database
                    .filtered_folder_page(
                        self.selected().source_key,
                        *folder,
                        &self.filter,
                        self.descending,
                        0,
                        0,
                        &cancel,
                    )
                    .await?
                    .0
            }
        };
        Ok((
            folders,
            self.database
                .song_count(self.song_query().await?.expect("Folder query"), &cancel)
                .await?,
        ))
    }

    fn folder_row(
        &self,
        id: String,
        name: String,
        artwork_identity: Option<Vec<u8>>,
    ) -> AndroidBrowseRow {
        let Route::Folders { path } = &self.route else {
            unreachable!("Folder route")
        };
        let mut path = path.clone();
        path.push(rufin_core::route::FolderPathItem {
            id: id.clone(),
            name: name.clone(),
        });
        AndroidBrowseRow {
            track_key: None,
            kind: "folder".into(),
            playback_context_id: None,
            key: id,
            title: name,
            artist: String::new(),
            album: String::new(),
            fields: Vec::new(),
            media_uri: String::new(),
            subtitle: String::new(),
            favorite: false,
            duration_millis: 0,
            detail_route: Some(route_json(Route::Folders { path })),
            pin: None,
            section: String::new(),
            section_id: String::new(),
            section_kind: String::new(),
            section_refreshable: false,
            year: None,
            track_count: 0,
            source_id: Some(self.selected().source_id.to_string()),
            source_name: String::new(),
            last_played: None,
            writable: false,
            downloaded: false,
            artwork_identity,
        }
    }

    pub(super) async fn folder_rows(
        &self,
        offset: usize,
        limit: usize,
    ) -> library::LibraryResult<Vec<AndroidBrowseRow>> {
        let source = self.selected().source_key;
        let cancel = ReadCancellation::new();
        let projection = self.folder_projection().await?;
        let (folder_count, mut rows) = match projection {
            FolderProjection::Live { .. } => (
                projection
                    .live_folders(&self.filter, self.descending)
                    .count(),
                projection
                    .live_folders(&self.filter, self.descending)
                    .skip(offset)
                    .take(limit)
                    .map(|folder| {
                        self.folder_row(folder.object_id.clone(), folder.name.clone(), None)
                    })
                    .collect::<Vec<_>>(),
            ),
            FolderProjection::Cached(folder) => {
                let (count, folders) = self
                    .database
                    .filtered_folder_page(
                        source,
                        *folder,
                        &self.filter,
                        self.descending,
                        offset,
                        limit,
                        &cancel,
                    )
                    .await?;
                (
                    count,
                    folders
                        .into_iter()
                        .map(|folder| {
                            self.folder_row(folder.object_id, folder.name, folder.artwork_binding)
                        })
                        .collect(),
                )
            }
        };
        let tracks = self
            .database
            .song_rows(
                self.song_query().await?.expect("Folder query"),
                offset.saturating_sub(folder_count),
                limit.saturating_sub(rows.len()),
                &cancel,
            )
            .await?;
        rows.extend(tracks.into_iter().enumerate().map(|(index, track)| {
            self.track_row(track, offset.saturating_sub(folder_count) + index)
        }));
        Ok(rows)
    }
}
