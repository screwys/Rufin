//! Library-owned groups for regular and smart playlists.
use crate::*;
use sqlx::{FromRow, SqliteConnection};

#[derive(Clone, Debug, PartialEq, FromRow)]
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
    connection: &mut SqliteConnection,
    member: PlaylistFolderMember,
    folder: Option<PlaylistFolderKey>,
) -> LibraryResult<()> {
    let smart = matches!(member, PlaylistFolderMember::SmartPlaylist { .. });
    let (table, column, key) = member_relation(&member);
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
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {table} SET playlist_folder_key=?2 WHERE {column}=?1 AND playlist_folder_key IS NOT ?2"
    )))
    .bind(key).bind(folder).execute(connection).await?;
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
        let key = sqlx::query_scalar("INSERT INTO playlist_folders(object_id,name,smart) VALUES('rufin:playlist-folder:'||lower(hex(randomblob(16))),?1,?2) RETURNING folder_key").bind(name).bind(smart).fetch_one(write.connection().await?).await?;
        write.touch_playlist_folders();
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
        sqlx::query("UPDATE playlist_folders SET name=?2 WHERE folder_key=?1")
            .bind(key)
            .bind(name)
            .execute(write.connection().await?)
            .await?;
        write.touch_playlist_folders();
        write.commit().await?;
        Ok(())
    }
    pub async fn delete_playlist_folder(
        &self,
        key: PlaylistFolderKey,
    ) -> LibraryResult<Option<(String, bool)>> {
        let mut write = self.write().await?;
        // Members leave the folder as the Store's foreign keys clear it.
        let playlists: Vec<i64> =
            sqlx::query_scalar("SELECT playlist_key FROM playlists WHERE playlist_folder_key=?1")
                .bind(key)
                .fetch_all(write.connection().await?)
                .await?;
        let smart: Vec<i64> = sqlx::query_scalar(
            "SELECT smart_playlist_key FROM smart_playlists WHERE playlist_folder_key=?1",
        )
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
            write.touch_playlist(playlist);
        }
        for smart in smart {
            write.touch_smart_playlist(smart);
        }
        write.touch_playlist_folders();
        write.commit().await?;
        Ok(deleted)
    }
    pub async fn playlist_member_folder(
        &self,
        member: &PlaylistFolderMember,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistFolderKey>> {
        let (table, column, key) = member_relation(member);
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(
            sqlx::query_scalar::<_, Option<PlaylistFolderKey>>(sqlx::AssertSqlSafe(format!(
                "SELECT playlist_folder_key FROM {table} WHERE {column}=?1"
            )))
            .bind(key)
            .fetch_optional(&mut *connection)
            .await?
            .flatten(),
        )
    }
    pub async fn move_playlist_to_folder(
        &self,
        member: PlaylistFolderMember,
        folder: Option<PlaylistFolderKey>,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        move_to_folder_on(write.connection().await?, member.clone(), folder).await?;
        match member {
            PlaylistFolderMember::Playlist { key } => write.touch_playlist(key.raw()),
            PlaylistFolderMember::SmartPlaylist { key } => write.touch_smart_playlist(key.raw()),
        }
        write.commit().await?;
        Ok(())
    }
    pub async fn playlist_folder_choices(
        &self,
        smart: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(PlaylistFolderKey, String)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_as("SELECT folder_key,name FROM playlist_folders WHERE smart=?1 ORDER BY lower(name),folder_key")
            .bind(smart).fetch_all(&mut *connection).await?)
    }
    pub async fn playlist_folder_by_id(
        &self,
        id: &str,
        source: Option<SourceKey>,
        music_folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistFolderRow>> {
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let folder: Option<(i64, bool)> =
            sqlx::query_as("SELECT folder_key,smart FROM playlist_folders WHERE object_id=?1")
                .bind(id)
                .fetch_optional(&mut *connection)
                .await?;
        let Some((key, smart)) = folder else {
            return Ok(None);
        };
        let mut read = ListRead::new(
            if smart {
                ListKind::SmartPlaylists {
                    now: chrono::Utc::now().timestamp(),
                }
            } else {
                ListKind::Playlists
            },
            source,
            CatalogSort::Title,
        );
        read.folder = music_folder;
        Ok(load_items(self, &mut connection, &read, &[-key])
            .await?
            .into_iter()
            .find_map(|row| match row {
                PlaylistListRow::Folder(row) => Some(row),
                _ => None,
            }))
    }
}

/// Playlist folder rows: totals of the playlists each holds and its first playlist's artwork.
async fn load_folder_rows(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    read: &ListRead,
    folders: &[i64],
) -> LibraryResult<Vec<PlaylistFolderRow>> {
    let totals = {
        let mut children = read.clone();
        children.filter.clear();
        if read.playlist_scope == PlaylistScope::All {
            children.source = None;
        }
        catalog.loaded().await;
        let snapshot = catalog.snapshot();
        crate::catalog::views::Context::new(&snapshot, false).playlist_folder_totals(&children)
    };
    #[derive(FromRow)]
    struct Row {
        #[sqlx(flatten)]
        folder: PlaylistFolderRow,
        source_id: Option<String>,
        cover_object_id: Option<String>,
        mosaic: Option<String>,
    }
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT folder.folder_key,folder.object_id,folder.name,folder.smart,{} artwork_binding,0 track_count,0 duration_millis,source.object_id source_id,cover.object_id cover_object_id,cover.mosaic
         FROM json_each(?1) requested JOIN playlist_folders folder ON folder.folder_key=requested.value AND NOT folder.smart
         LEFT JOIN playlists cover ON cover.playlist_key=(SELECT min(child.playlist_key) FROM playlists child WHERE child.playlist_folder_key=folder.folder_key AND child.name IS NOT NULL AND (child.owner='rufin' OR child.indexed))
         LEFT JOIN sources source ON source.source_key=cover.source_key
         ORDER BY CAST(requested.key AS INTEGER)",
        crate::artwork::preview_sql("cover.artwork_binding")
    )))
    .bind(serde_json::to_string(folders)?)
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|mut row| {
            if let Some(totals) = totals.get(&row.folder.folder_key.raw()) {
                row.folder.track_count = i64::from(totals.tracks);
                row.folder.duration_millis = totals.duration as i64;
            }
            row.folder.artwork_binding = match row.cover_object_id {
                Some(id) => crate::artwork::playlist_artwork(
                    row.folder.artwork_binding.take(),
                    (false, row.source_id, id),
                    row.mosaic.as_deref(),
                )?,
                None => None,
            };
            Ok(row.folder)
        })
        .collect()
}

/// Rows of a playlist list in one key space: playlists by key, folders by negative key.
pub(crate) async fn load_items(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    read: &ListRead,
    keys: &[i64],
) -> LibraryResult<Vec<PlaylistListRow>> {
    let folders = keys
        .iter()
        .filter(|key| **key < 0)
        .map(|key| -key)
        .collect::<Vec<_>>();
    let playlists = keys
        .iter()
        .copied()
        .filter(|key| *key >= 0)
        .collect::<Vec<_>>();
    let mut loaded = std::collections::HashMap::new();
    if let ListKind::SmartPlaylists { now } = read.kind {
        for row in
            crate::smart_playlists::load_smart_folder_rows(catalog, connection, &folders, now)
                .await?
        {
            loaded.insert(-row.folder_key.raw(), PlaylistListRow::Folder(row));
        }
        let keys = playlists
            .iter()
            .copied()
            .map(SmartPlaylistKey::from_raw)
            .collect::<Vec<_>>();
        for row in crate::smart_playlists::load_smart_playlist_rows(catalog, connection, &keys, now)
            .await?
        {
            loaded.insert(
                row.smart_playlist_key.raw(),
                PlaylistListRow::SmartPlaylist(row),
            );
        }
    } else {
        if !folders.is_empty() {
            for row in load_folder_rows(catalog, connection, read, &folders).await? {
                loaded.insert(-row.folder_key.raw(), PlaylistListRow::Folder(row));
            }
        }
        let keys = playlists
            .iter()
            .copied()
            .map(PlaylistKey::from_raw)
            .collect::<Vec<_>>();
        for row in crate::playlists::load_playlist_rows(catalog, connection, &keys).await? {
            loaded.insert(row.playlist_key.raw(), PlaylistListRow::Playlist(row));
        }
    }
    Ok(keys.iter().filter_map(|key| loaded.remove(key)).collect())
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
