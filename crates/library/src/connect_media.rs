//! Device-local representations of shared collection entries.
use crate::{Database, LibraryError, LibraryResult, LocalMediaLocation};

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
    async fn cue_receipts_migrate_the_backing_index() {
        use sqlx::Connection;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("old.sqlite");
        let mut connection = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::query("CREATE TABLE connect_media_files(media_uri TEXT NOT NULL,encoding TEXT NOT NULL,revision TEXT NOT NULL,path TEXT NOT NULL,managed INTEGER NOT NULL,hash TEXT,PRIMARY KEY(media_uri,encoding)) STRICT").execute(&mut connection).await.unwrap();
        let backing = url::Url::from_file_path(root.path().join("album.flac"))
            .unwrap()
            .to_string();
        for index in 0..130 {
            let uri = crate::cue_media_uri(
                &index.to_string(),
                &backing,
                index * 1000,
                (index + 1) * 1000,
            );
            sqlx::query("INSERT INTO connect_media_files VALUES(?1,'original','v1',?2,1,'blob')")
                .bind(uri)
                .bind(&backing)
                .execute(&mut connection)
                .await
                .unwrap();
        }
        drop(connection);
        let database = Database::open(&path).await.unwrap();
        let mut reader = database.acquire_reader().await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM connect_media_files WHERE backing_uri=?1")
                .bind(&backing)
                .fetch_one(&mut *reader)
                .await
                .unwrap();
        assert_eq!(count, 130);
        drop(reader);
        let uri = crate::cue_media_uri("new-track", &backing, 130_000, 131_000);
        let shared = database
            .connect_backing_media_file(&uri, "original", "v1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(shared.path, backing);
        assert_eq!(
            database.connect_media_file_users(&shared).await.unwrap(),
            (true, true)
        );
    }

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
        Ok(sqlx::query_scalar("SELECT media_uri FROM catalog.tracks t WHERE media_uri>?1 AND ((media_uri>='file:' AND media_uri<'file;') OR (media_uri>='rufin:cue/' AND media_uri<'rufin:cue0') OR (media_uri>='rufin:document/' AND media_uri<'rufin:document0') OR EXISTS(SELECT 1 FROM catalog.local_files f WHERE f.source_key=t.source_key AND f.path=t.source_path AND f.path LIKE 'rufin-document:%') OR EXISTS(SELECT 1 FROM main.local_locators l WHERE l.media_uri=t.media_uri AND l.origin IN ('local','import') AND l.access_uri LIKE 'content://%') OR EXISTS(SELECT 1 FROM connect_collection c WHERE c.media_uri=t.media_uri AND json_extract(c.payload,'$.local_media')=1)) ORDER BY media_uri LIMIT ?2")
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
        self.connect_set_queue_access(
            item,
            path.to_string_lossy().into_owned(),
            path.parent().unwrap_or(path).to_string_lossy().into_owned(),
            access_uri,
            i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        )
        .await
    }

    pub async fn connect_set_queue_document(
        &self,
        item: &crate::QueueItem,
        uri: &str,
        size: Option<u64>,
    ) -> LibraryResult<()> {
        self.connect_set_queue_access(
            item,
            uri.into(),
            uri.into(),
            uri.into(),
            size.and_then(|size| i64::try_from(size).ok()).unwrap_or(0),
        )
        .await
    }

    async fn connect_set_queue_access(
        &self,
        item: &crate::QueueItem,
        path: String,
        root: String,
        access_uri: String,
        size_bytes: i64,
    ) -> LibraryResult<()> {
        self.upsert_local_access(
            None,
            &crate::LocalAccessWrite {
                media_uri: item.media_uri.clone(),
                origin: crate::LocalAccessOrigin::Download,
                path,
                root,
                relative_path: String::new(),
                size_bytes,
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
        let native = crate::file_media_path(backing.as_deref().unwrap_or(uri)).is_some();
        // Native scans set source_path; Connect never copies it from another device.
        // Playlist imports keep that path without configuring a folder.
        let path: Option<String> = sqlx::query_scalar(
            "SELECT source_path FROM catalog.tracks t WHERE media_uri=?1 AND source_path IS NOT NULL
             AND (?2 OR EXISTS(SELECT 1 FROM catalog.local_files f WHERE f.source_key=t.source_key AND f.path=t.source_path))",
        )
        .bind(uri)
        .bind(native)
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
            "DELETE FROM local_locators WHERE media_uri=?1 AND access_uri=?2 AND origin='download' AND NOT EXISTS(SELECT 1 FROM connect_media_files WHERE media_uri=?1 AND path=?2 AND (encoding,revision)<>(?3,?4))",
        )
        .bind(&file.media_uri)
        .bind(&file.path)
        .bind(&file.encoding)
        .bind(&file.revision)
        .execute(&mut *writer)
        .await?;
        sqlx::query(
            "DELETE FROM connect_media_files WHERE media_uri=?1 AND encoding=?2 AND path=?3 AND revision=?4",
        )
        .bind(&file.media_uri)
        .bind(&file.encoding)
        .bind(&file.path)
        .bind(&file.revision)
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

    pub async fn connect_media_file_at_path(
        &self,
        path: &str,
        encoding: &str,
        revision: &str,
    ) -> LibraryResult<Option<ConnectMediaFile>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT * FROM connect_media_files WHERE path=?1 AND encoding=?2 ORDER BY revision=?3 DESC,managed DESC LIMIT 1")
            .bind(path).bind(encoding).bind(revision)
            .fetch_optional(&mut *reader).await?)
    }

    pub async fn connect_backing_media_file(
        &self,
        uri: &str,
        encoding: &str,
        revision: &str,
    ) -> LibraryResult<Option<ConnectMediaFile>> {
        let backing = self.connect_backing_id(uri).await?;
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT * FROM connect_media_files WHERE backing_uri=?1 AND encoding=?2 AND revision=?3 ORDER BY managed DESC LIMIT 1")
            .bind(backing).bind(encoding).bind(revision)
            .fetch_optional(&mut *reader).await?)
    }

    /// Other tracks can use the same backing file or transferred blob.
    pub async fn connect_media_file_users(
        &self,
        file: &ConnectMediaFile,
    ) -> LibraryResult<(bool, bool)> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT EXISTS(SELECT 1 FROM connect_media_files WHERE path=?1 AND (media_uri,encoding,revision,path)<>(?2,?3,?5,?1)), EXISTS(SELECT 1 FROM connect_media_files WHERE hash=?4 AND (media_uri,encoding,revision,path)<>(?2,?3,?5,?1))")
            .bind(&file.path).bind(&file.media_uri).bind(&file.encoding).bind(&file.hash).bind(&file.revision)
            .fetch_one(&mut *reader).await?)
    }

    pub async fn connect_save_media_file(&self, file: &ConnectMediaFile) -> LibraryResult<()> {
        let backing = self.connect_backing_id(&file.media_uri).await?;
        let mut writer = self.writer().await?;
        sqlx::query("INSERT INTO connect_media_files(media_uri,encoding,revision,path,managed,hash,backing_uri) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(media_uri,encoding) DO UPDATE SET revision=excluded.revision,path=excluded.path,managed=excluded.managed,hash=excluded.hash,backing_uri=excluded.backing_uri")
            .bind(&file.media_uri).bind(&file.encoding).bind(&file.revision).bind(&file.path).bind(file.managed).bind(&file.hash)
            .bind(backing)
            .execute(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?).await?;
        Ok(())
    }

    pub async fn connect_backing_id(&self, uri: &str) -> LibraryResult<String> {
        let reference = self.connect_track_reference(uri).await?;
        Ok(reference
            .as_ref()
            .and_then(|value| value["backing_id"].as_str())
            .map(str::to_owned)
            .or_else(|| crate::cue_media_parts(uri).map(|(_, backing, _, _)| backing))
            .unwrap_or_else(|| uri.to_string()))
    }

    pub async fn connect_document_backing_used(
        &self,
        uri: &str,
        encoding: &str,
        revision: &str,
    ) -> LibraryResult<bool> {
        let backing = self.connect_backing_id(uri).await?;
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM connect_media_files WHERE backing_uri=?1 AND encoding=?2 AND revision=?3 AND path LIKE 'content://%')")
            .bind(backing).bind(encoding).bind(revision).fetch_one(&mut *reader).await?)
    }

    pub async fn connect_media_window(&self, uri: &str) -> LibraryResult<Option<(u64, u64)>> {
        if let Some((_, _, start, end)) = crate::cue_media_parts(uri) {
            return Ok(Some((start as u64, end as u64)));
        }
        let reference = self.connect_track_reference(uri).await?;
        Ok(reference.as_ref().and_then(|value| {
            let start = value["cue_start_millis"].as_u64()?;
            let end = value["cue_end_millis"].as_u64()?;
            (end > start).then_some((start, end))
        }))
    }
}
