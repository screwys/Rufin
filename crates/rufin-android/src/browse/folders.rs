use super::*;

impl AndroidBrowseQuery {
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
        page: &rufin_core::browse::FolderPage,
        offset: usize,
        limit: usize,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Vec<AndroidBrowseRow>> {
        let folders = if offset < page.folder_count {
            page.projection
                .child_page(
                    &self.database,
                    self.selected().source_key,
                    &page.read,
                    offset..offset.saturating_add(limit),
                    &cancel,
                )
                .await?
                .1
        } else {
            Vec::new()
        };
        let mut rows = folders
            .into_iter()
            .map(|folder| self.folder_row(folder.object_id, folder.name, folder.artwork_binding))
            .collect::<Vec<_>>();
        rows.extend(
            self.member_rows(
                page.read
                    .rows(
                        &self.database,
                        offset.saturating_sub(page.folder_count),
                        limit.saturating_sub(rows.len()),
                        &cancel,
                    )
                    .await?,
                offset.saturating_sub(page.folder_count),
            ),
        );
        Ok(rows)
    }

    pub(super) fn folder_seed(
        &self,
        page: &rufin_core::browse::FolderPage,
    ) -> Vec<AndroidBrowseRow> {
        let mut rows = page
            .folders
            .iter()
            .map(|folder| {
                self.folder_row(
                    folder.object_id.clone(),
                    folder.name.clone(),
                    folder.artwork_binding.clone(),
                )
            })
            .collect::<Vec<_>>();
        rows.extend(self.member_rows(
            page.tracks.clone(),
            page.first_row_position.saturating_sub(page.folder_count),
        ));
        rows
    }
}
