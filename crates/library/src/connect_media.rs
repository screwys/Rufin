//! Device-local representations of shared collection entries.
use crate::{Database, LibraryError, LibraryResult, LocalFileWrite, LocalMediaLocation};
use serde::{Deserialize, Serialize};
use sqlx::Connection;

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

#[derive(Clone, Debug, Default, sqlx::FromRow)]
pub struct ConnectMediaFile {
    pub media_uri: String,
    pub encoding: String,
    pub revision: String,
    pub path: String,
    pub managed: bool,
    pub source_id: Option<String>,
    pub owners: String,
    pub storage_root: String,
    pub relative_path: String,
    pub completed_size: Option<i64>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cue_receipts_share_the_backing_index_after_reopen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("library.sqlite");
        let database = Database::open(&path).await.unwrap();
        let backing = url::Url::from_file_path(root.path().join("album.flac"))
            .unwrap()
            .to_string();
        std::fs::write(root.path().join("album.flac"), b"original").unwrap();
        let source_id = crate::SourceId::new("local");
        database
            .reconcile_source(&source_id, "local", "Local")
            .await
            .unwrap();
        for index in 0..130 {
            let uri = crate::cue_media_uri(
                &index.to_string(),
                &backing,
                index * 1000,
                (index + 1) * 1000,
            );
            if index == 0 {
                database
                    .catalog(source_id.as_str(), None)
                    .await
                    .unwrap()
                    .apply_catalog(
                        &crate::CatalogBatch {
                            tracks: vec![crate::TrackObservation {
                                object_id: "track".into(),
                                media_uri: Some(uri.clone()),
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                        true,
                    )
                    .await
                    .unwrap();
                database
                    .write_local_access(
                        None,
                        &[&crate::LocalAccessWrite {
                            media_uri: uri.clone(),
                            origin: crate::LocalAccessOrigin::Download,
                            path: root
                                .path()
                                .join("album.flac")
                                .to_string_lossy()
                                .into_owned(),
                            root: root.path().to_string_lossy().into_owned(),
                            relative_path: "album.flac".into(),
                            access_uri: backing.clone(),
                            size_bytes: 8,
                            mtime_ns: 0,
                            device_id: None,
                            inode: None,
                            parser_version: 1,
                            loudness_analysis_key: None,
                        }],
                    )
                    .await
                    .unwrap();
            }
            database
                .connect_save_media_file(&ConnectMediaFile {
                    media_uri: uri,
                    encoding: "original".into(),
                    revision: "v1".into(),
                    path: backing.clone(),
                    managed: true,
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        assert_eq!(database.downloaded_count(None).await.unwrap(), 1);
        database.close().await.unwrap();
        let database = Database::open(&path).await.unwrap();
        assert_eq!(
            database
                .downloaded_count(Some(source_id.as_str()))
                .await
                .unwrap(),
            1
        );
        assert_eq!(database.downloaded_count(None).await.unwrap(), 0);
        let records = database
            .download_files_page(
                Some(source_id.as_str()),
                &(String::new(), String::new()),
                None,
            )
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, backing);
        assert_eq!(
            std::fs::read(root.path().join("album.flac")).unwrap(),
            b"original"
        );
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
        assert!(database.connect_media_file_users(&shared).await.unwrap());
    }

    #[tokio::test]
    async fn download_receipt_removal_preserves_original_and_borrowed_access() {
        let root = tempfile::tempdir().unwrap();
        let database = Database::open(root.path().join("library.sqlite"))
            .await
            .unwrap();
        let original = root.path().join("original.flac");
        std::fs::write(&original, b"original").unwrap();
        let uri = url::Url::from_file_path(&original).unwrap().to_string();
        let item = crate::TrackReference::direct(&uri, "Track", "Artist", "Album", 180_000);
        database
            .write_direct_track_metadata(&uri, &item.metadata)
            .await
            .unwrap();
        let copy = root.path().join("download.mp3");
        std::fs::write(&copy, b"download").unwrap();
        let mut access = crate::LocalAccessWrite {
            media_uri: item.media_uri.clone(),
            origin: crate::LocalAccessOrigin::Download,
            path: copy.to_string_lossy().into_owned(),
            root: root.path().to_string_lossy().into_owned(),
            relative_path: "download.mp3".into(),
            size_bytes: 8,
            mtime_ns: 0,
            device_id: None,
            inode: None,
            parser_version: 1,
            access_uri: url::Url::from_file_path(&copy).unwrap().to_string(),
            loudness_analysis_key: None,
        };
        database.write_local_access(None, &[&access]).await.unwrap();
        assert!(
            database
                .connect_original_file(&uri, |path| path)
                .await
                .unwrap()
                .is_none()
        );
        let receipt = ConnectMediaFile {
            media_uri: uri.clone(),
            encoding: "mp3".into(),
            revision: "1".into(),
            path: url::Url::from_file_path(&copy).unwrap().to_string(),
            managed: true,
            ..Default::default()
        };
        database.connect_save_media_file(&receipt).await.unwrap();
        assert_eq!(
            database.playback_access(&uri).await.unwrap().unwrap().0,
            receipt.path
        );
        assert_eq!(
            match crate::CatalogQuery::uris(crate::CatalogKind::Track, vec![uri.clone()])
                .rows(&database, 0, 1, &crate::ReadCancellation::new())
                .await
                .unwrap()
            {
                crate::CatalogRows::Tracks(rows) => rows[0].metadata.title.clone(),
                _ => unreachable!("song query"),
            },
            "Track"
        );
        assert!(
            database
                .connect_track_reference(&uri)
                .await
                .unwrap()
                .is_none()
        );
        let alternative = root.path().join("download.opus");
        std::fs::write(&alternative, b"alternative").unwrap();
        let alternate = ConnectMediaFile {
            encoding: media_encoding_key(Some(crate::AudioEncoding {
                codec: crate::AudioCodec::Opus,
                ..crate::AudioEncoding::mp3()
            })),
            path: url::Url::from_file_path(&alternative).unwrap().to_string(),
            completed_size: Some(11),
            storage_root: root.path().to_string_lossy().into_owned(),
            relative_path: "download.opus".into(),
            ..receipt.clone()
        };
        let mut alternate_access = access.clone();
        alternate_access.path = alternative.to_string_lossy().into_owned();
        alternate_access.access_uri = alternate.path.clone();
        alternate_access.relative_path = "download.opus".into();
        alternate_access.size_bytes = 11;
        database
            .commit_download(&alternate, None, &alternate_access)
            .await
            .unwrap();
        database
            .connect_forget_media_file(&alternate)
            .await
            .unwrap();
        assert_eq!(
            database.playback_access(&uri).await.unwrap().unwrap().0,
            receipt.path
        );
        database.connect_forget_media_file(&receipt).await.unwrap();
        assert!(database.playback_access(&uri).await.unwrap().is_none());
        assert_eq!(
            match crate::CatalogQuery::uris(crate::CatalogKind::Track, vec![uri.clone()])
                .rows(&database, 0, 1, &crate::ReadCancellation::new())
                .await
                .unwrap()
            {
                crate::CatalogRows::Tracks(rows) => rows[0].metadata.title.clone(),
                _ => unreachable!("song query"),
            },
            "Track"
        );
        let mut borrowed = receipt.clone();
        borrowed.path = uri.clone();
        borrowed.managed = false;
        access.origin = crate::LocalAccessOrigin::Mapping;
        access.path = original.to_string_lossy().into_owned();
        access.relative_path = "original.flac".into();
        access.access_uri = uri.clone();
        database.write_local_access(None, &[&access]).await.unwrap();
        assert!(matches!(
            database.connect_original_file(&uri, |path| path).await.unwrap(),
            Some(LocalMediaLocation::File(path)) if path == original
        ));
        let cue = crate::cue_media_uri("mapped", &uri, 0, 1_000);
        assert!(matches!(
            database.connect_original_file(&cue, |path| path).await.unwrap(),
            Some(LocalMediaLocation::File(path)) if path == original
        ));
        database.connect_save_media_file(&borrowed).await.unwrap();
        database.connect_forget_media_file(&borrowed).await.unwrap();
        assert_eq!(
            database.playback_access(&uri).await.unwrap().unwrap().0,
            uri
        );
        assert_eq!(std::fs::read(original).unwrap(), b"original");
        access.path = "content://provider/document/original.flac".into();
        access.access_uri = access.path.clone();
        access.root = "content://provider/tree/music".into();
        database.write_local_access(None, &[&access]).await.unwrap();
        assert!(matches!(
            database.connect_original_file(&uri, |path| path).await.unwrap(),
            Some(LocalMediaLocation::Document(document)) if document == access.access_uri
        ));
    }
}

impl Database {
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
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let removed = sqlx::query("DELETE FROM connect_media_files WHERE media_uri=?1 AND encoding=?2 AND path=?3 AND revision=?4")
            .bind(&file.media_uri).bind(&file.encoding).bind(&file.path).bind(&file.revision).execute(&mut *transaction).await?.rows_affected()>0;
        if removed {
            let access = sqlx::query("DELETE FROM local_locators WHERE media_uri=?1 AND access_uri=?2 AND origin='download' AND NOT EXISTS(SELECT 1 FROM connect_media_files WHERE media_uri=?1 AND path=?2)")
                .bind(&file.media_uri).bind(&file.path).execute(&mut *transaction).await?.rows_affected()>0;
            if access && let Some(next) = sqlx::query_as::<_, ConnectMediaFile>("SELECT * FROM connect_media_files WHERE media_uri=?1 ORDER BY rowid DESC LIMIT 1")
                .bind(&file.media_uri).fetch_optional(&mut *transaction).await? {
                let path = crate::file_media_path(&next.path);
                let root = if next.storage_root.is_empty() { path.as_deref().and_then(std::path::Path::parent).map(|path|path.to_string_lossy().into_owned()).unwrap_or_else(|| next.path.clone()) } else { next.storage_root.clone() };
                crate::local::write_local_locator(&mut transaction, &crate::LocalLocatorWrite {
                    source_id: next.source_id.clone(), media_uri: next.media_uri.clone(), origin: "download".into(),
                    path: path.map(|path|path.to_string_lossy().into_owned()).unwrap_or_else(||next.path.clone()), root, relative_path: next.relative_path.clone(), access_uri: next.path.clone(),
                }).await?;
                sqlx::query("INSERT INTO local_access_metadata SELECT ?1,coalesce(?2,size_bytes),0,NULL,NULL,parser_version,loudness_analysis_key FROM local_access_metadata WHERE access_uri=?3 ON CONFLICT(access_uri) DO NOTHING")
                    .bind(&next.path).bind(next.completed_size).bind(&file.path).execute(&mut *transaction).await?;
            }
        }
        transaction.commit().await?;
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

    /// Other tracks can use the same backing file.
    pub async fn connect_media_file_users(&self, file: &ConnectMediaFile) -> LibraryResult<bool> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM connect_media_files WHERE path=?1 AND (media_uri,encoding,revision)<>(?2,?3,?4))")
            .bind(&file.path).bind(&file.media_uri).bind(&file.encoding).bind(&file.revision)
            .fetch_one(&mut *reader).await?)
    }

    pub async fn connect_save_media_file(&self, file: &ConnectMediaFile) -> LibraryResult<()> {
        let backing = self.connect_backing_id(&file.media_uri).await?;
        let mut writer = self.writer().await?;
        save_download_on(
            writer.as_mut().ok_or(LibraryError::WriterUnavailable)?,
            file,
            &backing,
        )
        .await
    }

    pub async fn commit_download(
        &self,
        file: &ConnectMediaFile,
        source: Option<crate::SourceKey>,
        access: &crate::LocalAccessWrite,
    ) -> LibraryResult<()> {
        let backing = self.connect_backing_id(&file.media_uri).await?;
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        crate::local::write_local_access_on(&mut transaction, source, &[access]).await?;
        save_download_on(&mut transaction, file, &backing).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn download_files_page(
        &self,
        source: Option<&str>,
        after: &(String, String),
        owner: Option<&str>,
    ) -> LibraryResult<Vec<ConnectMediaFile>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT * FROM connect_media_files WHERE source_id IS ?1 AND (media_uri,encoding)>(?2,?3) AND (?4 IS NULL OR EXISTS(SELECT 1 FROM json_each(owners) WHERE value=json_extract(?4,'$'))) ORDER BY media_uri,encoding LIMIT 128")
            .bind(source).bind(&after.0).bind(&after.1).bind(owner).fetch_all(&mut *reader).await?)
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
}

async fn save_download_on(
    connection: &mut sqlx::SqliteConnection,
    file: &ConnectMediaFile,
    backing: &str,
) -> LibraryResult<()> {
    sqlx::query("INSERT INTO connect_media_files(media_uri,encoding,revision,path,managed,backing_uri,source_id,owners,storage_root,relative_path,completed_size,retired_documents) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) ON CONFLICT(media_uri,encoding) DO UPDATE SET revision=excluded.revision,path=excluded.path,managed=excluded.managed,backing_uri=excluded.backing_uri,source_id=coalesce(excluded.source_id,connect_media_files.source_id),owners=excluded.owners,storage_root=excluded.storage_root,relative_path=excluded.relative_path,completed_size=excluded.completed_size,retired_documents=excluded.retired_documents")
        .bind(&file.media_uri).bind(&file.encoding).bind(&file.revision).bind(&file.path).bind(file.managed).bind(backing).bind(&file.source_id)
        .bind(if file.owners.is_empty() { "[]" } else { &file.owners }).bind(&file.storage_root).bind(&file.relative_path).bind(file.completed_size)
        .bind(if file.retired_documents.is_empty() { "[]" } else { &file.retired_documents }).execute(connection).await?;
    Ok(())
}
