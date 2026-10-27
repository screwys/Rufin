//! Device-local representations of shared collection entries.
use crate::{Catalog, LibraryResult, LocalFileWrite, LocalMediaLocation};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogFileStamp {
    pub size_bytes: Option<i64>,
    pub mtime_ns: i64,
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogFileDependency {
    pub path: String,
    pub accepted: Option<CatalogFileStamp>,
    pub file: Option<LocalFileWrite>,
    #[serde(default)]
    pub root_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogFile {
    pub file: LocalFileWrite,
    #[serde(default)]
    pub root_id: Option<String>,
    pub parsed: Option<(CatalogFileStamp, Option<i64>)>,
    pub directory_complete: bool,
    pub dependencies: Vec<CatalogFileDependency>,
}

impl Catalog {
    pub async fn connect_original_file(
        &self,
        uri: &str,
        resolve: impl Fn(std::path::PathBuf) -> std::path::PathBuf,
    ) -> LibraryResult<Option<LocalMediaLocation>> {
        let mut reader = self.acquire_reader().await?;
        let backing = crate::cue_media_parts(uri).map(|(_, backing, _, _)| backing);
        let access: Option<String> = sqlx::query_scalar("SELECT access_uri FROM local_locators WHERE media_uri IN (?1,?2) AND origin IN ('local','import','mapping') ORDER BY media_uri=?1 DESC LIMIT 1")
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
            "SELECT access.source_path FROM tracks t JOIN sources source USING(source_key) JOIN local_track_access access USING(track_key) WHERE media_uri=?1 AND source.kind='local'",
        )
        .bind(uri)
        .fetch_optional(&mut *reader)
        .await?;
        if let Some(uri) = path.as_deref().and_then(crate::document_access_uri) {
            return Ok(Some(LocalMediaLocation::Document(uri)));
        }
        Ok(path
            .map(std::path::PathBuf::from)
            .map(resolve)
            .filter(|path| path.is_file())
            .map(LocalMediaLocation::File))
    }
}
