use super::*;

impl AndroidBrowseQuery {
    pub(super) async fn folder_sections(&self) -> Result<Vec<AndroidScrollSection>, AndroidError> {
        let query = self
            .song_query()
            .await
            .map_err(error)?
            .expect("Folder query");
        Ok(self
            .folder_projection()
            .await
            .map_err(error)?
            .sections(
                &self.database,
                self.selected().source_key,
                query,
                &ReadCancellation::new(),
            )
            .await
            .map_err(error)?
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
        track_key: Option<i64>,
        placement: rufin_core::playback::QueuePlacement,
        shuffled: bool,
        context_title: Option<Arc<library::QueueContextTitle>>,
    ) -> Result<(), AndroidError> {
        let (folders, _) = self.folder_counts().await.map_err(error)?;
        let rank = usize::try_from(index)
            .map_err(error)?
            .saturating_sub(folders);
        let selection = self.folder_selection().await?;
        let mut request =
            rufin_core::playback::PlayRequest::captured(selection, rank, placement, false)
                .with_context(self.context_id())
                .with_context_title(context_title)
                .shuffled(shuffled);
        let key = match track_key {
            Some(key) => Some(library::TrackKey::from_raw(key)),
            None => self
                .database
                .track_key_by_uri(&media_uri, &ReadCancellation::new())
                .await
                .map_err(error)?,
        };
        if let Some(key) = key {
            request = request.with_anchor(key, None);
        }
        self.playback.play_selection(request);
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
                    &self.source,
                    &self.database,
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
        let folders = projection
            .child_page(
                &self.database,
                self.selected().source_key,
                self.song_query().await?.expect("Folder query"),
                0..0,
                &cancel,
            )
            .await?
            .0;
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
            context_source_id: Some(self.selected().source_id.to_string()),
            ..AndroidBrowseRow::new(AndroidBrowseItem::Folder {
                key: id,
                title: name,
                route: Route::Folders { path },
                artwork: artwork_identity,
            })
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
        let (folder_count, folders) = projection
            .child_page(
                &self.database,
                source,
                self.song_query().await?.expect("Folder query"),
                offset..offset.saturating_add(limit),
                &cancel,
            )
            .await?;
        let mut rows = folders
            .into_iter()
            .map(|folder| self.folder_row(folder.object_id, folder.name, folder.artwork_binding))
            .collect::<Vec<_>>();
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
