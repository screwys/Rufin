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

/// The file behind a track this device holds as a file.
#[derive(Clone, Debug)]
pub struct FileTrack {
    /// The file's revision, as download records keep it.
    pub revision: String,
    /// The URI of the document the track is part of, for a track inside a document.
    pub backing: Option<String>,
    pub format: Option<String>,
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
    /// The file behind a track this device holds as a file, or None for another track.
    pub async fn file_track(&self, uri: &str) -> LibraryResult<Option<FileTrack>> {
        let mut reader = self.acquire_reader().await?;
        let needed = crate::connect::NEEDED_TRACK_KEYS;
        let row: Option<(Option<String>, bool, Option<String>, Option<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT (SELECT revision FROM files WHERE source_key=r.source_key AND path=coalesce(d.source_path,(SELECT path FROM local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)) LIMIT 1),
                EXISTS(SELECT 1 FROM files f WHERE f.source_key=r.source_key AND f.path=d.source_path AND f.path LIKE 'rufin-document:%') OR EXISTS(SELECT 1 FROM local_locators l WHERE l.media_uri=r.media_uri AND l.origin IN ('local','import') AND l.access_uri LIKE 'content://%'),
                coalesce(d.source_path,d.metadata->>'$.source_path'),coalesce(r.source_format,d.metadata->>'$.source_format')
             FROM tracks r LEFT JOIN track_details d USING(track_key) WHERE r.media_uri=?1 AND (r.indexed=1 OR (r.track_key IN ({needed}) AND (d.source_path IS NOT NULL OR EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=r.media_uri AND locator.origin IN ('local','import')))))"
        )))
        .bind(uri)
        .fetch_optional(&mut *reader)
        .await?;
        Ok(row
            .filter(|(_, document, _, _)| {
                *document
                    || crate::file_media_path(uri).is_some()
                    || crate::cue_media_parts(uri).is_some()
                    || crate::document_media_id(uri).is_some()
            })
            .map(|(revision, _, path, format)| FileTrack {
                revision: serde_json::Value::from(revision).to_string(),
                backing: path
                    .as_deref()
                    .and_then(|path| {
                        crate::document_locator_parts(path)
                            .map(|(id, _)| id)
                            .or_else(|| crate::document_media_id(path))
                    })
                    .map(|id| crate::document_media_uri(&id)),
                format,
            }))
    }

    pub async fn download_file(&self, uri: &str) -> LibraryResult<Option<DownloadFile>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT * FROM downloads WHERE media_uri=?1")
            .bind(uri)
            .fetch_optional(&mut *reader)
            .await?)
    }

    /// A copy another track of the same whole file already downloaded, such as another track
    /// of one CUE sheet.
    pub async fn shared_download_file(
        &self,
        uri: &str,
        revision: &str,
    ) -> LibraryResult<Option<DownloadFile>> {
        let backing = crate::cue_media_parts(uri)
            .map(|(_, backing, _, _)| backing)
            .unwrap_or_else(|| uri.to_owned());
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT * FROM downloads WHERE revision=?2 AND (media_uri=?1 OR media_uri GLOB ?3) ORDER BY managed DESC LIMIT 1")
            .bind(&backing).bind(revision).bind(crate::keys::cue_file_glob(&backing))
            .fetch_optional(&mut *reader).await?)
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
