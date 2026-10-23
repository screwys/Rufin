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

fn folder_name(name: &str) -> LibraryResult<&str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LibraryError::InvalidRequest(
            "Folder name cannot be empty".into(),
        ));
    }
    Ok(name)
}
impl Database {
    pub async fn create_playlist_folder(
        &self,
        name: &str,
        smart: bool,
    ) -> LibraryResult<PlaylistFolderKey> {
        let name = folder_name(name)?;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query_scalar("INSERT INTO playlist_folders(object_id,name,smart) VALUES('rufin:playlist-folder:'||lower(hex(randomblob(16))),?1,?2) RETURNING folder_key").bind(name).bind(smart).fetch_one(connection).await?)
    }
    pub async fn rename_playlist_folder(
        &self,
        key: PlaylistFolderKey,
        name: &str,
    ) -> LibraryResult<()> {
        let name = folder_name(name)?;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("UPDATE playlist_folders SET name=?2 WHERE folder_key=?1")
            .bind(key)
            .bind(name)
            .execute(connection)
            .await?;
        Ok(())
    }
    pub async fn delete_playlist_folder(
        &self,
        key: PlaylistFolderKey,
    ) -> LibraryResult<Option<(String, bool)>> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query_as(
            "DELETE FROM playlist_folders WHERE folder_key=?1 RETURNING object_id,smart",
        )
        .bind(key)
        .fetch_optional(connection)
        .await?)
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
        let smart = matches!(member, PlaylistFolderMember::SmartPlaylist { .. });
        let (table, column, key) = member_relation(&member);
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
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
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let namespace: Option<bool> =
            sqlx::query_scalar("SELECT smart FROM playlist_folders WHERE object_id=?1")
                .bind(id)
                .fetch_optional(&mut *connection)
                .await?;
        let Some(smart) = namespace else {
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
        Ok(load_folders_on(
            &mut connection,
            &read,
            &format!("folder.object_id={}", crate::query::quote(id)),
        )
        .await?
        .pop())
    }
}

async fn child_query_on(
    connection: &mut SqliteConnection,
    read: &ListRead,
) -> LibraryResult<crate::query::OrderedQuery> {
    if let ListKind::SmartPlaylists { now } = read.kind {
        crate::smart_playlists::smart_list_query(connection, read, now).await
    } else {
        Ok(crate::playlists::playlist_list_query(
            read.source,
            read.folder,
            read.sort,
            read.descending,
            &read.filter,
        ))
    }
}
fn child_relation(read: &ListRead, query: &crate::query::OrderedQuery) -> String {
    let smart = matches!(read.kind, ListKind::SmartPlaylists { .. });
    let key = if smart {
        "smart_playlist_key"
    } else {
        "playlist_key"
    };
    let name = if smart {
        "playlist.normalized_name"
    } else {
        "playlist.sort_text"
    };
    let totals = if smart { "playlist" } else { "totals" };
    let stats = if smart && matches!(read.sort, CatalogSort::Position | CatalogSort::Title) {
        "0 track_count,0 duration_millis,0 downloaded_count".into()
    } else {
        format!(
            "coalesce({totals}.track_count,0) track_count,coalesce({totals}.duration_millis,0) duration_millis,coalesce({totals}.downloaded_count,0) downloaded_count"
        )
    };
    format!(
        "SELECT playlist.{key} item_key,playlist.name name,{name} sort_text,playlist.position,{stats},playlist.playlist_folder_key folder_key FROM {} WHERE {}",
        query.from, query.predicate
    )
}
fn item_query(read: &ListRead, children: Option<String>) -> crate::query::OrderedQuery {
    let smart = matches!(read.kind, ListKind::SmartPlaylists { .. });
    let scope = match read.playlist_scope {
        PlaylistScope::Folder { key } => format!("member.folder_key={}", key.raw()),
        _ => "member.folder_key IS NULL".into(),
    };
    let mut items = Vec::new();
    if read.playlist_scope != PlaylistScope::Folders {
        items.push(format!("SELECT member.item_key,member.name,member.sort_text,member.position,member.track_count,member.duration_millis,member.downloaded_count FROM children member WHERE {scope}"));
    }
    if matches!(
        read.playlist_scope,
        PlaylistScope::Root | PlaylistScope::Folders
    ) {
        items.push(if matches!(read.sort, CatalogSort::TrackCount | CatalogSort::Duration) {
            format!("SELECT -folder.folder_key item_key,folder.name,lower(folder.name) sort_text,folder.folder_key position,coalesce(sum(member.track_count),0) track_count,coalesce(sum(member.duration_millis),0) duration_millis,coalesce(sum(member.downloaded_count),0) downloaded_count FROM playlist_folders folder LEFT JOIN children member ON member.folder_key=folder.folder_key WHERE folder.smart={} GROUP BY folder.folder_key", i32::from(smart))
        } else {
            format!("SELECT -folder_key item_key,name,lower(name) sort_text,folder_key position,0 track_count,0 duration_millis,0 downloaded_count FROM playlist_folders WHERE smart={}", i32::from(smart))
        });
    }
    let children = children.map_or(String::new(), |children| {
        let materialized =
            if smart && matches!(read.sort, CatalogSort::TrackCount | CatalogSort::Duration) {
                "MATERIALIZED "
            } else {
                ""
            };
        format!("WITH children AS {materialized}({children}) ")
    });
    let field = match read.sort {
        CatalogSort::Position => "item.position",
        CatalogSort::TrackCount => "item.track_count",
        CatalogSort::Duration => "item.duration_millis",
        _ => "item.sort_text",
    };
    crate::query::OrderedQuery {
        key: "item.item_key".into(),
        text: Some("item.sort_text".into()),
        from: format!("({children}{}) item", items.join(" UNION ALL ")),
        predicate: format!(
            "instr(lower(item.name),{})>0",
            crate::query::quote(&read.filter.trim().to_lowercase())
        ),
        order: vec![format!(
            "{field} {},item.sort_text,item.item_key",
            if read.descending { "DESC" } else { "ASC" }
        )],
        entry_key: "NULL".into(),
        arguments: Default::default(),
    }
}

async fn load_folders_on(
    connection: &mut SqliteConnection,
    read: &ListRead,
    predicate: &str,
) -> LibraryResult<Vec<PlaylistFolderRow>> {
    if let ListKind::SmartPlaylists { now } = read.kind {
        let requested = crate::query::RequestedKeys {
            sql: format!(
                "SELECT -folder.folder_key value,row_number() OVER(ORDER BY lower(folder.name),folder.folder_key) ordinal FROM playlist_folders folder WHERE {predicate}"
            ),
            arguments: Default::default(),
        };
        return Ok(
            crate::smart_playlists::load_smart_items_on(connection, requested, now, false)
                .await?
                .into_iter()
                .filter_map(|row| match row {
                    PlaylistListRow::Folder(row) => Some(row),
                    _ => None,
                })
                .collect(),
        );
    }
    #[derive(FromRow)]
    struct Projection {
        #[sqlx(flatten)]
        folder: PlaylistFolderRow,
        source_id: Option<String>,
        cover_object_id: Option<String>,
    }
    let mut child_read = read.clone();
    child_read.filter.clear();
    child_read.sort = CatalogSort::TrackCount;
    let mut query = child_query_on(connection, &child_read).await?;
    if read.playlist_scope == PlaylistScope::All {
        query.predicate =
            "playlist.name IS NOT NULL AND (playlist.owner='rufin' OR playlist.indexed)".into();
    }
    let children = child_relation(&child_read, &query);
    let binding = crate::artwork::preview_sql("regular_cover.artwork_binding");
    let rows: Vec<Projection> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT folder.folder_key,folder.object_id,folder.name,folder.smart,{binding} artwork_binding,coalesce(sum(child.track_count),0) track_count,coalesce(sum(child.duration_millis),0) duration_millis,source.object_id source_id,regular_cover.object_id cover_object_id FROM playlist_folders folder LEFT JOIN ({children}) child ON child.folder_key=folder.folder_key LEFT JOIN playlists regular_cover ON regular_cover.playlist_key=folder.playlist_cover_key LEFT JOIN sources source ON source.source_key=regular_cover.source_key WHERE {predicate} GROUP BY folder.folder_key ORDER BY lower(folder.name),folder.folder_key"
    ))).fetch_all(connection).await?;
    rows.into_iter()
        .map(|mut row| {
            row.folder.artwork_binding = row
                .cover_object_id
                .map(|id| {
                    crate::artwork::playlist_artwork(
                        row.folder.artwork_binding.take(),
                        (false, row.source_id, id),
                        (None, None),
                    )
                })
                .transpose()?;
            Ok(row.folder)
        })
        .collect()
}

async fn has_folders_on(connection: &mut SqliteConnection, read: &ListRead) -> LibraryResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM playlist_folders WHERE smart=?1)")
            .bind(matches!(read.kind, ListKind::SmartPlaylists { .. }))
            .fetch_one(connection)
            .await?,
    )
}

pub(crate) async fn playlist_query_on(
    connection: &mut SqliteConnection,
    read: &ListRead,
) -> LibraryResult<crate::query::OrderedQuery> {
    if read.playlist_scope == PlaylistScope::All
        || read.playlist_scope == PlaylistScope::Root && !has_folders_on(connection, read).await?
    {
        return child_query_on(connection, read).await;
    }
    if read.playlist_scope == PlaylistScope::Folders
        && matches!(read.sort, CatalogSort::Position | CatalogSort::Title)
    {
        return Ok(item_query(read, None));
    }
    let mut children_read = read.clone();
    children_read.filter.clear();
    let children = child_query_on(connection, &children_read).await?;
    Ok(item_query(
        read,
        Some(child_relation(&children_read, &children)),
    ))
}

pub(crate) async fn load_items_on(
    connection: &mut SqliteConnection,
    read: &ListRead,
    requested: crate::query::RequestedKeys,
) -> LibraryResult<Vec<PlaylistListRow>> {
    if let ListKind::SmartPlaylists { now } = read.kind {
        return crate::smart_playlists::load_smart_items_on(
            connection,
            requested,
            now,
            matches!(read.sort, CatalogSort::TrackCount | CatalogSort::Duration),
        )
        .await;
    }
    if read.playlist_scope == PlaylistScope::All {
        return Ok(Database::load_playlist_requested(connection, requested)
            .await?
            .into_iter()
            .map(PlaylistListRow::Playlist)
            .collect());
    }
    let keys: Vec<i64> =
        sqlx::query_scalar_with(sqlx::AssertSqlSafe(requested.sql), requested.arguments)
            .fetch_all(&mut *connection)
            .await?;
    let folder_keys = keys
        .iter()
        .filter(|key| **key < 0)
        .map(|key| -key)
        .collect::<Vec<_>>();
    let playlist_keys = keys
        .iter()
        .copied()
        .filter(|key| *key >= 0)
        .collect::<Vec<_>>();
    let mut loaded = std::collections::HashMap::new();
    if !folder_keys.is_empty() {
        let predicate = format!(
            "folder.folder_key IN(SELECT value FROM json_each({}))",
            crate::query::quote(&serde_json::to_string(&folder_keys)?)
        );
        for row in load_folders_on(connection, read, &predicate).await? {
            loaded.insert(-row.folder_key.raw(), PlaylistListRow::Folder(row));
        }
    }
    for row in Database::load_playlist_requested(
        connection,
        crate::query::RequestedKeys::batch(&playlist_keys)?,
    )
    .await?
    {
        loaded.insert(row.playlist_key.raw(), PlaylistListRow::Playlist(row));
    }
    Ok(keys
        .into_iter()
        .filter_map(|key| loaded.remove(&key))
        .collect())
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
    let mut members = sqlx::query_as::<_, (String, Option<String>, String, bool)>("SELECT folder.object_id,source.object_id,playlist.object_id,coalesce(folder.playlist_cover_key=playlist.playlist_key,0) FROM playlists playlist JOIN playlist_folders folder ON folder.folder_key=playlist.playlist_folder_key LEFT JOIN sources source USING(source_key) ORDER BY folder.folder_key,playlist.playlist_key").fetch(&mut *connection);
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
    let mut members = sqlx::query_as::<_, (String, String, bool)>("SELECT folder.object_id,playlist.object_id,coalesce(folder.smart_cover_key=playlist.smart_playlist_key,0) FROM smart_playlists playlist JOIN playlist_folders folder ON folder.folder_key=playlist.playlist_folder_key ORDER BY folder.folder_key,playlist.smart_playlist_key").fetch(&mut *connection);
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
            FolderRecord::Playlist {
                folder,
                source,
                id,
                cover,
            } => {
                sqlx::query("UPDATE playlists SET playlist_folder_key=(SELECT folder_key FROM playlist_folders WHERE object_id=?3 AND NOT smart) WHERE object_id=?1 AND source_key IS(SELECT source_key FROM sources WHERE object_id=?2)").bind(&id).bind(&source).bind(&folder).execute(&mut *connection).await?;
                if cover {
                    sqlx::query("UPDATE playlist_folders SET playlist_cover_key=(SELECT playlist_key FROM playlists WHERE object_id=?1 AND source_key IS(SELECT source_key FROM sources WHERE object_id=?2)) WHERE object_id=?3").bind(id).bind(source).bind(folder).execute(&mut *connection).await?;
                }
            }
            FolderRecord::SmartPlaylist { folder, id, cover } => {
                sqlx::query("UPDATE smart_playlists SET playlist_folder_key=(SELECT folder_key FROM playlist_folders WHERE object_id=?2 AND smart) WHERE object_id=?1").bind(&id).bind(&folder).execute(&mut *connection).await?;
                if cover {
                    sqlx::query("UPDATE playlist_folders SET smart_cover_key=(SELECT smart_playlist_key FROM smart_playlists WHERE object_id=?1) WHERE object_id=?2").bind(id).bind(folder).execute(&mut *connection).await?;
                }
            }
        }
    }
    Ok(())
}
