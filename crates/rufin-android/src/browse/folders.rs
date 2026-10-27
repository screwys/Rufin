use super::*;

impl AndroidBrowseQuery {
    pub(super) fn folder_row(
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
}
