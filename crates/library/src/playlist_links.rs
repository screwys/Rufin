//! Durable file associations, independent of playlist ownership and entry identities.
use crate::{Catalog, LibraryError, LibraryResult, PlaylistKey, PlaylistPathMode, SourceId};
use sqlx::{Row, SqliteConnection};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistFileLink {
    pub playlist: PlaylistKey,
    pub source_id: Option<SourceId>,
    pub path: String,
    pub auto_refresh: bool,
    pub auto_save: Option<bool>,
    pub path_mode: PlaylistPathMode,
    pub revision: Option<String>,
    pub dirty: bool,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! check {
        ($condition:expr $(,)?) => {
            if !$condition {
                return Err(LibraryError::InvalidRequest(format!(
                    "test assertion failed: {}",
                    stringify!($condition)
                )));
            }
        };
    }

    macro_rules! check_eq {
        ($left:expr, $right:expr $(,)?) => {{
            let left = &$left;
            let right = &$right;
            if left != right {
                return Err(LibraryError::InvalidRequest(format!(
                    "test assertion failed: {} == {} (left: {left:?}, right: {right:?})",
                    stringify!($left),
                    stringify!($right),
                )));
            }
        }};
    }

    #[tokio::test]
    async fn removing_earlier_roots_keeps_links_on_their_original_files() -> LibraryResult<()> {
        let directory = tempfile::tempdir()?;
        let catalog = Catalog::open(directory.path().join("store.sqlite")).await?;
        let root_a = directory.path().join("A");
        let root_b = directory.path().join("B");
        let root_c = directory.path().join("C");
        for root in [&root_a, &root_b, &root_c] {
            std::fs::create_dir(root)?;
            std::fs::write(root.join("music.m3u8"), "#EXTM3U\n")?;
        }
        let source = SourceId::new("local");
        catalog
            .set_playlist_file_auto_save(Some(&source), true)
            .await?;
        let mut keys = Vec::new();
        // Deliberately create higher ordinals first to exercise unique-path compaction.
        for index in [2, 1, 0] {
            let playlist = catalog
                .create_playlist(
                    &format!("Root {index}"),
                    &crate::MemberSelection::Uris(((&[]).to_vec()).into()),
                )
                .await?
                .unwrap()
                .0;
            catalog
                .save_playlist_file_link(&PlaylistFileLink {
                    playlist,
                    source_id: Some(source.clone()),
                    path: format!("@{index}/music.m3u8"),
                    auto_refresh: true,
                    auto_save: None,
                    path_mode: PlaylistPathMode::Relative,
                    revision: Some("saved".into()),
                    dirty: false,
                    error: None,
                })
                .await?;
            keys.push(playlist);
            catalog
                .ignore_playlist_file(Some(&source), &format!("@{index}/ignored.pls"))
                .await?;
        }
        catalog
            .remove_playlist_file_roots(&source, &[(0, root_a.clone())])
            .await?;
        let a = catalog.get_playlist_file_link(keys[2]).await?.unwrap();
        check_eq!(a.source_id, None);
        check_eq!(std::path::PathBuf::from(&a.path), root_a.join("music.m3u8"));
        check_eq!(a.auto_save, Some(true));
        let b = catalog.get_playlist_file_link(keys[1]).await?.unwrap();
        check_eq!(b.path, "@0/music.m3u8");
        check_eq!(b.source_id, Some(source.clone()));
        check_eq!(b.auto_save, None);
        check_eq!(
            catalog.get_playlist_file_link(keys[0]).await?.unwrap().path,
            "@1/music.m3u8"
        );
        check!(
            catalog
                .is_playlist_file_ignored(None, &root_a.join("ignored.pls").to_string_lossy())
                .await?
        );
        check!(
            catalog
                .is_playlist_file_ignored(Some(&source), "@0/ignored.pls")
                .await?
        );
        check!(
            catalog
                .is_playlist_file_ignored(Some(&source), "@1/ignored.pls")
                .await?
        );
        std::fs::write(&a.path, "#EXTM3U\nremoved-root-song.flac\n")?;
        check_eq!(
            std::fs::read_to_string(root_b.join("music.m3u8"))?,
            "#EXTM3U\n"
        );
        catalog.close().await
    }

    #[tokio::test]
    async fn rejected_files_are_scoped_to_their_source_and_observed_playlists_are_paged()
    -> LibraryResult<()> {
        let directory = tempfile::tempdir()?;
        let catalog = Catalog::open(directory.path().join("store.sqlite")).await?;
        {
            let mut writer = catalog.writer().await?;
            sqlx::query(
                "INSERT INTO sources(source_key,object_id,display_name) VALUES (1,'files','Files')",
            )
            .execute(&mut *writer)
            .await?;
        }
        let files = [
            (
                "album/photo.png",
                "photo.png",
                crate::LocalFileKind::Image,
                crate::LocalFileState::Rejected,
            ),
            (
                "album/Music.M3U8",
                "Music.M3U8",
                crate::LocalFileKind::Media,
                crate::LocalFileState::Rejected,
            ),
            (
                "album/other.pls",
                "other.pls",
                crate::LocalFileKind::Media,
                crate::LocalFileState::Observed,
            ),
            (
                "album/folder.xspf",
                "folder.xspf",
                crate::LocalFileKind::Directory,
                crate::LocalFileState::Observed,
            ),
        ]
        .into_iter()
        .map(|(path, relative, kind, state)| {
            (
                crate::LocalFileWrite {
                    path: path.into(),
                    root: "album".into(),
                    relative_path: relative.into(),
                    kind,
                    size_bytes: None,
                    mtime_ns: 1,
                    device_id: None,
                    inode: None,
                    native_id: None,
                    picture_index: None,
                    artist_pictures: None,
                    revision: None,
                    parse_version: Some(1),
                    state,
                },
                Vec::new(),
            )
        })
        .collect::<Vec<_>>();
        let mut observation = catalog.observe("files", None).await?;
        observation
            .write_file_observations(&files, Vec::new(), true)
            .await?;
        drop(observation);
        let source = SourceId::new("files");
        check!(
            catalog
                .file_path_is_rejected(&source, "album/photo.png")
                .await?
        );
        check!(
            catalog
                .file_path_is_rejected(&source, "album/Music.M3U8")
                .await?
        );
        check!(
            !catalog
                .file_path_is_rejected(&source, "album/missing.flac")
                .await?
        );
        check!(
            !catalog
                .file_path_is_rejected(&SourceId::new("other"), "album/photo.png")
                .await?
        );
        let rows = catalog
            .observed_playlist_file_page(crate::SourceKey::from_raw(1), None)
            .await?;
        check_eq!(rows.len(), 2);
        let next = catalog
            .observed_playlist_file_page(
                crate::SourceKey::from_raw(1),
                Some(rows[0].local_file_key),
            )
            .await?;
        check_eq!(next.len(), 1);
        check_eq!(next[0].path, "album/other.pls");
        let playlist = catalog
            .create_playlist("Shared", &crate::MemberSelection::Uris((Vec::new()).into()))
            .await?
            .unwrap()
            .0;
        catalog
            .associate_playlist_files(&source, &[(rows[0].path.clone(), playlist)])
            .await?;
        catalog
            .observe("files", None)
            .await?
            .write_file_observations(&files, Vec::new(), true)
            .await?;
        let associated = catalog
            .observed_playlist_file_page(crate::SourceKey::from_raw(1), None)
            .await?;
        check_eq!(associated[0].playlist, Some(playlist));
        check!(associated[0].playlist_identity.is_some());
        catalog.delete_playlist(playlist).await?;
        let deleted = catalog
            .observed_playlist_file_page(crate::SourceKey::from_raw(1), None)
            .await?;
        check_eq!(deleted[0].playlist, None);
        check!(deleted[0].playlist_identity.is_some());
        catalog.close().await
    }

    #[tokio::test]
    async fn associations_survive_restart() -> LibraryResult<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("library.sqlite");
        let catalog = Catalog::open(&path).await?;
        let playlist = catalog
            .create_playlist(
                "Music",
                &crate::MemberSelection::Uris(
                    ((&["file:///song.flac".into(), "file:///two.flac".into()]).to_vec()).into(),
                ),
            )
            .await?
            .unwrap()
            .0;
        let source = SourceId::new("local");
        let link = PlaylistFileLink {
            playlist,
            source_id: Some(source.clone()),
            path: "0/favorites.m3u8".into(),
            auto_refresh: true,
            auto_save: None,
            path_mode: PlaylistPathMode::Automatic,
            revision: Some("first".into()),
            dirty: false,
            error: None,
        };
        check!(catalog.save_playlist_file_link(&link).await?);
        check!(!catalog.save_playlist_file_link(&link).await?);
        let second = catalog
            .create_playlist(
                "Second",
                &crate::MemberSelection::Uris(((&[]).to_vec()).into()),
            )
            .await?
            .unwrap()
            .0;
        let mut duplicate = link.clone();
        duplicate.playlist = second;
        check!(catalog.save_playlist_file_link(&duplicate).await.is_err());
        check!(!catalog.playlist_file_auto_save(Some(&source)).await?);
        catalog
            .set_playlist_file_auto_save(Some(&source), true)
            .await?;
        catalog
            .ignore_playlist_file(Some(&source), "0/ignored.pls")
            .await?;
        catalog.mark_playlist_file_synced(playlist, "third").await?;
        catalog.close().await?;
        let catalog = Catalog::open(&path).await?;
        let restored = catalog
            .find_playlist_file_link(Some(&source), &link.path)
            .await?
            .unwrap();
        check_eq!(restored.revision.as_deref(), Some("third"));
        check!(!restored.dirty);
        check!(catalog.playlist_file_auto_save(Some(&source)).await?);
        check!(
            catalog
                .is_playlist_file_ignored(Some(&source), "0/ignored.pls")
                .await?
        );
        catalog.delete_playlist(playlist).await?;
        check!(catalog.get_playlist_file_link(playlist).await?.is_none());
        catalog.close().await
    }

    #[tokio::test]
    async fn backup_restores_links_by_identity() -> LibraryResult<()> {
        let directory = tempfile::tempdir()?;
        let catalog = Catalog::open(directory.path().join("store.sqlite")).await?;
        let playlist = catalog
            .create_playlist(
                "Playlist",
                &crate::MemberSelection::Uris(((&[]).to_vec()).into()),
            )
            .await?
            .unwrap()
            .0;
        let link = PlaylistFileLink {
            playlist,
            source_id: None,
            path: directory
                .path()
                .join("playlist.xspf")
                .to_string_lossy()
                .into_owned(),
            auto_refresh: true,
            auto_save: Some(true),
            path_mode: PlaylistPathMode::Relative,
            revision: None,
            dirty: true,
            error: Some("Unavailable".into()),
        };
        catalog.save_playlist_file_link(&link).await?;
        catalog.ignore_playlist_file(None, "ignored.m3u").await?;
        catalog.set_playlist_file_auto_save(None, true).await?;
        let backup = directory.path().join("backup");
        std::fs::create_dir(&backup)?;
        let groups = crate::StateGroups {
            playlists: true,
            favorites: false,
            local_imports: false,
            activity: false,
            queue: false,
        };
        let count = catalog.export_state(&backup, groups).await?;
        catalog.unlink_playlist_file(playlist).await?;
        catalog.restore_state(&backup, groups, count).await?;
        let restored = catalog
            .find_playlist_file_link(None, &link.path)
            .await?
            .unwrap();
        check_eq!(restored.path_mode, PlaylistPathMode::Relative);
        check_eq!(restored.auto_save, Some(true));
        check!(restored.dirty);
        check_eq!(restored.error, link.error);
        check!(
            catalog
                .is_playlist_file_ignored(None, "ignored.m3u")
                .await?
        );
        check!(catalog.playlist_file_auto_save(None).await?);
        catalog.close().await
    }
}

fn location_source(source: Option<&SourceId>) -> &str {
    source.map_or("", SourceId::as_str)
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct ObservedPlaylistFile {
    pub local_file_key: crate::LocalFileKey,
    pub playlist_identity: Option<String>,
    pub playlist: Option<PlaylistKey>,
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub size_bytes: Option<i64>,
    pub mtime_ns: i64,
    pub revision: Option<String>,
}

fn read_link(row: sqlx::sqlite::SqliteRow) -> LibraryResult<PlaylistFileLink> {
    let source: String = row.try_get("source_id")?;
    Ok(PlaylistFileLink {
        playlist: row.try_get("playlist_key")?,
        source_id: (!source.is_empty()).then(|| SourceId::new(source)),
        path: row.try_get("path")?,
        auto_refresh: row.try_get("auto_refresh")?,
        auto_save: row.try_get("auto_save")?,
        path_mode: serde_json::from_str(row.try_get("path_mode")?)?,
        revision: row.try_get("revision")?,
        dirty: row.try_get("dirty")?,
        error: row.try_get("error")?,
    })
}

pub(crate) async fn save_link_on(
    connection: &mut SqliteConnection,
    link: &PlaylistFileLink,
) -> LibraryResult<bool> {
    let authored = linkable_on(connection, link.playlist).await?;
    if !authored {
        return Err(LibraryError::InvalidRequest(
            "Only an editable playlist can be linked to a file".into(),
        ));
    }
    Ok(sqlx::query("INSERT INTO playlist_file_links(playlist_key,source_id,path,auto_refresh,auto_save,path_mode,revision,dirty,error)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
        ON CONFLICT(playlist_key) DO UPDATE SET source_id=excluded.source_id,path=excluded.path,
        auto_refresh=excluded.auto_refresh,auto_save=excluded.auto_save,path_mode=excluded.path_mode,
        revision=excluded.revision,dirty=excluded.dirty,error=excluded.error
        WHERE (source_id,path,auto_refresh,auto_save,path_mode,revision,dirty,error)
        IS NOT (excluded.source_id,excluded.path,excluded.auto_refresh,excluded.auto_save,excluded.path_mode,excluded.revision,excluded.dirty,excluded.error)")
        .bind(link.playlist).bind(location_source(link.source_id.as_ref())).bind(&link.path)
        .bind(link.auto_refresh).bind(link.auto_save).bind(serde_json::to_string(&link.path_mode)?)
        .bind(&link.revision).bind(link.dirty).bind(&link.error).execute(connection).await?.rows_affected() != 0)
}

async fn linkable_on(
    connection: &mut SqliteConnection,
    playlist: PlaylistKey,
) -> LibraryResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM main.playlists WHERE playlist_key=?1 AND owner='rufin' AND name IS NOT NULL)")
        .bind(playlist).fetch_one(connection).await?)
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) enum PlaylistFileRecord {
    Link {
        source_id: Option<String>,
        object_id: String,
        link: PlaylistFileLink,
    },
    Ignored {
        source_id: String,
        path: String,
    },
    Settings {
        source_id: String,
        auto_save: bool,
    },
}

pub(crate) async fn backup_records_on(
    connection: &mut SqliteConnection,
    mut write: impl FnMut(PlaylistFileRecord) -> LibraryResult<()>,
) -> LibraryResult<()> {
    use futures_util::TryStreamExt;
    let mut rows = sqlx::query("SELECT link.*,source.object_id owner_source_id,playlist.object_id FROM playlist_file_links link JOIN main.playlists playlist USING(playlist_key) LEFT JOIN sources source USING(source_key)").fetch(&mut *connection);
    while let Some(row) = rows.try_next().await? {
        write(PlaylistFileRecord::Link {
            source_id: row.try_get("owner_source_id")?,
            object_id: row.try_get("object_id")?,
            link: read_link(row)?,
        })?;
    }
    drop(rows);
    let mut rows =
        sqlx::query("SELECT source_id,path FROM playlist_file_ignored").fetch(&mut *connection);
    while let Some(row) = rows.try_next().await? {
        write(PlaylistFileRecord::Ignored {
            source_id: row.try_get("source_id")?,
            path: row.try_get("path")?,
        })?;
    }
    drop(rows);
    let mut rows = sqlx::query("SELECT source_id,auto_save FROM playlist_file_settings")
        .fetch(&mut *connection);
    while let Some(row) = rows.try_next().await? {
        write(PlaylistFileRecord::Settings {
            source_id: row.try_get("source_id")?,
            auto_save: row.try_get("auto_save")?,
        })?;
    }
    Ok(())
}

pub(crate) async fn restore_record_on(
    connection: &mut SqliteConnection,
    record: PlaylistFileRecord,
) -> LibraryResult<()> {
    match record {
        PlaylistFileRecord::Link {
            source_id,
            object_id,
            mut link,
        } => {
            link.playlist = sqlx::query_scalar("SELECT playlist_key FROM main.playlists playlist LEFT JOIN sources source USING(source_key) WHERE source.object_id IS ?1 AND playlist.object_id=?2")
                .bind(source_id).bind(object_id).fetch_one(&mut *connection).await?;
            save_link_on(connection, &link).await?;
        }
        PlaylistFileRecord::Ignored { source_id, path } => {
            sqlx::query("INSERT INTO playlist_file_ignored(source_id,path) VALUES(?1,?2)")
                .bind(source_id)
                .bind(path)
                .execute(connection)
                .await?;
        }
        PlaylistFileRecord::Settings {
            source_id,
            auto_save,
        } => {
            sqlx::query("INSERT INTO playlist_file_settings(source_id,auto_save) VALUES(?1,?2)")
                .bind(source_id)
                .bind(auto_save)
                .execute(connection)
                .await?;
        }
    }
    Ok(())
}

impl Catalog {
    /// Keep file locations attached to the same folders when earlier roots are removed.
    pub async fn remove_playlist_file_roots(
        &self,
        source: &SourceId,
        removed: &[(usize, std::path::PathBuf)],
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let connection = write.connection().await?;
        let mut removed = removed.iter().collect::<Vec<_>>();
        removed.sort_by_key(|(index, _)| std::cmp::Reverse(*index));
        for (index, root) in removed {
            let prefix = format!("@{index}/");
            let root = format!(
                "{}{}",
                root.to_string_lossy()
                    .trim_end_matches(std::path::MAIN_SEPARATOR),
                std::path::MAIN_SEPARATOR
            );
            sqlx::query("UPDATE playlist_file_links SET source_id='',path=?3||substr(path,length(?2)+1),auto_save=COALESCE(auto_save,(SELECT auto_save FROM playlist_file_settings WHERE source_id=?1),0) WHERE source_id=?1 AND substr(path,1,length(?2))=?2")
                .bind(source.as_str()).bind(&prefix).bind(&root).execute(&mut *connection).await?;
            sqlx::query("INSERT OR IGNORE INTO playlist_file_ignored(source_id,path) SELECT '',?3||substr(path,length(?2)+1) FROM playlist_file_ignored WHERE source_id=?1 AND substr(path,1,length(?2))=?2")
                .bind(source.as_str()).bind(&prefix).bind(&root).execute(&mut *connection).await?;
            sqlx::query("DELETE FROM playlist_file_ignored WHERE source_id=?1 AND substr(path,1,length(?2))=?2")
                .bind(source.as_str()).bind(&prefix).execute(&mut *connection).await?;
            // Move higher ordinals out of their occupied namespace before compacting them.
            for table in ["playlist_file_links", "playlist_file_ignored"] {
                sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET path='@-'||substr(path,2) WHERE source_id=?1 AND substr(path,1,1)='@' AND CAST(substr(path,2,instr(path,'/')-2) AS INTEGER)>?2")))
                    .bind(source.as_str()).bind(*index as i64).execute(&mut *connection).await?;
                sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET path='@'||(CAST(substr(path,3,instr(path,'/')-3) AS INTEGER)-1)||substr(path,instr(path,'/')) WHERE source_id=?1 AND substr(path,1,2)='@-'")))
                    .bind(source.as_str()).execute(&mut *connection).await?;
            }
        }
        write.keep();
        write.commit().await?;
        Ok(())
    }

    pub async fn playlist_file_linkable(&self, playlist: PlaylistKey) -> LibraryResult<bool> {
        let mut reader = self.acquire_reader().await?;
        linkable_on(&mut reader, playlist).await
    }
    /// Whether this exact file was inspected and rejected by its source scan.
    pub async fn file_path_is_rejected(
        &self,
        source_id: &SourceId,
        path: &str,
    ) -> LibraryResult<bool> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM observed_local_files file JOIN sources source USING(source_key) WHERE source.object_id=?1 AND file.path=?2 AND file.state='rejected')")
            .bind(source_id.as_str()).bind(path).fetch_one(&mut *reader).await?)
    }

    pub async fn observed_playlist_file_page(
        &self,
        source: crate::SourceKey,
        after: Option<crate::LocalFileKey>,
    ) -> LibraryResult<Vec<ObservedPlaylistFile>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT observed.local_file_key,observed.path,observed.root,observed.relative_path,observed.size_bytes,observed.mtime_ns,observed.revision,file.playlist_identity,(SELECT playlist.playlist_key FROM playlists playlist WHERE playlist.source_key IS(SELECT source_key FROM sources WHERE object_id=json_extract(file.playlist_identity,'$[0]')) AND (json_extract(file.playlist_identity,'$[0]') IS NULL OR playlist.source_key IS NOT NULL) AND playlist.object_id=json_extract(file.playlist_identity,'$[1]') AND playlist.owner='rufin' AND playlist.name IS NOT NULL) playlist FROM local_files file JOIN observed_local_files observed USING(local_file_key) WHERE file.source_key=?1 AND file.local_file_key>?2 AND (lower(file.path) LIKE '%.m3u' OR lower(file.path) LIKE '%.m3u8' OR lower(file.path) LIKE '%.pls' OR lower(file.path) LIKE '%.xspf') AND observed.kind<>'directory' AND (lower(observed.path) LIKE '%.m3u' OR lower(observed.path) LIKE '%.m3u8' OR lower(observed.path) LIKE '%.pls' OR lower(observed.path) LIKE '%.xspf') ORDER BY file.local_file_key LIMIT 256")
            .bind(source).bind(after.map_or(0, crate::LocalFileKey::raw)).fetch_all(&mut *reader).await?)
    }
    pub async fn associate_playlist_files(
        &self,
        source: &SourceId,
        files: &[(String, PlaylistKey)],
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let connection = write.connection().await?;
        for (path, playlist) in files {
            sqlx::query("WITH target AS(SELECT json_array(source.object_id,playlist.object_id) identity FROM playlists playlist LEFT JOIN sources source USING(source_key) WHERE playlist.playlist_key=?3 AND playlist.owner='rufin' AND playlist.name IS NOT NULL) UPDATE local_files SET playlist_identity=(SELECT identity FROM target) WHERE local_file_key=(SELECT file.local_file_key FROM observed_local_files file JOIN sources source USING(source_key) WHERE source.object_id=?1 AND file.path=?2) AND EXISTS(SELECT 1 FROM target) AND playlist_identity IS NOT(SELECT identity FROM target)")
                .bind(source.as_str()).bind(path).bind(playlist).execute(&mut *connection).await?;
        }
        write.keep();
        write.commit().await?;
        Ok(())
    }

    pub async fn get_playlist_file_link(
        &self,
        playlist: PlaylistKey,
    ) -> LibraryResult<Option<PlaylistFileLink>> {
        let mut reader = self.acquire_reader().await?;
        sqlx::query("SELECT * FROM playlist_file_links WHERE playlist_key=?1")
            .bind(playlist)
            .fetch_optional(&mut *reader)
            .await?
            .map(read_link)
            .transpose()
    }

    pub async fn find_playlist_file_link(
        &self,
        source_id: Option<&SourceId>,
        path: &str,
    ) -> LibraryResult<Option<PlaylistFileLink>> {
        let mut reader = self.acquire_reader().await?;
        sqlx::query("SELECT * FROM playlist_file_links WHERE source_id=?1 AND path=?2")
            .bind(location_source(source_id))
            .bind(path)
            .fetch_optional(&mut *reader)
            .await?
            .map(read_link)
            .transpose()
    }

    pub async fn playlist_file_link_page(
        &self,
        after: PlaylistKey,
        limit: i64,
    ) -> LibraryResult<Vec<PlaylistFileLink>> {
        let mut reader = self.acquire_reader().await?;
        sqlx::query("SELECT * FROM playlist_file_links WHERE playlist_key>?1 ORDER BY playlist_key LIMIT ?2")
            .bind(after).bind(limit.clamp(0,256)).fetch_all(&mut *reader).await?.into_iter().map(read_link).collect()
    }

    pub async fn save_playlist_file_link(&self, link: &PlaylistFileLink) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let saved = save_link_on(write.connection().await?, link).await?;
        write.keep();
        write.commit().await?;
        Ok(saved)
    }

    pub async fn unlink_playlist_file(&self, playlist: PlaylistKey) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let removed = sqlx::query("DELETE FROM playlist_file_links WHERE playlist_key=?1")
            .bind(playlist)
            .execute(write.connection().await?)
            .await?
            .rows_affected();
        write.keep();
        write.commit().await?;
        Ok(removed != 0)
    }

    /// The playlists linked to a file.
    pub async fn linked_playlists(&self) -> LibraryResult<Vec<PlaylistKey>> {
        let mut reader = self.acquire_reader().await?;
        Ok(
            sqlx::query_scalar("SELECT playlist_key FROM playlist_file_links")
                .fetch_all(&mut *reader)
                .await?,
        )
    }

    pub async fn mark_playlist_file_synced(
        &self,
        playlist: PlaylistKey,
        revision: &str,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        sqlx::query("UPDATE playlist_file_links SET revision=?2,dirty=0,error=NULL WHERE playlist_key=?1 AND (revision IS NOT ?2 OR dirty<>0 OR error IS NOT NULL)")
            .bind(playlist).bind(revision).execute(write.connection().await?).await?;
        write.keep();
        write.commit().await?;
        Ok(())
    }

    pub async fn set_playlist_file_error(
        &self,
        playlist: PlaylistKey,
        error: Option<&str>,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        sqlx::query(
            "UPDATE playlist_file_links SET error=?2 WHERE playlist_key=?1 AND error IS NOT ?2",
        )
        .bind(playlist)
        .bind(error)
        .execute(write.connection().await?)
        .await?;
        write.keep();
        write.commit().await?;
        Ok(())
    }

    pub async fn ignore_playlist_file(
        &self,
        source_id: Option<&SourceId>,
        path: &str,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        sqlx::query("INSERT OR IGNORE INTO playlist_file_ignored(source_id,path) VALUES(?1,?2)")
            .bind(location_source(source_id))
            .bind(path)
            .execute(write.connection().await?)
            .await?;
        write.keep();
        write.commit().await?;
        Ok(())
    }

    pub async fn is_playlist_file_ignored(
        &self,
        source_id: Option<&SourceId>,
        path: &str,
    ) -> LibraryResult<bool> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM playlist_file_ignored WHERE source_id=?1 AND path=?2)",
        )
        .bind(location_source(source_id))
        .bind(path)
        .fetch_one(&mut *reader)
        .await?)
    }

    pub async fn playlist_file_auto_save(
        &self,
        source_id: Option<&SourceId>,
    ) -> LibraryResult<bool> {
        let mut reader = self.acquire_reader().await?;
        Ok(
            sqlx::query_scalar("SELECT auto_save FROM playlist_file_settings WHERE source_id=?1")
                .bind(location_source(source_id))
                .fetch_optional(&mut *reader)
                .await?
                .unwrap_or(false),
        )
    }

    pub async fn set_playlist_file_auto_save(
        &self,
        source_id: Option<&SourceId>,
        enabled: bool,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        sqlx::query("INSERT INTO playlist_file_settings(source_id,auto_save) VALUES(?1,?2) ON CONFLICT(source_id) DO UPDATE SET auto_save=excluded.auto_save WHERE auto_save<>excluded.auto_save")
            .bind(location_source(source_id)).bind(enabled).execute(write.connection().await?).await?;
        write.keep();
        write.commit().await?;
        Ok(())
    }
}

/// An edit made the playlist differ from its linked file.
pub(crate) async fn mark_dirty_on(
    connection: &mut SqliteConnection,
    playlist: PlaylistKey,
) -> LibraryResult<()> {
    sqlx::query("UPDATE playlist_file_links SET dirty=1 WHERE playlist_key=?1 AND dirty=0")
        .bind(playlist)
        .execute(connection)
        .await?;
    Ok(())
}
