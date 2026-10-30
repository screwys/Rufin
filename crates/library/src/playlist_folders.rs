//! Library-owned groups for regular and smart playlists.
use crate::catalog::write::{
    FOLDER_COLUMNS, FolderRow, PLAYLIST_COLUMNS, PlaylistRow as PlaylistHeader, SMART_COLUMNS,
    SmartRow, Write,
};
use crate::*;
use sqlx::{FromRow, SqliteConnection};

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistFolderRow {
    pub folder_key: PlaylistFolderKey,
    pub object_id: String,
    pub name: String,
    pub smart: bool,
    pub artwork_binding: Option<Vec<u8>>,
    pub track_count: i64,
    pub duration_millis: i64,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum PlaylistListRow {
    Playlist(PlaylistRow),
    SmartPlaylist(SmartPlaylistRow),
    Folder(PlaylistFolderRow),
}
impl PlaylistListRow {
    pub fn key(&self) -> (u8, i64) {
        match self {
            Self::Playlist(row) => (0, row.playlist_key.raw()),
            Self::SmartPlaylist(row) => (1, row.smart_playlist_key.raw()),
            Self::Folder(row) => (2, row.folder_key.raw()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum PlaylistFolderMember {
    Playlist { key: PlaylistKey },
    SmartPlaylist { key: SmartPlaylistKey },
}

fn member_relation(member: &PlaylistFolderMember) -> (&'static str, &'static str, i64) {
    match member {
        PlaylistFolderMember::Playlist { key } => ("playlists", "playlist_key", key.raw()),
        PlaylistFolderMember::SmartPlaylist { key } => {
            ("smart_playlists", "smart_playlist_key", key.raw())
        }
    }
}

pub(crate) async fn move_to_folder_on(
    write: &mut Write<'_>,
    member: PlaylistFolderMember,
    folder: Option<PlaylistFolderKey>,
) -> LibraryResult<()> {
    let smart = matches!(member, PlaylistFolderMember::SmartPlaylist { .. });
    let (table, column, key) = member_relation(&member);
    let connection = write.connection().await?;
    if let Some(folder) = folder {
        let namespace: Option<bool> =
            sqlx::query_scalar("SELECT smart FROM playlist_folders WHERE folder_key=?1")
                .bind(folder)
                .fetch_optional(&mut *connection)
                .await?;
        if namespace != Some(smart) {
            return Err(LibraryError::InvalidRequest(
                "Playlist folder does not match the playlist kind".into(),
            ));
        }
    }
    let returning = if smart {
        SMART_COLUMNS
    } else {
        PLAYLIST_COLUMNS
    };
    let moved = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {table} SET playlist_folder_key=?2 WHERE {column}=?1 AND playlist_folder_key IS NOT ?2 RETURNING {returning}"
    )))
    .bind(key).bind(folder).fetch_optional(connection).await?;
    if let Some(row) = moved {
        if smart {
            write.put_smart(SmartRow::from_row(&row)?);
        } else {
            write.put_playlist(PlaylistHeader::from_row(&row)?);
        }
    }
    Ok(())
}

fn folder_name(name: &str) -> LibraryResult<&str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LibraryError::InvalidRequest(
            "Folder name cannot be empty".into(),
        ));
    }
    Ok(name)
}
impl Catalog {
    pub async fn create_playlist_folder(
        &self,
        name: &str,
        smart: bool,
    ) -> LibraryResult<PlaylistFolderKey> {
        let name = folder_name(name)?;
        let mut write = self.write().await?;
        let row: FolderRow = sqlx::query_as(sqlx::AssertSqlSafe(format!("INSERT INTO playlist_folders(object_id,name,smart) VALUES('rufin:playlist-folder:'||lower(hex(randomblob(16))),?1,?2) RETURNING {FOLDER_COLUMNS}"))).bind(name).bind(smart).fetch_one(write.connection().await?).await?;
        let key = PlaylistFolderKey::from_raw(row.0);
        write.put_folder(row);
        write.commit().await?;
        Ok(key)
    }
    pub async fn rename_playlist_folder(
        &self,
        key: PlaylistFolderKey,
        name: &str,
    ) -> LibraryResult<()> {
        let name = folder_name(name)?;
        let mut write = self.write().await?;
        let row: Option<FolderRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE playlist_folders SET name=?2 WHERE folder_key=?1 AND name IS NOT ?2 RETURNING {FOLDER_COLUMNS}"
        )))
        .bind(key)
        .bind(name)
        .fetch_optional(write.connection().await?)
        .await?;
        if let Some(row) = row {
            write.put_folder(row);
        }
        write.commit().await?;
        Ok(())
    }
    pub async fn delete_playlist_folder(
        &self,
        key: PlaylistFolderKey,
    ) -> LibraryResult<Option<(String, bool)>> {
        let mut write = self.write().await?;
        // Members leave the folder first, so their rows say so.
        let playlists: Vec<PlaylistHeader> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE playlists SET playlist_folder_key=NULL WHERE playlist_folder_key=?1 RETURNING {PLAYLIST_COLUMNS}"
        )))
        .bind(key)
        .fetch_all(write.connection().await?)
        .await?;
        let smart: Vec<SmartRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE smart_playlists SET playlist_folder_key=NULL WHERE playlist_folder_key=?1 RETURNING {SMART_COLUMNS}"
        )))
        .bind(key)
        .fetch_all(write.connection().await?)
        .await?;
        let deleted = sqlx::query_as(
            "DELETE FROM playlist_folders WHERE folder_key=?1 RETURNING object_id,smart",
        )
        .bind(key)
        .fetch_optional(write.connection().await?)
        .await?;
        for playlist in playlists {
            write.put_playlist(playlist);
        }
        for smart in smart {
            write.put_smart(smart);
        }
        if deleted.is_some() {
            write.remove_folder(key.raw());
        }
        write.commit().await?;
        Ok(deleted)
    }
    /// The folder a playlist or smart playlist is in.
    pub fn playlist_member_folder(
        &self,
        member: &PlaylistFolderMember,
    ) -> Option<PlaylistFolderKey> {
        let snapshot = self.snapshot();
        match member {
            PlaylistFolderMember::Playlist { key } => snapshot
                .playlists
                .get(key.raw())
                .filter(|playlist| playlist.folder != 0)
                .map(|playlist| PlaylistFolderKey::from_raw(playlist.folder)),
            PlaylistFolderMember::SmartPlaylist { key } => snapshot
                .smart_playlists
                .get(&key.raw())
                .and_then(|smart| smart.folder)
                .map(PlaylistFolderKey::from_raw),
        }
    }
    pub async fn move_playlist_to_folder(
        &self,
        member: PlaylistFolderMember,
        folder: Option<PlaylistFolderKey>,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        move_to_folder_on(&mut write, member, folder).await?;
        write.commit().await?;
        Ok(())
    }
    /// Playlist folders of one kind by name.
    pub fn playlist_folder_choices(&self, smart: bool) -> Vec<(PlaylistFolderKey, String)> {
        let snapshot = self.snapshot();
        let mut folders = snapshot
            .playlist_folders
            .values()
            .filter(|folder| folder.smart == smart)
            .map(|folder| (folder.name.to_lowercase(), folder.key, folder.name.clone()))
            .collect::<Vec<_>>();
        folders.sort();
        folders
            .into_iter()
            .map(|(_, key, name)| (PlaylistFolderKey::from_raw(key), name))
            .collect()
    }

    pub async fn playlist_folder_by_id(
        &self,
        id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistFolderRow>> {
        self.loaded().await;
        let key = self
            .snapshot()
            .playlist_folders
            .values()
            .find(|folder| folder.object == id)
            .map(|folder| PlaylistFolderKey::from_raw(folder.key));
        let Some(key) = key else {
            return Ok(None);
        };
        Ok(self
            .rows_of(&[Item::PlaylistFolder(key)], true, cancellation)
            .await?
            .into_iter()
            .find_map(|row| match row {
                Row::Playlist(PlaylistListRow::Folder(row)) => Some(row),
                _ => None,
            }))
    }
}

/// A playlist folder by object id.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistFolderCopy {
    pub object_id: String,
    pub name: String,
    pub smart: bool,
}

impl Catalog {
    /// Every playlist folder by object id.
    pub async fn playlist_folder_copies(&self) -> Vec<PlaylistFolderCopy> {
        self.loaded().await;
        self.snapshot()
            .playlist_folders
            .values()
            .map(|folder| PlaylistFolderCopy {
                object_id: folder.object.clone(),
                name: folder.name.clone(),
                smart: folder.smart,
            })
            .collect()
    }

    /// Creates or renames playlist folders by object id, in one Write.
    pub async fn put_playlist_folders(&self, folders: &[PlaylistFolderCopy]) -> LibraryResult<()> {
        let folders = folders
            .iter()
            .map(|folder| Ok((&folder.object_id, folder_name(&folder.name)?, folder.smart)))
            .collect::<LibraryResult<Vec<_>>>()?;
        if folders.is_empty() {
            return Ok(());
        }
        let mut write = self.write().await?;
        let rows: Vec<FolderRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO playlist_folders(object_id,name,smart)
             SELECT value->>0,value->>1,value->>2 FROM json_each(?1) WHERE true
             ON CONFLICT(object_id) DO UPDATE SET name=excluded.name WHERE name IS NOT excluded.name
             RETURNING {FOLDER_COLUMNS}"
        )))
        .bind(serde_json::to_string(&folders)?)
        .fetch_all(write.connection().await?)
        .await?;
        for row in rows {
            write.put_folder(row);
        }
        write.commit().await?;
        Ok(())
    }

    /// Deletes playlist folders by object id, in one Write. Their members leave them.
    pub async fn delete_playlist_folders_by_object(&self, objects: &[String]) -> LibraryResult<()> {
        if objects.is_empty() {
            return Ok(());
        }
        let objects = serde_json::to_string(objects)?;
        let folders = "(SELECT folder_key FROM playlist_folders WHERE object_id IN(SELECT value FROM json_each(?1)))";
        let mut write = self.write().await?;
        let playlists: Vec<PlaylistHeader> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE playlists SET playlist_folder_key=NULL WHERE playlist_folder_key IN{folders} RETURNING {PLAYLIST_COLUMNS}"
        )))
        .bind(&objects)
        .fetch_all(write.connection().await?)
        .await?;
        let smart: Vec<SmartRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE smart_playlists SET playlist_folder_key=NULL WHERE playlist_folder_key IN{folders} RETURNING {SMART_COLUMNS}"
        )))
        .bind(&objects)
        .fetch_all(write.connection().await?)
        .await?;
        let deleted: Vec<i64> = sqlx::query_scalar(
            "DELETE FROM playlist_folders WHERE object_id IN(SELECT value FROM json_each(?1)) RETURNING folder_key",
        )
        .bind(&objects)
        .fetch_all(write.connection().await?)
        .await?;
        for playlist in playlists {
            write.put_playlist(playlist);
        }
        for smart in smart {
            write.put_smart(smart);
        }
        for key in deleted {
            write.remove_folder(key);
        }
        write.commit().await?;
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
enum FolderRecord {
    Folder {
        id: String,
        name: String,
        smart: bool,
    },
    Playlist {
        folder: String,
        source: Option<String>,
        id: String,
        cover: bool,
    },
    SmartPlaylist {
        folder: String,
        id: String,
        cover: bool,
    },
}
pub(crate) async fn export_on(
    connection: &mut SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<()> {
    use futures_util::TryStreamExt;
    let mut folders = sqlx::query_as::<_, (String, String, bool)>(
        "SELECT object_id,name,smart FROM playlist_folders ORDER BY folder_key",
    )
    .fetch(&mut *connection);
    while let Some((id, name, smart)) = folders.try_next().await? {
        serde_json::to_writer(&mut output, &FolderRecord::Folder { id, name, smart })?;
        output.write_all(b"\n")?;
    }
    drop(folders);
    let mut members = sqlx::query_as::<_, (String, Option<String>, String, bool)>("SELECT folder.object_id,source.object_id,playlist.object_id,playlist.playlist_key=(SELECT min(child.playlist_key) FROM playlists child WHERE child.playlist_folder_key=folder.folder_key AND child.name IS NOT NULL AND (child.owner='rufin' OR child.indexed)) FROM playlists playlist JOIN playlist_folders folder ON folder.folder_key=playlist.playlist_folder_key LEFT JOIN sources source USING(source_key) ORDER BY folder.folder_key,playlist.playlist_key").fetch(&mut *connection);
    while let Some((folder, source, id, cover)) = members.try_next().await? {
        serde_json::to_writer(
            &mut output,
            &FolderRecord::Playlist {
                folder,
                source,
                id,
                cover,
            },
        )?;
        output.write_all(b"\n")?;
    }
    drop(members);
    let mut members = sqlx::query_as::<_, (String, String, bool)>("SELECT folder.object_id,playlist.object_id,playlist.smart_playlist_key=(SELECT min(child.smart_playlist_key) FROM smart_playlists child WHERE child.playlist_folder_key=folder.folder_key) FROM smart_playlists playlist JOIN playlist_folders folder ON folder.folder_key=playlist.playlist_folder_key ORDER BY folder.folder_key,playlist.smart_playlist_key").fetch(&mut *connection);
    while let Some((folder, id, cover)) = members.try_next().await? {
        serde_json::to_writer(
            &mut output,
            &FolderRecord::SmartPlaylist { folder, id, cover },
        )?;
        output.write_all(b"\n")?;
    }
    Ok(())
}
pub(crate) async fn import_on(
    connection: &mut SqliteConnection,
    input: impl std::io::BufRead,
) -> LibraryResult<()> {
    for line in input.lines() {
        match serde_json::from_str::<FolderRecord>(&line?)? {
            FolderRecord::Folder { id, name, smart } => {
                sqlx::query("INSERT INTO playlist_folders(object_id,name,smart) VALUES(?1,?2,?3)")
                    .bind(id)
                    .bind(name)
                    .bind(smart)
                    .execute(&mut *connection)
                    .await?;
            }
            // A folder's cover is its first playlist, so the saved cover choice is not read.
            FolderRecord::Playlist {
                folder, source, id, ..
            } => {
                sqlx::query("UPDATE playlists SET playlist_folder_key=(SELECT folder_key FROM playlist_folders WHERE object_id=?3 AND NOT smart) WHERE object_id=?1 AND (owner='rufin' OR source_key IS(SELECT source_key FROM sources WHERE object_id=?2))").bind(&id).bind(&source).bind(&folder).execute(&mut *connection).await?;
            }
            FolderRecord::SmartPlaylist { folder, id, .. } => {
                sqlx::query("UPDATE smart_playlists SET playlist_folder_key=(SELECT folder_key FROM playlist_folders WHERE object_id=?2 AND smart) WHERE object_id=?1").bind(&id).bind(&folder).execute(&mut *connection).await?;
            }
        }
    }
    Ok(())
}
