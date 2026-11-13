//! Library-owned groups of playlists and smart playlists. A folder holds both kinds and shows
//! on both pages; a playlist may be in several.
use crate::catalog::write::{
    FOLDER_COLUMNS, FolderRow, PLAYLIST_COLUMNS, PlaylistRow as PlaylistHeader, SMART_COLUMNS,
    SmartRow, Write,
};
use crate::*;
use sqlx::SqliteConnection;

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistFolderRow {
    pub folder_key: PlaylistFolderKey,
    pub object_id: String,
    pub name: String,
    /// Whether the row was read on the Smart Playlists page, which its page opens on.
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

/// A member's membership table, its key column there and its key.
fn member_relation(member: &PlaylistFolderMember) -> (&'static str, &'static str, i64) {
    match member {
        PlaylistFolderMember::Playlist { key } => {
            ("playlist_folder_playlists", "playlist_key", key.raw())
        }
        PlaylistFolderMember::SmartPlaylist { key } => (
            "playlist_folder_smart_playlists",
            "smart_playlist_key",
            key.raw(),
        ),
    }
}

/// Stages a member's row again, which lists the folders it is in.
async fn stage_member(write: &mut Write<'_>, member: &PlaylistFolderMember) -> LibraryResult<()> {
    let connection = write.connection().await?;
    match member {
        PlaylistFolderMember::Playlist { key } => {
            let row: Option<PlaylistHeader> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT {PLAYLIST_COLUMNS} FROM playlists WHERE playlist_key=?1"
            )))
            .bind(key.raw())
            .fetch_optional(connection)
            .await?;
            if let Some(row) = row {
                write.put_playlist(row);
            }
        }
        PlaylistFolderMember::SmartPlaylist { key } => {
            let row: Option<SmartRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT {SMART_COLUMNS} FROM smart_playlists WHERE smart_playlist_key=?1"
            )))
            .bind(key.raw())
            .fetch_optional(connection)
            .await?;
            if let Some(row) = row {
                write.put_smart(row);
            }
        }
    }
    Ok(())
}

pub(crate) async fn add_to_folder_on(
    write: &mut Write<'_>,
    members: &[PlaylistFolderMember],
    folder: PlaylistFolderKey,
) -> LibraryResult<()> {
    for member in members {
        let (table, column, key) = member_relation(member);
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT OR IGNORE INTO {table}({column},folder_key) VALUES(?1,?2)"
        )))
        .bind(key)
        .bind(folder)
        .execute(write.connection().await?)
        .await?;
        stage_member(write, member).await?;
    }
    Ok(())
}

/// Puts `member` in exactly the folders of these object ids that exist.
pub(crate) async fn set_folders_on(
    write: &mut Write<'_>,
    member: &PlaylistFolderMember,
    folders: &[String],
) -> LibraryResult<()> {
    let (table, column, key) = member_relation(member);
    let folders = serde_json::to_string(folders)?;
    let named = "SELECT folder_key FROM playlist_folders WHERE object_id IN(SELECT value FROM json_each(?2))";
    let connection = write.connection().await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM {table} WHERE {column}=?1 AND folder_key NOT IN({named})"
    )))
    .bind(key)
    .bind(&folders)
    .execute(&mut *connection)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT OR IGNORE INTO {table}({column},folder_key) SELECT ?1,folder_key FROM({named})"
    )))
    .bind(key)
    .bind(&folders)
    .execute(connection)
    .await?;
    stage_member(write, member).await
}

/// The object ids of a JSON list a statement built.
pub(crate) fn object_ids(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_default()
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
    pub async fn create_playlist_folder(&self, name: &str) -> LibraryResult<PlaylistFolderKey> {
        let name = folder_name(name)?;
        let mut write = self.write().await?;
        let row: FolderRow = sqlx::query_as(sqlx::AssertSqlSafe(format!("INSERT INTO playlist_folders(object_id,name) VALUES('rufin:playlist-folder:'||lower(hex(randomblob(16))),?1) RETURNING {FOLDER_COLUMNS}"))).bind(name).fetch_one(write.connection().await?).await?;
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
    /// Deletes playlist folders in one Write; their members' rows say they left them. The
    /// object id of each folder that went.
    pub async fn delete_playlist_folders(
        &self,
        keys: &[PlaylistFolderKey],
    ) -> LibraryResult<Vec<String>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let keys = serde_json::to_string(keys)?;
        let folders = "(SELECT value FROM json_each(?1))";
        let mut write = self.write().await?;
        let playlists: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT playlist_key FROM playlist_folder_playlists WHERE folder_key IN{folders}"
        )))
        .bind(&keys)
        .fetch_all(write.connection().await?)
        .await?;
        let smart: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT smart_playlist_key FROM playlist_folder_smart_playlists WHERE folder_key IN{folders}"
        )))
        .bind(&keys)
        .fetch_all(write.connection().await?)
        .await?;
        let deleted: Vec<(i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "DELETE FROM playlist_folders WHERE folder_key IN{folders} RETURNING folder_key,object_id"
        )))
        .bind(&keys)
        .fetch_all(write.connection().await?)
        .await?;
        let members = playlists
            .into_iter()
            .map(|key| PlaylistFolderMember::Playlist {
                key: PlaylistKey::from_raw(key),
            })
            .chain(
                smart
                    .into_iter()
                    .map(|key| PlaylistFolderMember::SmartPlaylist {
                        key: SmartPlaylistKey::from_raw(key),
                    }),
            );
        for member in members {
            stage_member(&mut write, &member).await?;
        }
        for (key, _) in &deleted {
            write.remove_folder(*key);
        }
        write.commit().await?;
        Ok(deleted.into_iter().map(|(_, object)| object).collect())
    }
    /// The folders a playlist or smart playlist is in.
    pub fn playlist_member_folders(&self, member: &PlaylistFolderMember) -> Vec<PlaylistFolderKey> {
        let snapshot = self.snapshot();
        let folders = match member {
            PlaylistFolderMember::Playlist { key } => snapshot
                .playlists
                .get(key.raw())
                .map(|playlist| playlist.folders.to_vec()),
            PlaylistFolderMember::SmartPlaylist { key } => snapshot
                .smart_playlists
                .get(&key.raw())
                .map(|smart| smart.folders.clone()),
        };
        folders
            .unwrap_or_default()
            .into_iter()
            .map(PlaylistFolderKey::from_raw)
            .collect()
    }
    pub async fn add_to_playlist_folder(
        &self,
        members: &[PlaylistFolderMember],
        folder: PlaylistFolderKey,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        add_to_folder_on(&mut write, members, folder).await?;
        write.commit().await?;
        Ok(())
    }
    /// Takes playlists and smart playlists out of `folder`, or out of every folder.
    pub async fn remove_from_playlist_folder(
        &self,
        members: &[PlaylistFolderMember],
        folder: Option<PlaylistFolderKey>,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        for member in members {
            let (table, column, key) = member_relation(member);
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE {column}=?1 AND (?2 IS NULL OR folder_key=?2)"
            )))
            .bind(key)
            .bind(folder)
            .execute(write.connection().await?)
            .await?;
            stage_member(&mut write, member).await?;
        }
        write.commit().await?;
        Ok(())
    }
    /// Playlist folders whose names hold `filter`, by name, without those every one of
    /// `members` is already in.
    pub fn playlist_folder_choices(
        &self,
        filter: &str,
        members: &[PlaylistFolderMember],
    ) -> Vec<(PlaylistFolderKey, String)> {
        let holds = crate::playlists::name_holds(filter);
        let held = members
            .iter()
            .map(|member| self.playlist_member_folders(member))
            .collect::<Vec<_>>();
        let snapshot = self.snapshot();
        let mut folders = snapshot
            .playlist_folders
            .values()
            .filter(|folder| {
                holds(&folder.name)
                    && (held.is_empty()
                        || !held.iter().all(|folders| {
                            folders.contains(&PlaylistFolderKey::from_raw(folder.key))
                        }))
            })
            .map(|folder| (folder.name.to_lowercase(), folder.key, folder.name.clone()))
            .collect::<Vec<_>>();
        folders.sort();
        folders
            .into_iter()
            .map(|(_, key, name)| (PlaylistFolderKey::from_raw(key), name))
            .collect()
    }
}

/// A playlist folder by object id.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistFolderCopy {
    pub object_id: String,
    pub name: String,
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
            })
            .collect()
    }

    /// Creates or renames playlist folders by object id, in one Write.
    pub async fn put_playlist_folders(&self, folders: &[PlaylistFolderCopy]) -> LibraryResult<()> {
        let folders = folders
            .iter()
            .map(|folder| Ok((&folder.object_id, folder_name(&folder.name)?)))
            .collect::<LibraryResult<Vec<_>>>()?;
        if folders.is_empty() {
            return Ok(());
        }
        let mut write = self.write().await?;
        let rows: Vec<FolderRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO playlist_folders(object_id,name)
             SELECT value->>0,value->>1 FROM json_each(?1) WHERE true
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
}

/// A line of a backup's folder file. `smart` and `cover` are written for older Rufin, which
/// needs them; a folder holds both kinds and its cover is its first member's.
#[derive(serde::Serialize, serde::Deserialize)]
enum FolderRecord {
    Folder {
        id: String,
        name: String,
        #[serde(default)]
        smart: bool,
    },
    Playlist {
        folder: String,
        source: Option<String>,
        id: String,
        #[serde(default)]
        cover: bool,
    },
    SmartPlaylist {
        folder: String,
        id: String,
        #[serde(default)]
        cover: bool,
    },
}
fn write_record(output: &mut impl std::io::Write, record: &FolderRecord) -> LibraryResult<()> {
    serde_json::to_writer(&mut *output, record)?;
    output.write_all(b"\n")?;
    Ok(())
}
pub(crate) async fn export_on(
    connection: &mut SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<()> {
    use futures_util::TryStreamExt;
    let mut folders = sqlx::query_as::<_, (String, String)>(
        "SELECT object_id,name FROM playlist_folders ORDER BY folder_key",
    )
    .fetch(&mut *connection);
    while let Some((id, name)) = folders.try_next().await? {
        write_record(
            &mut output,
            &FolderRecord::Folder {
                id,
                name,
                smart: false,
            },
        )?;
    }
    drop(folders);
    let mut members = sqlx::query_as::<_, (String, Option<String>, String)>("SELECT folder.object_id,source.object_id,playlist.object_id FROM playlist_folder_playlists member JOIN playlist_folders folder USING(folder_key) JOIN playlists playlist USING(playlist_key) LEFT JOIN sources source USING(source_key) ORDER BY member.folder_key,member.playlist_key").fetch(&mut *connection);
    while let Some((folder, source, id)) = members.try_next().await? {
        write_record(
            &mut output,
            &FolderRecord::Playlist {
                folder,
                source,
                id,
                cover: false,
            },
        )?;
    }
    drop(members);
    let mut members = sqlx::query_as::<_, (String, String)>("SELECT folder.object_id,playlist.object_id FROM playlist_folder_smart_playlists member JOIN playlist_folders folder USING(folder_key) JOIN smart_playlists playlist USING(smart_playlist_key) ORDER BY member.folder_key,member.smart_playlist_key").fetch(&mut *connection);
    while let Some((folder, id)) = members.try_next().await? {
        write_record(
            &mut output,
            &FolderRecord::SmartPlaylist {
                folder,
                id,
                cover: false,
            },
        )?;
    }
    Ok(())
}
pub(crate) async fn import_on(
    connection: &mut SqliteConnection,
    input: impl std::io::BufRead,
) -> LibraryResult<()> {
    for line in input.lines() {
        match serde_json::from_str::<FolderRecord>(&line?)? {
            FolderRecord::Folder { id, name, .. } => {
                sqlx::query("INSERT INTO playlist_folders(object_id,name) VALUES(?1,?2)")
                    .bind(id)
                    .bind(name)
                    .execute(&mut *connection)
                    .await?;
            }
            FolderRecord::Playlist {
                folder, source, id, ..
            } => {
                sqlx::query("INSERT OR IGNORE INTO playlist_folder_playlists(playlist_key,folder_key) SELECT playlist.playlist_key,folder.folder_key FROM playlists playlist JOIN playlist_folders folder ON folder.object_id=?3 WHERE playlist.object_id=?1 AND (playlist.owner='rufin' OR playlist.source_key IS(SELECT source_key FROM sources WHERE object_id=?2))").bind(&id).bind(&source).bind(&folder).execute(&mut *connection).await?;
            }
            FolderRecord::SmartPlaylist { folder, id, .. } => {
                sqlx::query("INSERT OR IGNORE INTO playlist_folder_smart_playlists(smart_playlist_key,folder_key) SELECT playlist.smart_playlist_key,folder.folder_key FROM smart_playlists playlist JOIN playlist_folders folder ON folder.object_id=?2 WHERE playlist.object_id=?1").bind(&id).bind(&folder).execute(&mut *connection).await?;
            }
        }
    }
    Ok(())
}
