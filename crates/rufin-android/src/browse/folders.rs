use super::*;

impl AndroidBrowseQuery {
    pub(super) async fn folder_sections(&self) -> Result<Vec<AndroidScrollSection>, AndroidError> {
        let source = self.selected().source_key;
        let cancel = ReadCancellation::new();
        let (mut folders, tracks) = match self.folder_projection().await.map_err(error)? {
            FolderProjection::Live {
                folders,
                candidates,
            } => (
                self.database
                    .ordered_name_sections(
                        &folders
                            .iter()
                            .map(|folder| folder.name.clone())
                            .collect::<Vec<_>>(),
                        &cancel,
                    )
                    .await
                    .map_err(error)?,
                self.database
                    .live_folder_track_sections(
                        source,
                        candidates,
                        &self.filter,
                        self.sort.track_sort(),
                        self.descending,
                        &cancel,
                    )
                    .await
                    .map_err(error)?,
            ),
            FolderProjection::Cached(folder) => (
                self.database
                    .folder_section_positions(
                        source,
                        *folder,
                        &self.filter,
                        self.descending,
                        &cancel,
                    )
                    .await
                    .map_err(error)?,
                self.database
                    .track_section_positions(
                        &library::TrackQuery {
                            source,
                            collection: None,
                            folder: *folder,
                            downloaded_only: false,
                            favorites_only: false,
                        },
                        &self.filter,
                        self.sort.track_sort(),
                        self.descending,
                        &cancel,
                    )
                    .await
                    .map_err(error)?,
            ),
        };
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
        let (folder_count, _) = self.folder_counts().await.map_err(error)?;
        let rank = usize::try_from(index)
            .map_err(error)?
            .saturating_sub(folder_count);
        let input = self.folder_input(Some(media_uri)).await?;
        self.queue.play(
            playback::PlayRequest::captured(input, rank, placement, false)
                .with_context_title(context_title)
                .shuffled(shuffled),
        );
        Ok(())
    }

    pub(super) async fn folder_target_play(
        &self,
        placement: playback::QueuePlacement,
        shuffled: bool,
        context_title: Option<Arc<library::QueueContextTitle>>,
    ) -> Result<(), AndroidError> {
        let input = self.folder_input(None).await?;
        self.queue.play(
            playback::PlayRequest::ordered(input, 0, placement, true)
                .with_context_title(context_title)
                .shuffled(shuffled),
        );
        Ok(())
    }

    pub(super) async fn folder_target_uris(&self) -> Result<Vec<String>, AndroidError> {
        match self.folder_input(None).await? {
            library::QueueInput::Uris { order, .. } => Ok(order.to_vec()),
            library::QueueInput::Query {
                folder,
                filter,
                sort,
                descending,
                ..
            } => self
                .database
                .query_track_media_uris(
                    &library::TrackQuery {
                        source: self.selected().source_key,
                        collection: None,
                        folder,
                        downloaded_only: false,
                        favorites_only: false,
                    },
                    &filter,
                    sort,
                    descending,
                    &ReadCancellation::new(),
                )
                .await
                .map_err(error),
            _ => unreachable!("Folder input"),
        }
    }

    async fn folder_input(
        &self,
        anchor_uri: Option<String>,
    ) -> Result<library::QueueInput, AndroidError> {
        let context: Arc<str> = format!(
            "android:{}:{}:{}",
            self.selected().source_id,
            route_json(self.route.clone()),
            self.filter
        )
        .into();
        Ok(match self.folder_projection().await.map_err(error)? {
            FolderProjection::Live { candidates, .. } => library::QueueInput::Uris {
                order: self
                    .database
                    .live_folder_track_order(
                        self.selected().source_key,
                        candidates,
                        &self.filter,
                        self.sort.track_sort(),
                        self.descending,
                        &ReadCancellation::new(),
                    )
                    .await
                    .map_err(error)?
                    .into(),
                context_id: context,
                source_start: 0,
            },
            FolderProjection::Cached(folder) => library::QueueInput::Query {
                query: library::QueueQuery::Tracks {
                    source: self.selected().source_key,
                    downloaded_only: false,
                    favorites_only: false,
                    recursive: false,
                },
                folder: *folder,
                filter: self.filter.clone(),
                sort: self.sort.track_sort(),
                descending: self.descending,
                context_id: context,
                anchor_uri,
            },
        })
    }
    pub(super) async fn folder_projection(&self) -> library::LibraryResult<&FolderProjection> {
        self.folder_projection
            .get_or_try_init(|| async {
                let selected = self.selected();
                let Route::Folders { path } = &self.route else {
                    unreachable!("Folder route")
                };
                let projection = rufin_core::source::folders::resolve_folder(
                    selected,
                    path.last().map(|item| item.id.as_str()),
                    &ReadCancellation::new(),
                )
                .await?;
                match projection {
                    FolderProjection::Live {
                        folders,
                        candidates,
                    } => {
                        let filter = self.filter.trim().to_lowercase();
                        let mut folders = folders
                            .into_iter()
                            .filter(|folder| folder.name.to_lowercase().contains(&filter))
                            .collect::<Vec<_>>();
                        folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                        if self.descending {
                            folders.reverse();
                        }
                        Ok(FolderProjection::Live {
                            folders,
                            candidates,
                        })
                    }
                    cached => Ok(cached),
                }
            })
            .await
    }

    pub(super) async fn folder_counts(&self) -> library::LibraryResult<(usize, i64)> {
        let source = self.selected().source_key;
        let cancel = ReadCancellation::new();
        match self.folder_projection().await? {
            FolderProjection::Live {
                folders,
                candidates,
            } => Ok((
                folders.len(),
                self.database
                    .live_folder_track_count(source, candidates, &self.filter, &cancel)
                    .await?,
            )),
            FolderProjection::Cached(folder) => {
                let (folders, _) = self
                    .database
                    .filtered_folder_page(
                        source,
                        *folder,
                        &self.filter,
                        self.descending,
                        0,
                        0,
                        &cancel,
                    )
                    .await?;
                Ok((
                    folders,
                    self.database
                        .track_count(source, *folder, false, &self.filter, &cancel)
                        .await?,
                ))
            }
        }
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
        let (folder_count, mut rows, tracks) = match self.folder_projection().await? {
            FolderProjection::Live {
                folders,
                candidates,
            } => {
                let rows = folders
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .map(|folder| {
                        self.folder_row(folder.object_id.clone(), folder.name.clone(), None)
                    })
                    .collect::<Vec<_>>();
                let tracks = self
                    .database
                    .live_folder_track_page(
                        source,
                        candidates,
                        &self.filter,
                        self.sort.track_sort(),
                        self.descending,
                        offset.saturating_sub(folders.len()),
                        limit.saturating_sub(rows.len()),
                        &cancel,
                    )
                    .await?;
                (folders.len(), rows, tracks)
            }
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
                let rows = folders
                    .into_iter()
                    .map(|folder| {
                        self.folder_row(folder.object_id, folder.name, folder.artwork_binding)
                    })
                    .collect::<Vec<_>>();
                let tracks = self
                    .database
                    .track_page(
                        source,
                        *folder,
                        false,
                        &self.filter,
                        self.sort.track_sort(),
                        self.descending,
                        offset.saturating_sub(count),
                        limit.saturating_sub(rows.len()),
                        &cancel,
                    )
                    .await?;
                (count, rows, tracks)
            }
        };
        rows.extend(tracks.into_iter().enumerate().map(|(index, track)| {
            self.track_row(track, offset.saturating_sub(folder_count) + index)
        }));
        Ok(rows)
    }
}
