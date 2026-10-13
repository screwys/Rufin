use crate::library::AndroidLibrary;
use rufin_core::metadata::{MetadataEditor, MetadataItemId};
use sources::SourceMetadataError;
use std::sync::Arc;

#[uniffi::export]
impl AndroidLibrary {
    pub async fn edit_metadata(
        &self,
        kind: String,
        media_uri: String,
    ) -> Result<Arc<MetadataEditor>, SourceMetadataError> {
        let item = match kind.as_str() {
            "track" => MetadataItemId::Track(media_uri),
            "album" | "album_header" => MetadataItemId::Album(media_uri),
            "artist" => MetadataItemId::Artist(media_uri),
            _ => {
                return Err(SourceMetadataError::Write(
                    "Choose a track, album or artist".into(),
                ));
            }
        };
        MetadataEditor::open(self.source.clone(), item).await
    }
}
