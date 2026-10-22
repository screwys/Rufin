//! Library-owned groups for regular and smart playlists.
use crate::*;
use sqlx::{Connection, FromRow, SqliteConnection};

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
    pub async fn playlist_destination_folders(
        &self,
        source: Option<SourceKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(PlaylistKey, PlaylistFolderKey)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_as("SELECT member.playlist_key,member.folder_key FROM playlist_folder_members member JOIN playlists playlist USING(playlist_key) WHERE playlist.writable AND playlist.name IS NOT NULL AND (playlist.owner='rufin' OR playlist.indexed) AND (playlist.source_key IS NULL OR playlist.source_key=?1)")
            .bind(source)
            .fetch_all(&mut *connection)
            .await?)
    }
    pub async fn playlist_member_folder(
        &self,
        member: &PlaylistFolderMember,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistFolderKey>> {
        let (table, column, key) = match member {
            PlaylistFolderMember::Playlist { key } => {
                ("playlist_folder_members", "playlist_key", key.raw())
            }
            PlaylistFolderMember::SmartPlaylist { key } => (
                "smart_playlist_folder_members",
                "smart_playlist_key",
                key.raw(),
            ),
        };
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT folder_key FROM {table} WHERE {column}=?1"
        )))
        .bind(key)
        .fetch_optional(&mut *connection)
        .await?)
    }
    pub async fn move_playlist_to_folder(
        &self,
        member: PlaylistFolderMember,
        folder: Option<PlaylistFolderKey>,
    ) -> LibraryResult<()> {
        let (table, column, key, smart) = match member {
            PlaylistFolderMember::Playlist { key } => {
                ("playlist_folder_members", "playlist_key", key.raw(), false)
            }
            PlaylistFolderMember::SmartPlaylist { key } => (
                "smart_playlist_folder_members",
                "smart_playlist_key",
                key.raw(),
                true,
            ),
        };
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if let Some(folder) = folder {
            let namespace: Option<bool> =
                sqlx::query_scalar("SELECT smart FROM playlist_folders WHERE folder_key=?1")
                    .bind(folder)
                    .fetch_optional(&mut *transaction)
                    .await?;
            if namespace != Some(smart) {
                return Err(LibraryError::InvalidRequest(
                    "Playlist folder does not match the playlist kind".into(),
                ));
            }
            sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO {table}({column},folder_key) VALUES(?1,?2) ON CONFLICT({column}) DO UPDATE SET folder_key=excluded.folder_key WHERE folder_key<>excluded.folder_key"))).bind(key).bind(folder).execute(&mut *transaction).await?;
        } else {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE {column}=?1"
            )))
            .bind(key)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }
    pub async fn playlist_folders(
        &self,
        smart: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistFolderRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let read = ListRead::new(
            if smart {
                ListKind::SmartPlaylists {
                    now: chrono::Utc::now().timestamp(),
                }
            } else {
                ListKind::Playlists
            },
            None,
            CatalogSort::Title,
        );
        let mut children = child_query_on(&mut connection, &read).await?;
        if !smart {
            children.predicate =
                "playlist.name IS NOT NULL AND (playlist.owner='rufin' OR playlist.indexed)".into();
        }
        load_folders_on(
            &mut connection,
            smart,
            &children,
            &format!("folder.smart={}", i32::from(smart)),
        )
        .await
    }
    pub async fn playlist_folder(
        &self,
        key: PlaylistFolderKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistFolderRow>> {
        self.playlist_folder_lookup("folder_key", &key.raw().to_string(), cancellation)
            .await
    }
    pub async fn playlist_folder_by_id(
        &self,
        id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistFolderRow>> {
        self.playlist_folder_lookup("object_id", id, cancellation)
            .await
    }
    async fn playlist_folder_lookup(
        &self,
        column: &str,
        value: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistFolderRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let namespace: Option<bool> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT smart FROM playlist_folders WHERE {column}=?1"
        )))
        .bind(value)
        .fetch_optional(&mut *connection)
        .await?;
        let Some(smart) = namespace else {
            return Ok(None);
        };
        let read = ListRead::new(
            if smart {
                ListKind::SmartPlaylists {
                    now: chrono::Utc::now().timestamp(),
                }
            } else {
                ListKind::Playlists
            },
            None,
            CatalogSort::Title,
        );
        let mut children = child_query_on(&mut connection, &read).await?;
        if !smart {
            children.predicate =
                "playlist.name IS NOT NULL AND (playlist.owner='rufin' OR playlist.indexed)".into();
        }
        Ok(load_folders_on(
            &mut connection,
            smart,
            &children,
            &format!("folder.{column}={}", crate::query::quote(value)),
        )
        .await?
        .pop())
    }
}

async fn child_query_on(
    connection: &mut SqliteConnection,
    read: &ListRead,
) -> LibraryResult<crate::query::OrderedQuery> {
    match read.kind {
        ListKind::Playlists => Ok(crate::playlists::playlist_list_query(
            read.source,
            read.folder,
            CatalogSort::TrackCount,
            false,
            "",
        )),
        ListKind::SmartPlaylists { now } => {
            crate::smart_playlists::smart_list_query(
                connection,
                read.source,
                read.folder,
                CatalogSort::TrackCount,
                false,
                "",
                now,
            )
            .await
        }
        _ => Err(LibraryError::InvalidRequest(
            "Expected a playlist list".into(),
        )),
    }
}
fn child_relation(smart: bool, query: &crate::query::OrderedQuery) -> String {
    let key = if smart {
        "smart_playlist_key"
    } else {
        "playlist_key"
    };
    let table = if smart {
        "smart_playlist_folder_members"
    } else {
        "playlist_folder_members"
    };
    let name = if smart {
        "playlist.normalized_name"
    } else {
        "playlist.sort_text"
    };
    let display_name = if smart {
        "(SELECT name FROM smart_playlists parent WHERE parent.smart_playlist_key=playlist.smart_playlist_key)"
    } else {
        "playlist.name"
    };
    let totals = if smart { "playlist" } else { "totals" };
    format!(
        "SELECT playlist.{key} item_key,{display_name} name,{name} sort_text,playlist.position,coalesce({totals}.track_count,0) track_count,coalesce({totals}.duration_millis,0) duration_millis,member.folder_key FROM {} LEFT JOIN {table} member ON member.{key}=playlist.{key} WHERE {}",
        query.from, query.predicate
    )
}
fn item_query(read: &ListRead, children: String) -> crate::query::OrderedQuery {
    let smart = matches!(read.kind, ListKind::SmartPlaylists { .. });
    let scope = read
        .playlist_folder
        .map_or("member.folder_key IS NULL".into(), |key| {
            format!("member.folder_key={}", key.raw())
        });
    let folders = if read.playlist_folder.is_none() {
        format!(
            " UNION ALL SELECT -folder.folder_key,folder.name,lower(folder.name),folder.folder_key,coalesce(sum(member.track_count),0),coalesce(sum(member.duration_millis),0) FROM playlist_folders folder LEFT JOIN children member ON member.folder_key=folder.folder_key WHERE folder.smart={} GROUP BY folder.folder_key",
            i32::from(smart)
        )
    } else {
        String::new()
    };
    let field = match read.sort {
        CatalogSort::Position => "item.position",
        CatalogSort::TrackCount => "item.track_count",
        CatalogSort::Duration => "item.duration_millis",
        _ => "item.sort_text",
    };
    crate::query::OrderedQuery {
        key: "item.item_key".into(),
        text: Some("item.sort_text".into()),
        from: format!(
            "(WITH children AS ({children}) SELECT member.item_key,member.name,member.sort_text,member.position,member.track_count,member.duration_millis FROM children member WHERE {scope}{folders}) item"
        ),
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
    smart: bool,
    children: &crate::query::OrderedQuery,
    predicate: &str,
) -> LibraryResult<Vec<PlaylistFolderRow>> {
    #[derive(FromRow)]
    struct Projection {
        #[sqlx(flatten)]
        folder: PlaylistFolderRow,
        source_id: Option<String>,
        cover_object_id: Option<String>,
    }
    let relation = child_relation(smart, children);
    let binding = crate::artwork::preview_sql(
        "CASE WHEN folder.smart THEN smart_cover.artwork_binding ELSE regular_cover.artwork_binding END",
    );
    let rows: Vec<Projection> = sqlx::query_as(sqlx::AssertSqlSafe(format!("WITH children AS ({relation}) SELECT folder.folder_key,folder.object_id,folder.name,folder.smart,{binding} artwork_binding,coalesce(sum(child.track_count),0) track_count,coalesce(sum(child.duration_millis),0) duration_millis,source.object_id source_id,coalesce(smart_cover.object_id,regular_cover.object_id) cover_object_id FROM playlist_folders folder LEFT JOIN children child ON child.folder_key=folder.folder_key LEFT JOIN playlists regular_cover ON regular_cover.playlist_key=folder.playlist_cover_key LEFT JOIN sources source ON source.source_key=regular_cover.source_key LEFT JOIN smart_playlists smart_cover ON smart_cover.smart_playlist_key=folder.smart_cover_key WHERE {predicate} GROUP BY folder.folder_key ORDER BY lower(folder.name),folder.folder_key"))).fetch_all(&mut *connection).await?;
    rows.into_iter()
        .map(|mut row| {
            row.folder.artwork_binding = row
                .cover_object_id
                .map(|id| {
                    crate::artwork::playlist_artwork(
                        row.folder.artwork_binding.take(),
                        (smart, row.source_id, id),
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

pub(crate) async fn playlist_item_query_on(
    connection: &mut SqliteConnection,
    read: &ListRead,
) -> LibraryResult<crate::query::OrderedQuery> {
    if read.playlist_folder.is_none() && !has_folders_on(connection, read).await? {
        return crate::query::list_query_on(connection, read).await;
    }
    let children = child_query_on(connection, read).await?;
    Ok(item_query(
        read,
        child_relation(
            matches!(read.kind, ListKind::SmartPlaylists { .. }),
            &children,
        ),
    ))
}

impl ListRead {
    pub fn playlist_items_selection(self) -> MemberSelection {
        MemberSelection::PlaylistItems {
            read: self,
            ranges: None,
        }
    }
    pub async fn playlist_items_count(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<usize> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        Ok(playlist_item_query_on(&mut connection, self)
            .await?
            .count(&mut connection)
            .await? as usize)
    }
    pub async fn playlist_items_sections(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ScrollSection>> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        playlist_item_query_on(&mut connection, self)
            .await?
            .sections(&mut connection)
            .await
    }
    pub async fn playlist_items(
        &self,
        database: &Database,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistListRow>> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        if self.playlist_folder.is_none() && !has_folders_on(&mut connection, self).await? {
            drop(connection);
            drop(_permit);
            return match self.kind {
                ListKind::SmartPlaylists { .. } => Ok(self
                    .smart_playlists(database, offset, limit, cancellation)
                    .await?
                    .into_iter()
                    .map(PlaylistListRow::SmartPlaylist)
                    .collect()),
                _ => Ok(self
                    .playlists(database, offset, limit, cancellation)
                    .await?
                    .into_iter()
                    .map(PlaylistListRow::Playlist)
                    .collect()),
            };
        }
        let mut transaction = connection.begin().await?;
        let children = child_query_on(&mut transaction, self).await?;
        let query = item_query(
            self,
            child_relation(
                matches!(self.kind, ListKind::SmartPlaylists { .. }),
                &children,
            ),
        );
        let rows = load_items_on(
            &mut transaction,
            self,
            &children,
            &query,
            offset,
            limit.min(256),
        )
        .await?;
        transaction.commit().await?;
        Ok(rows)
    }
    pub async fn page_playlist_items(
        &self,
        database: &Database,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Page<PlaylistListRow>> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        if self.playlist_folder.is_none() && !has_folders_on(&mut connection, self).await? {
            drop(connection);
            drop(_permit);
            return match self.kind {
                ListKind::SmartPlaylists { .. } => {
                    let page = self
                        .page_smart_playlists(database, window, cancellation)
                        .await?;
                    Ok(Page {
                        count: page.count,
                        first_row_position: page.first_row_position,
                        rows: page
                            .rows
                            .into_iter()
                            .map(PlaylistListRow::SmartPlaylist)
                            .collect(),
                    })
                }
                _ => {
                    let page = self.page_playlists(database, window, cancellation).await?;
                    Ok(Page {
                        count: page.count,
                        first_row_position: page.first_row_position,
                        rows: page
                            .rows
                            .into_iter()
                            .map(PlaylistListRow::Playlist)
                            .collect(),
                    })
                }
            };
        }
        let mut transaction = connection.begin().await?;
        let children = child_query_on(&mut transaction, self).await?;
        let query = item_query(
            self,
            child_relation(
                matches!(self.kind, ListKind::SmartPlaylists { .. }),
                &children,
            ),
        );
        let count = query.count(&mut transaction).await? as usize;
        let range = window.range(count);
        let rows = load_items_on(
            &mut transaction,
            self,
            &children,
            &query,
            range.start,
            range.len(),
        )
        .await?;
        transaction.commit().await?;
        Ok(Page {
            count,
            first_row_position: range.start,
            rows,
        })
    }
}
async fn load_items_on(
    connection: &mut SqliteConnection,
    read: &ListRead,
    children: &crate::query::OrderedQuery,
    query: &crate::query::OrderedQuery,
    offset: usize,
    limit: usize,
) -> LibraryResult<Vec<PlaylistListRow>> {
    let requested = query.requested_keys(offset, limit);
    let keys: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(requested.sql))
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
        for row in load_folders_on(
            connection,
            matches!(read.kind, ListKind::SmartPlaylists { .. }),
            children,
            &predicate,
        )
        .await?
        {
            loaded.insert(-row.folder_key.raw(), PlaylistListRow::Folder(row));
        }
    }
    if let ListKind::SmartPlaylists { now } = read.kind {
        for row in crate::smart_playlists::load_smart_playlist_requested(
            connection,
            read.source,
            crate::query::RequestedKeys::batch(&playlist_keys)?,
            read.folder,
            now,
        )
        .await?
        {
            loaded.insert(
                row.smart_playlist_key.raw(),
                PlaylistListRow::SmartPlaylist(row),
            );
        }
    } else {
        for row in Database::load_playlist_requested(
            connection,
            crate::query::RequestedKeys::batch(&playlist_keys)?,
        )
        .await?
        {
            loaded.insert(row.playlist_key.raw(), PlaylistListRow::Playlist(row));
        }
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
    let mut members = sqlx::query_as::<_, (String, Option<String>, String, bool)>("SELECT folder.object_id,source.object_id,playlist.object_id,coalesce(folder.playlist_cover_key=playlist.playlist_key,0) FROM playlist_folder_members member JOIN playlist_folders folder USING(folder_key) JOIN playlists playlist USING(playlist_key) LEFT JOIN sources source USING(source_key) ORDER BY folder.folder_key,playlist.playlist_key").fetch(&mut *connection);
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
    let mut members = sqlx::query_as::<_, (String, String, bool)>("SELECT folder.object_id,playlist.object_id,coalesce(folder.smart_cover_key=playlist.smart_playlist_key,0) FROM smart_playlist_folder_members member JOIN playlist_folders folder USING(folder_key) JOIN smart_playlists playlist USING(smart_playlist_key) ORDER BY folder.folder_key,playlist.smart_playlist_key").fetch(&mut *connection);
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
                sqlx::query("INSERT INTO playlist_folder_members(playlist_key,folder_key) SELECT playlist.playlist_key,folder.folder_key FROM playlists playlist,playlist_folders folder WHERE playlist.object_id=?1 AND playlist.source_key IS(SELECT source_key FROM sources WHERE object_id=?2) AND folder.object_id=?3 AND NOT folder.smart").bind(&id).bind(&source).bind(&folder).execute(&mut *connection).await?;
                if cover {
                    sqlx::query("UPDATE playlist_folders SET playlist_cover_key=(SELECT playlist_key FROM playlists WHERE object_id=?1 AND source_key IS(SELECT source_key FROM sources WHERE object_id=?2)) WHERE object_id=?3").bind(id).bind(source).bind(folder).execute(&mut *connection).await?;
                }
            }
            FolderRecord::SmartPlaylist { folder, id, cover } => {
                sqlx::query("INSERT INTO smart_playlist_folder_members(smart_playlist_key,folder_key) SELECT playlist.smart_playlist_key,folder.folder_key FROM smart_playlists playlist,playlist_folders folder WHERE playlist.object_id=?1 AND folder.object_id=?2 AND folder.smart").bind(&id).bind(&folder).execute(&mut *connection).await?;
                if cover {
                    sqlx::query("UPDATE playlist_folders SET smart_cover_key=(SELECT smart_playlist_key FROM smart_playlists WHERE object_id=?1) WHERE object_id=?2").bind(id).bind(folder).execute(&mut *connection).await?;
                }
            }
        }
    }
    Ok(())
}
