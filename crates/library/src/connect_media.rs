//! Where a track a peer asks for lives on this device.
use crate::{Catalog, LibraryResult, LocalMediaLocation};

impl Catalog {
    pub async fn connect_original_file(
        &self,
        uri: &str,
        resolve: impl Fn(std::path::PathBuf) -> std::path::PathBuf,
    ) -> LibraryResult<Option<LocalMediaLocation>> {
        let mut reader = self.acquire_reader().await?;
        let backing = crate::cue_media_parts(uri).map(|(_, backing, _, _)| backing);
        let access: Option<String> = sqlx::query_scalar("SELECT access_uri FROM local_locators WHERE media_uri IN (?1,?2) AND origin IN ('local','import') ORDER BY media_uri=?1 DESC LIMIT 1")
            .bind(uri).bind(backing.as_deref()).fetch_optional(&mut *reader).await?;
        if let Some(uri) = access.as_ref().filter(|uri| uri.starts_with("content://")) {
            return Ok(Some(LocalMediaLocation::Document(uri.clone())));
        }
        if let Some(path) = access
            .and_then(|uri| crate::file_media_path(&uri))
            .map(&resolve)
            .filter(|path| path.is_file())
        {
            return Ok(Some(LocalMediaLocation::File(path)));
        }
        let path: Option<String> = sqlx::query_scalar(
            "SELECT file.path FROM tracks t JOIN sources source USING(source_key) JOIN track_details details USING(track_key) JOIN files file ON file.source_key=t.source_key AND file.path=details.source_path WHERE media_uri=?1 AND source.kind='local'",
        )
        .bind(uri)
        .fetch_optional(&mut *reader)
        .await?;
        drop(reader);
        if let Some(uri) = path.as_deref().and_then(crate::document_access_uri) {
            return Ok(Some(LocalMediaLocation::Document(uri)));
        }
        if let Some(path) = path
            .map(std::path::PathBuf::from)
            .map(resolve)
            .filter(|path| path.is_file())
        {
            return Ok(Some(LocalMediaLocation::File(path)));
        }
        Ok(self.located_mapped_file(uri).await?)
    }
}
