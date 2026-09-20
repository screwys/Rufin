//! Device-local representations of shared collection entries.
use crate::{Database, LibraryError, LibraryResult};

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct ConnectMediaFile {
    pub media_uri: String,
    pub encoding: String,
    pub revision: String,
    pub path: String,
    pub managed: bool,
    pub hash: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn direct_continuation_access_stays_local_and_removal_preserves_original() {
        let root = tempfile::tempdir().unwrap();
        let database = Database::open(root.path().join("library.sqlite"))
            .await
            .unwrap();
        database.connect_capture_enabled(true).await.unwrap();
        let original = root.path().join("original.flac");
        std::fs::write(&original, b"original").unwrap();
        let uri = url::Url::from_file_path(&original).unwrap().to_string();
        let item = crate::QueueItem::direct(&uri, "Track", "Artist", "Album", 180_000);
        let copy = root.path().join("download.mp3");
        std::fs::write(&copy, b"download").unwrap();
        database.connect_set_queue_file(&item, &copy).await.unwrap();
        let receipt = ConnectMediaFile {
            media_uri: uri.clone(),
            encoding: "mp3".into(),
            revision: "1".into(),
            path: url::Url::from_file_path(&copy).unwrap().to_string(),
            managed: true,
            hash: None,
        };
        database.connect_save_media_file(&receipt).await.unwrap();
        assert_eq!(
            database.playback_access(&uri).await.unwrap().unwrap().0,
            receipt.path
        );
        assert!(
            database
                .connect_track_reference(&uri)
                .await
                .unwrap()
                .is_none()
        );
        database.connect_forget_media_file(&receipt).await.unwrap();
        assert!(database.playback_access(&uri).await.unwrap().is_none());
        assert_eq!(std::fs::read(original).unwrap(), b"original");
    }
}

impl Database {
    pub async fn connect_local_media_page(&self, after: &str) -> LibraryResult<Vec<String>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT media_uri FROM catalog.tracks WHERE media_uri>?1 AND media_uri>='file:' AND media_uri<'file;' UNION SELECT media_uri FROM catalog.tracks WHERE media_uri>?1 AND media_uri>='rufin:cue/' AND media_uri<'rufin:cue0' ORDER BY media_uri LIMIT ?2")
            .bind(after).bind(crate::CONNECT_PAGE_SIZE as i64)
            .fetch_all(&mut *reader).await?)
    }

    pub async fn connect_received_occurrence(
        &self,
        id: &crate::OccurrenceId,
    ) -> LibraryResult<bool> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT received_from_connect FROM queue_occurrences WHERE object_id=?1",
        )
        .bind(id.as_str())
        .fetch_optional(&mut *reader)
        .await?
        .unwrap_or(false))
    }
    /// A transferred direct queue file has local access without catalog enrollment.
    pub async fn connect_set_queue_file(
        &self,
        item: &crate::QueueItem,
        path: &std::path::Path,
    ) -> LibraryResult<()> {
        let metadata = std::fs::metadata(path)?;
        let access_uri = url::Url::from_file_path(path)
            .map_err(|()| LibraryError::InvalidRequest("Media path must be absolute".into()))?
            .to_string();
        self.upsert_local_access(
            None,
            &crate::LocalAccessWrite {
                media_uri: item.media_uri.clone(),
                origin: crate::LocalAccessOrigin::Download,
                path: path.to_string_lossy().into_owned(),
                root: path.parent().unwrap_or(path).to_string_lossy().into_owned(),
                relative_path: String::new(),
                size_bytes: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                mtime_ns: 0,
                device_id: None,
                inode: None,
                parser_version: 1,
                title: item.title.clone(),
                album: item.album.clone(),
                artist: item.artist.clone(),
                disc_number: item.disc_number.unwrap_or(1),
                track_number: item.track_number.unwrap_or(1),
                duration_millis: item.duration_millis,
                access_uri,
                loudness_analysis_key: None,
            },
        )
        .await
        .map(|_| ())
    }

    pub async fn connect_original_file(
        &self,
        uri: &str,
    ) -> LibraryResult<Option<std::path::PathBuf>> {
        let mut reader = self.acquire_reader().await?;
        let access: Option<String> = sqlx::query_scalar("SELECT access_uri FROM local_locators WHERE media_uri=?1 AND origin IN ('local','import') LIMIT 1")
            .bind(uri).fetch_optional(&mut *reader).await?;
        if let Some(path) = access
            .and_then(|uri| crate::file_media_path(&uri))
            .filter(|path| path.is_file())
        {
            return Ok(Some(path));
        }
        let path: Option<String> = sqlx::query_scalar("SELECT f.path FROM catalog.tracks t JOIN catalog.local_files f ON f.source_key=t.source_key AND f.path=t.source_path WHERE t.media_uri=?1 LIMIT 1")
            .bind(uri).fetch_optional(&mut *reader).await?;
        Ok(path
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_file()))
    }

    pub async fn connect_media_files(&self, uri: &str) -> LibraryResult<Vec<ConnectMediaFile>> {
        let mut reader = self.acquire_reader().await?;
        Ok(
            sqlx::query_as("SELECT * FROM connect_media_files WHERE media_uri=?1")
                .bind(uri)
                .fetch_all(&mut *reader)
                .await?,
        )
    }

    pub async fn connect_forget_media_file(&self, file: &ConnectMediaFile) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let writer = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query(
            "DELETE FROM local_locators WHERE media_uri=?1 AND access_uri=?2 AND origin='download' AND NOT EXISTS(SELECT 1 FROM connect_media_files WHERE media_uri=?1 AND path=?2 AND encoding<>?3)",
        )
        .bind(&file.media_uri)
        .bind(&file.path)
        .bind(&file.encoding)
        .execute(&mut *writer)
        .await?;
        sqlx::query(
            "DELETE FROM connect_media_files WHERE media_uri=?1 AND encoding=?2 AND path=?3",
        )
        .bind(&file.media_uri)
        .bind(&file.encoding)
        .bind(&file.path)
        .execute(writer)
        .await?;
        Ok(())
    }

    pub async fn connect_media_file(
        &self,
        uri: &str,
        encoding: &str,
    ) -> LibraryResult<Option<ConnectMediaFile>> {
        let mut reader = self.acquire_reader().await?;
        Ok(
            sqlx::query_as("SELECT * FROM connect_media_files WHERE media_uri=?1 AND encoding=?2")
                .bind(uri)
                .bind(encoding)
                .fetch_optional(&mut *reader)
                .await?,
        )
    }

    pub async fn connect_save_media_file(&self, file: &ConnectMediaFile) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        sqlx::query("INSERT INTO connect_media_files(media_uri,encoding,revision,path,managed,hash) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(media_uri,encoding) DO UPDATE SET revision=excluded.revision,path=excluded.path,managed=excluded.managed,hash=excluded.hash")
            .bind(&file.media_uri).bind(&file.encoding).bind(&file.revision).bind(&file.path).bind(file.managed).bind(&file.hash)
            .execute(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?).await?;
        Ok(())
    }
}
