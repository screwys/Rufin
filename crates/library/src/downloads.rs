//! Download records: one row per track, written after the file lands. A track is downloaded
//! while it has a row, at whatever quality it was downloaded.
use crate::{Catalog, LibraryResult};

#[derive(Clone, Debug, Default, sqlx::FromRow)]
pub struct DownloadFile {
    pub media_uri: String,
    #[sqlx(rename = "quality")]
    pub encoding: String,
    pub revision: String,
    pub path: String,
    pub managed: bool,
    pub source_id: Option<String>,
    /// The requests and rules that keep the copy, as a JSON list.
    #[sqlx(rename = "reason")]
    pub owners: String,
    #[sqlx(rename = "root")]
    pub storage_root: String,
    pub relative_path: String,
    #[sqlx(rename = "size")]
    pub completed_size: Option<i64>,
    /// Replaced documents that still wait for deletion, as a JSON list.
    #[sqlx(rename = "retired")]
    pub retired_documents: String,
}

pub fn media_encoding_key(encoding: Option<crate::AudioEncoding>) -> String {
    match encoding {
        None => "original".into(),
        Some(encoding) if encoding == crate::AudioEncoding::mp3() => "mp3".into(),
        Some(encoding) => {
            serde_json::to_string(&encoding).expect("an audio encoding can be stored")
        }
    }
}

impl Catalog {
    pub async fn download_file(&self, uri: &str) -> LibraryResult<Option<DownloadFile>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT * FROM downloads WHERE media_uri=?1")
            .bind(uri)
            .fetch_optional(&mut *reader)
            .await?)
    }

    /// Whether another record uses the same file.
    pub async fn download_file_shared(&self, file: &DownloadFile) -> LibraryResult<bool> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM downloads WHERE path=?1 AND (media_uri,revision)<>(?2,?3))")
            .bind(&file.path).bind(&file.media_uri).bind(&file.revision)
            .fetch_one(&mut *reader).await?)
    }

    /// Records a copy whose file landed, or updates the record of one.
    pub async fn save_download(&self, file: &DownloadFile) -> LibraryResult<()> {
        let mut write = self.write().await?;
        sqlx::query("INSERT INTO downloads(media_uri,quality,revision,path,managed,source_id,reason,root,relative_path,size,retired) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
            ON CONFLICT(media_uri) DO UPDATE SET quality=excluded.quality,revision=excluded.revision,path=excluded.path,managed=excluded.managed,source_id=coalesce(excluded.source_id,downloads.source_id),reason=excluded.reason,root=excluded.root,relative_path=excluded.relative_path,size=excluded.size,retired=excluded.retired")
            .bind(&file.media_uri).bind(&file.encoding).bind(&file.revision).bind(&file.path).bind(file.managed).bind(&file.source_id)
            .bind(if file.owners.is_empty() { "[]" } else { &file.owners }).bind(&file.storage_root).bind(&file.relative_path).bind(file.completed_size)
            .bind(if file.retired_documents.is_empty() { "[]" } else { &file.retired_documents })
            .execute(write.connection().await?).await?;
        write.keep();
        write.read_downloads(&[&file.media_uri]).await?;
        write.commit().await?;
        Ok(())
    }

    /// Forgets the record of a copy.
    pub async fn forget_download(&self, file: &DownloadFile) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let removed =
            sqlx::query("DELETE FROM downloads WHERE media_uri=?1 AND path=?2 AND revision=?3")
                .bind(&file.media_uri)
                .bind(&file.path)
                .bind(&file.revision)
                .execute(write.connection().await?)
                .await?
                .rows_affected()
                > 0;
        if removed {
            write.keep();
            write.read_downloads(&[&file.media_uri]).await?;
        }
        write.commit().await?;
        Ok(())
    }

    pub async fn download_files_page(
        &self,
        source: Option<&str>,
        after: &str,
        owner: Option<&str>,
    ) -> LibraryResult<Vec<DownloadFile>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT * FROM downloads WHERE source_id IS ?1 AND media_uri>?2 AND (?3 IS NULL OR EXISTS(SELECT 1 FROM json_each(reason) WHERE value=json_extract(?3,'$'))) ORDER BY media_uri LIMIT 128")
            .bind(source).bind(after).bind(owner).fetch_all(&mut *reader).await?)
    }
}
