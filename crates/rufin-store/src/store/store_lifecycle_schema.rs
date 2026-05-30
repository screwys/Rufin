use super::servers::*;
use super::*;

const SCHEMA_MIGRATIONS: &[SchemaMigration] = &[];
const SCHEMA_VERSION_10_TABLES: &[&str] = &[
    "queue_snapshots",
    "servers",
    "server_local_access",
    "server_music_folders",
    "track_music_folders",
    "track_local_matches",
    "server_library_preferences",
    "active_server",
    "sync_state",
    "albums",
    "tracks",
    "artists",
    "album_artists",
    "genres",
    "playlists",
    "album_genres",
    "track_genres",
    "album_artist_links",
    "track_artist_links",
    "playlist_tracks",
    "home_section_items",
    "home_section_prefetch_items",
    "lyrics_cache",
    "cover_cache",
    "external_image_lookup_misses",
    "library_fts",
];
const SCHEMA_VERSION_10_COLUMNS: &[(&str, &str)] = &[
    ("albums", "image_item_id"),
    ("albums", "image_tag"),
    ("albums", "release_date"),
    ("albums", "date_added"),
    ("albums", "last_played"),
    ("albums", "play_count"),
    ("albums", "user_rating"),
    ("tracks", "image_item_id"),
    ("tracks", "image_tag"),
    ("tracks", "release_date"),
    ("tracks", "date_added"),
    ("tracks", "last_played"),
    ("tracks", "play_count"),
    ("tracks", "user_rating"),
    ("tracks", "local_path"),
    ("artists", "image_item_id"),
    ("artists", "image_tag"),
    ("artists", "last_played"),
    ("artists", "play_count"),
    ("artists", "user_rating"),
    ("album_artists", "image_item_id"),
    ("album_artists", "image_tag"),
    ("album_artists", "last_played"),
    ("album_artists", "play_count"),
    ("album_artists", "user_rating"),
    ("genres", "image_item_id"),
    ("genres", "image_tag"),
    ("playlists", "image_item_id"),
    ("playlists", "image_tag"),
    ("playlist_tracks", "entry_id"),
    ("server_music_folders", "folder_id"),
    ("track_music_folders", "folder_id"),
    ("track_local_matches", "local_path"),
    ("server_library_preferences", "selected_music_folder_id"),
    ("lyrics_cache", "value"),
    ("cover_cache", "path"),
    ("external_image_lookup_misses", "reason"),
];

struct SchemaMigration {
    from_version: i64,
    run: fn(&Store) -> StoreResult<()>,
}

impl SchemaMigration {
    fn to_version(&self) -> i64 {
        self.from_version + 1
    }
}

fn schema_migration_path_from(version: i64) -> Option<Vec<&'static SchemaMigration>> {
    schema_migration_path(version, SCHEMA_VERSION, SCHEMA_MIGRATIONS)
}

fn schema_migration_path(
    mut version: i64,
    target_version: i64,
    migrations: &'static [SchemaMigration],
) -> Option<Vec<&'static SchemaMigration>> {
    if version > target_version {
        return None;
    }
    let mut path = Vec::new();
    while version < target_version {
        let migration = migrations
            .iter()
            .find(|migration| migration.from_version == version)?;
        version = migration.to_version();
        path.push(migration);
    }
    Some(path)
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let path = path.as_ref();
        let mut store = Self::open_file(path)?;
        if store.needs_reset()? {
            drop(store);
            reset_database_files(path)?;
            store = Self::open_file(path)?;
        }
        store.migrate()?;
        Ok(store)
    }
    pub fn open_memory() -> StoreResult<Self> {
        let connection = Connection::open_in_memory()?;
        let store = Self { connection };
        store.configure_pragmas(true)?;
        store.initialize_schema()?;
        Ok(store)
    }
    pub fn migrate(&self) -> StoreResult<()> {
        if !self.database_has_objects()? {
            return self.initialize_schema();
        }
        let version = self.schema_version()?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion(version));
        }
        if version < SCHEMA_VERSION && !self.schema_is_complete_for_version(version)? {
            return Err(StoreError::IncompleteSchemaVersion(version));
        }
        let Some(migrations) = schema_migration_path_from(version) else {
            return Err(StoreError::UnsupportedSchemaVersion(version));
        };
        if !migrations.is_empty() {
            self.connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration_result = (|| {
                for migration in migrations {
                    (migration.run)(self)?;
                    self.connection
                        .pragma_update(None, "user_version", migration.to_version())?;
                }
                Ok(())
            })();
            if let Err(error) = migration_result {
                let _rollback_result = self.connection.execute_batch("ROLLBACK");
                return Err(error);
            }
            self.connection.execute_batch("COMMIT")?;
        }
        self.initialize_schema()
    }
    pub(super) fn open_file(path: &Path) -> StoreResult<Self> {
        let connection = Connection::open(path)?;
        let store = Self { connection };
        store.configure_pragmas(true)?;
        Ok(store)
    }
    pub(super) fn needs_reset(&self) -> StoreResult<bool> {
        if !self.database_has_objects()? {
            return Ok(false);
        }
        let version = self.schema_version()?;
        if version > SCHEMA_VERSION {
            return Ok(true);
        }
        let schema_complete = if version == SCHEMA_VERSION {
            self.current_schema_is_complete()?
        } else {
            self.schema_is_complete_for_version(version)?
        };
        if !schema_complete {
            return Ok(true);
        }
        Ok(schema_migration_path_from(version).is_none())
    }
    pub(super) fn database_has_objects(&self) -> StoreResult<bool> {
        let exists = self.connection.query_row(
            "
            SELECT EXISTS(
                SELECT 1
                FROM sqlite_master
                WHERE name NOT LIKE 'sqlite_%'
            )
            ",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        Ok(exists)
    }
    pub(super) fn current_schema_is_complete(&self) -> StoreResult<bool> {
        self.schema_is_complete_for_version(SCHEMA_VERSION)
    }
    fn schema_is_complete_for_version(&self, version: i64) -> StoreResult<bool> {
        match version {
            10 => {
                self.schema_has_required_parts(SCHEMA_VERSION_10_TABLES, SCHEMA_VERSION_10_COLUMNS)
            }
            _ => Ok(false),
        }
    }
    fn schema_has_required_parts(
        &self,
        tables: &[&str],
        columns: &[(&str, &str)],
    ) -> StoreResult<bool> {
        for table in tables {
            if !self.table_exists(table)? {
                return Ok(false);
            }
        }
        for (table, column) in columns {
            if !self.table_has_column(table, column)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub(super) fn initialize_schema(&self) -> StoreResult<()> {
        self.connection.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS queue_snapshots (
                server_id TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS servers (
                server_id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                name TEXT NOT NULL,
                base_url TEXT NOT NULL,
                user_id TEXT NOT NULL,
                username TEXT NOT NULL,
                trust_invalid_cert INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS server_local_access (
                server_id TEXT PRIMARY KEY REFERENCES servers(server_id) ON DELETE CASCADE,
                root_path TEXT NOT NULL,
                path_replace_from TEXT,
                path_replace_to TEXT,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS server_music_folders (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                folder_id TEXT NOT NULL,
                name TEXT NOT NULL,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, folder_id)
            );
            CREATE TABLE IF NOT EXISTS track_music_folders (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                track_id TEXT NOT NULL,
                folder_id TEXT NOT NULL,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, track_id, folder_id)
            );
            CREATE TABLE IF NOT EXISTS track_local_matches (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                track_id TEXT NOT NULL,
                local_path TEXT NOT NULL,
                match_kind TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (server_id, track_id)
            );
            CREATE TABLE IF NOT EXISTS server_library_preferences (
                server_id TEXT PRIMARY KEY REFERENCES servers(server_id) ON DELETE CASCADE,
                selected_music_folder_id TEXT,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS active_server (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS sync_state (
                server_id TEXT PRIMARY KEY REFERENCES servers(server_id) ON DELETE CASCADE,
                generation INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL DEFAULT 'idle',
                last_started_at TEXT,
                last_completed_at TEXT,
                last_error TEXT
            );
            CREATE TABLE IF NOT EXISTS albums (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                album_id TEXT NOT NULL,
                title TEXT NOT NULL,
                artist TEXT NOT NULL,
                artist_id TEXT,
                year INTEGER NOT NULL,
                release_date TEXT,
                date_added TEXT,
                last_played TEXT,
                play_count INTEGER,
                user_rating INTEGER,
                track_count INTEGER NOT NULL,
                duration_seconds INTEGER NOT NULL,
                favorite INTEGER NOT NULL,
                color_seed INTEGER NOT NULL,
                image_item_id TEXT,
                image_tag TEXT,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, album_id)
            );
            CREATE TABLE IF NOT EXISTS tracks (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                track_id TEXT NOT NULL,
                album_id TEXT NOT NULL,
                title TEXT NOT NULL,
                artist TEXT NOT NULL,
                artist_id TEXT,
                album TEXT NOT NULL,
                year INTEGER NOT NULL,
                release_date TEXT,
                date_added TEXT,
                last_played TEXT,
                play_count INTEGER,
                user_rating INTEGER,
                duration_seconds INTEGER NOT NULL,
                favorite INTEGER NOT NULL,
                disc_number INTEGER NOT NULL,
                track_number INTEGER NOT NULL,
                image_item_id TEXT,
                image_tag TEXT,
                local_path TEXT,
                source_format TEXT,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, track_id)
            );
            CREATE TABLE IF NOT EXISTS artists (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                artist_id TEXT NOT NULL,
                name TEXT NOT NULL,
                album_count INTEGER NOT NULL,
                track_count INTEGER NOT NULL,
                favorite INTEGER NOT NULL,
                last_played TEXT,
                play_count INTEGER,
                user_rating INTEGER,
                image_item_id TEXT,
                image_tag TEXT,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, artist_id)
            );
            CREATE TABLE IF NOT EXISTS album_artists (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                artist_id TEXT NOT NULL,
                name TEXT NOT NULL,
                album_count INTEGER NOT NULL,
                track_count INTEGER NOT NULL,
                favorite INTEGER NOT NULL,
                last_played TEXT,
                play_count INTEGER,
                user_rating INTEGER,
                image_item_id TEXT,
                image_tag TEXT,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, artist_id)
            );
            CREATE TABLE IF NOT EXISTS genres (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                genre_id TEXT NOT NULL,
                name TEXT NOT NULL,
                album_count INTEGER NOT NULL,
                track_count INTEGER NOT NULL,
                image_item_id TEXT,
                image_tag TEXT,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, genre_id)
            );
            CREATE TABLE IF NOT EXISTS playlists (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                playlist_id TEXT NOT NULL,
                name TEXT NOT NULL,
                track_count INTEGER NOT NULL,
                duration_seconds INTEGER NOT NULL,
                image_item_id TEXT,
                image_tag TEXT,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, playlist_id)
            );
            CREATE TABLE IF NOT EXISTS album_genres (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                album_id TEXT NOT NULL,
                genre_name TEXT NOT NULL,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, album_id, genre_name)
            );
            CREATE TABLE IF NOT EXISTS track_genres (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                track_id TEXT NOT NULL,
                genre_name TEXT NOT NULL,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, track_id, genre_name)
            );
            CREATE TABLE IF NOT EXISTS album_artist_links (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                album_id TEXT NOT NULL,
                artist_id TEXT NOT NULL,
                name TEXT NOT NULL,
                position INTEGER NOT NULL,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, album_id, artist_id)
            );
            CREATE TABLE IF NOT EXISTS track_artist_links (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                track_id TEXT NOT NULL,
                album_id TEXT NOT NULL,
                artist_id TEXT NOT NULL,
                name TEXT NOT NULL,
                position INTEGER NOT NULL,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, track_id, artist_id)
            );
            CREATE TABLE IF NOT EXISTS playlist_tracks (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                playlist_id TEXT NOT NULL,
                entry_id TEXT NOT NULL,
                track_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, playlist_id, entry_id)
            );
            CREATE TABLE IF NOT EXISTS home_section_items (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                section_kind TEXT NOT NULL,
                item_type TEXT NOT NULL,
                item_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, section_kind, item_type, item_id)
            );
            CREATE TABLE IF NOT EXISTS home_section_prefetch_items (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                section_kind TEXT NOT NULL,
                item_type TEXT NOT NULL,
                item_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                sync_generation INTEGER NOT NULL,
                PRIMARY KEY (server_id, section_kind, item_type, item_id)
            );
            CREATE TABLE IF NOT EXISTS lyrics_cache (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                track_id TEXT NOT NULL,
                source TEXT NOT NULL,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (server_id, track_id)
            );
            CREATE TABLE IF NOT EXISTS cover_cache (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                item_id TEXT NOT NULL,
                image_tag TEXT NOT NULL,
                size INTEGER NOT NULL,
                path TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (server_id, item_id, image_tag, size)
            );
            CREATE TABLE IF NOT EXISTS external_image_lookup_misses (
                server_id TEXT NOT NULL REFERENCES servers(server_id) ON DELETE CASCADE,
                item_id TEXT NOT NULL,
                image_tag TEXT NOT NULL,
                size INTEGER NOT NULL,
                reason TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (server_id, item_id, image_tag, size)
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS library_fts USING fts5(
                server_id UNINDEXED,
                item_type UNINDEXED,
                item_id UNINDEXED,
                title,
                subtitle
            );
            CREATE INDEX IF NOT EXISTS albums_server_title_idx
                ON albums(server_id, title);
            CREATE INDEX IF NOT EXISTS albums_server_title_nocase_idx
                ON albums(server_id, title COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS albums_server_artist_idx
                ON albums(server_id, artist_id, album_id);
            CREATE INDEX IF NOT EXISTS tracks_server_title_idx
                ON tracks(server_id, title);
            CREATE INDEX IF NOT EXISTS tracks_server_title_nocase_idx
                ON tracks(server_id, title COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS artists_server_name_nocase_idx
                ON artists(server_id, name COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS album_artists_server_name_nocase_idx
                ON album_artists(server_id, name COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS genres_server_name_nocase_idx
                ON genres(server_id, name COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS playlists_server_name_nocase_idx
                ON playlists(server_id, name COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS tracks_server_album_idx
                ON tracks(server_id, album_id, disc_number, track_number);
            CREATE INDEX IF NOT EXISTS tracks_server_artist_idx
                ON tracks(server_id, artist_id, album_id);
            CREATE INDEX IF NOT EXISTS home_section_items_order_idx
                ON home_section_items(server_id, section_kind, position);
            CREATE INDEX IF NOT EXISTS home_section_prefetch_items_order_idx
                ON home_section_prefetch_items(server_id, section_kind, position);
            CREATE INDEX IF NOT EXISTS album_genres_server_genre_idx
                ON album_genres(server_id, genre_name, album_id);
            CREATE INDEX IF NOT EXISTS track_genres_server_genre_idx
                ON track_genres(server_id, genre_name, track_id);
            CREATE INDEX IF NOT EXISTS album_artist_links_server_artist_idx
                ON album_artist_links(server_id, artist_id, album_id);
            CREATE INDEX IF NOT EXISTS track_artist_links_server_artist_idx
                ON track_artist_links(server_id, artist_id, track_id);
            CREATE INDEX IF NOT EXISTS track_music_folders_folder_idx
                ON track_music_folders(server_id, folder_id, track_id);
            CREATE INDEX IF NOT EXISTS track_music_folders_track_idx
                ON track_music_folders(server_id, track_id, folder_id);
            CREATE INDEX IF NOT EXISTS track_local_matches_track_idx
                ON track_local_matches(server_id, track_id);
            ",
        )?;
        self.ensure_column("tracks", "source_format", "TEXT")?;
        self.connection
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }
    pub(super) fn table_exists(&self, table: &str) -> StoreResult<bool> {
        let count = self.connection.query_row(
            "
            SELECT COUNT(*)
            FROM sqlite_master
            WHERE type = 'table' AND name = ?1
            ",
            params![table],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }
    pub(super) fn table_has_column(&self, table: &str, column: &str) -> StoreResult<bool> {
        let mut statement = self
            .connection
            .prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
        Ok(collect_rows(columns)?.iter().any(|name| name == column))
    }
    pub(super) fn ensure_column(
        &self,
        table: &str,
        column: &str,
        definition: &str,
    ) -> StoreResult<()> {
        if self.table_exists(table)? && !self.table_has_column(table, column)? {
            self.connection.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
        Ok(())
    }
    pub fn load_queue_snapshot(&self, server_id: &ServerId) -> StoreResult<Option<QueueSnapshot>> {
        let value = self
            .connection
            .query_row(
                "SELECT value FROM queue_snapshots WHERE server_id = ?1",
                params![server_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        value
            .map(|json| serde_json::from_str(&json).map_err(StoreError::from))
            .transpose()
    }
    pub fn save_queue_snapshot(&self, snapshot: &QueueSnapshot) -> StoreResult<()> {
        let value = serde_json::to_string(snapshot)?;
        self.connection.execute(
            "
            INSERT INTO queue_snapshots (server_id, value, updated_at)
            VALUES (?1, ?2, CURRENT_TIMESTAMP)
            ON CONFLICT(server_id) DO UPDATE SET
                value = excluded.value,
                updated_at = excluded.updated_at
            ",
            params![snapshot.server_id.as_str(), value],
        )?;
        Ok(())
    }
    pub fn save_server(&self, saved: &SavedServer) -> StoreResult<()> {
        save_server_on_connection(&self.connection, saved)
    }
    pub fn save_server_settings_update(
        &self,
        saved: &SavedServer,
        clear_identity_cache: bool,
    ) -> StoreResult<()> {
        self.write_batch(|connection| {
            save_server_on_connection(connection, saved)?;
            if clear_identity_cache {
                clear_server_identity_cache_on_connection(connection, &saved.server.id)?;
            }
            Ok(())
        })
    }
    pub fn set_active_server(&self, server_id: &ServerId) -> StoreResult<()> {
        self.connection.execute(
            "
            INSERT INTO active_server (singleton, server_id)
            VALUES (1, ?1)
            ON CONFLICT(singleton) DO UPDATE SET server_id = excluded.server_id
            ",
            params![server_id.as_str()],
        )?;
        Ok(())
    }
    pub fn active_server(&self) -> StoreResult<Option<SavedServer>> {
        self.connection
            .query_row(
                "
                SELECT s.server_id, s.provider, s.name, s.base_url, s.user_id,
                       s.username, s.trust_invalid_cert
                FROM active_server a
                JOIN servers s ON s.server_id = a.server_id
                WHERE a.singleton = 1
                ",
                [],
                saved_server_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }
    pub fn list_servers(&self) -> StoreResult<Vec<SavedServer>> {
        let mut statement = self.connection.prepare(
            "
            SELECT server_id, provider, name, base_url, user_id, username, trust_invalid_cert
            FROM servers
            ORDER BY name
            ",
        )?;
        collect_rows(statement.query_map([], saved_server_from_row)?)
    }
    pub fn save_server_local_access(&self, access: &ServerLocalAccess) -> StoreResult<()> {
        self.connection.execute(
            "
            INSERT INTO server_local_access (
                server_id, root_path, path_replace_from, path_replace_to, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)
            ON CONFLICT(server_id) DO UPDATE SET
                root_path = excluded.root_path,
                path_replace_from = excluded.path_replace_from,
                path_replace_to = excluded.path_replace_to,
                updated_at = excluded.updated_at
            ",
            params![
                access.server_id.as_str(),
                access.root_path.as_str(),
                access.path_replace_from.as_deref(),
                access.path_replace_to.as_deref(),
            ],
        )?;
        Ok(())
    }
    pub fn server_local_access(
        &self,
        server_id: &ServerId,
    ) -> StoreResult<Option<ServerLocalAccess>> {
        self.connection
            .query_row(
                "
                SELECT server_id, root_path, path_replace_from, path_replace_to
                FROM server_local_access
                WHERE server_id = ?1
                ",
                params![server_id.as_str()],
                |row| {
                    Ok(ServerLocalAccess {
                        server_id: ServerId::new(row.get::<_, String>(0)?),
                        root_path: row.get(1)?,
                        path_replace_from: row.get(2)?,
                        path_replace_to: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }
    pub fn delete_server_local_access(&self, server_id: &ServerId) -> StoreResult<()> {
        self.connection.execute(
            "DELETE FROM server_local_access WHERE server_id = ?1",
            params![server_id.as_str()],
        )?;
        Ok(())
    }
    pub fn upsert_music_folders(
        &self,
        server_id: &ServerId,
        folders: &[MusicFolder],
        generation: i64,
    ) -> StoreResult<()> {
        self.write_batch(|connection| {
            let mut statement = connection.prepare(
                "
                INSERT INTO server_music_folders (server_id, folder_id, name, sync_generation)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(server_id, folder_id) DO UPDATE SET
                    name = excluded.name,
                    sync_generation = excluded.sync_generation
                ",
            )?;
            for folder in folders {
                statement.execute(params![
                    server_id.as_str(),
                    folder.id.as_str(),
                    folder.name.as_str(),
                    generation,
                ])?;
            }
            Ok(())
        })
    }
    pub fn list_music_folders(&self, server_id: &ServerId) -> StoreResult<Vec<MusicFolder>> {
        let mut statement = self.connection.prepare(
            "
            SELECT folder_id, name
            FROM server_music_folders
            WHERE server_id = ?1
            ORDER BY name COLLATE NOCASE
            ",
        )?;
        collect_rows(statement.query_map(params![server_id.as_str()], |row| {
            Ok(MusicFolder {
                id: MusicFolderId::new(row.get::<_, String>(0)?),
                name: row.get(1)?,
            })
        })?)
    }
    pub fn selected_music_folder_id(
        &self,
        server_id: &ServerId,
    ) -> StoreResult<Option<MusicFolderId>> {
        self.connection
            .query_row(
                "
                SELECT selected_music_folder_id
                FROM server_library_preferences
                WHERE server_id = ?1
                ",
                params![server_id.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|value| value.flatten().map(MusicFolderId::new))
            .map_err(StoreError::from)
    }
    pub fn set_selected_music_folder_id(
        &self,
        server_id: &ServerId,
        folder_id: Option<&MusicFolderId>,
    ) -> StoreResult<()> {
        self.connection.execute(
            "
            INSERT INTO server_library_preferences (
                server_id, selected_music_folder_id, updated_at
            )
            VALUES (?1, ?2, CURRENT_TIMESTAMP)
            ON CONFLICT(server_id) DO UPDATE SET
                selected_music_folder_id = excluded.selected_music_folder_id,
                updated_at = excluded.updated_at
            ",
            params![server_id.as_str(), folder_id.map(MusicFolderId::as_str)],
        )?;
        Ok(())
    }
    pub fn upsert_track_music_folder_memberships(
        &self,
        server_id: &ServerId,
        folder_id: &MusicFolderId,
        tracks: &[Track],
        generation: i64,
    ) -> StoreResult<()> {
        self.write_batch(|connection| {
            let mut statement = connection.prepare(
                "
                INSERT INTO track_music_folders (
                    server_id, track_id, folder_id, sync_generation
                )
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(server_id, track_id, folder_id) DO UPDATE SET
                    sync_generation = excluded.sync_generation
                ",
            )?;
            for track in tracks {
                statement.execute(params![
                    server_id.as_str(),
                    track.id.as_str(),
                    folder_id.as_str(),
                    generation,
                ])?;
            }
            Ok(())
        })
    }
    pub fn replace_track_local_matches(
        &self,
        server_id: &ServerId,
        matches: &[(TrackId, String, String)],
    ) -> StoreResult<()> {
        self.write_batch(|connection| {
            connection.execute(
                "DELETE FROM track_local_matches WHERE server_id = ?1",
                params![server_id.as_str()],
            )?;
            let mut statement = connection.prepare(
                "
                INSERT INTO track_local_matches (
                    server_id, track_id, local_path, match_kind, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)
                ",
            )?;
            for (track_id, local_path, match_kind) in matches {
                statement.execute(params![
                    server_id.as_str(),
                    track_id.as_str(),
                    local_path.as_str(),
                    match_kind.as_str(),
                ])?;
            }
            Ok(())
        })
    }
    pub fn delete_track_local_matches(&self, server_id: &ServerId) -> StoreResult<()> {
        self.connection.execute(
            "DELETE FROM track_local_matches WHERE server_id = ?1",
            params![server_id.as_str()],
        )?;
        Ok(())
    }
    pub fn track_local_match_path(
        &self,
        server_id: &ServerId,
        track_id: &TrackId,
    ) -> StoreResult<Option<String>> {
        self.connection
            .query_row(
                "
                SELECT local_path
                FROM track_local_matches
                WHERE server_id = ?1 AND track_id = ?2
                ",
                params![server_id.as_str(), track_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::from)
    }
    pub fn track_local_match_paths(
        &self,
        server_id: &ServerId,
    ) -> StoreResult<Vec<(TrackId, String)>> {
        let mut statement = self.connection.prepare(
            "
            SELECT track_id, local_path
            FROM track_local_matches
            WHERE server_id = ?1
            ORDER BY track_id
            ",
        )?;
        collect_rows(statement.query_map(params![server_id.as_str()], |row| {
            Ok((TrackId::new(row.get::<_, String>(0)?), row.get(1)?))
        })?)
    }
    pub fn sync_state(&self, server_id: &ServerId) -> StoreResult<SyncState> {
        self.connection
            .query_row(
                "
                SELECT server_id, generation, status, last_started_at, last_completed_at, last_error
                FROM sync_state
                WHERE server_id = ?1
                ",
                params![server_id.as_str()],
                |row| {
                    Ok(SyncState {
                        server_id: ServerId::new(row.get::<_, String>(0)?),
                        generation: row.get(1)?,
                        status: row.get(2)?,
                        last_started_at: row.get(3)?,
                        last_completed_at: row.get(4)?,
                        last_error: row.get(5)?,
                    })
                },
            )
            .map_err(StoreError::from)
    }
    pub fn sync_completed_age_seconds(&self, server_id: &ServerId) -> StoreResult<Option<i64>> {
        self.connection
            .query_row(
                "
                SELECT CAST(strftime('%s', 'now') AS INTEGER)
                     - CAST(strftime('%s', last_completed_at) AS INTEGER)
                FROM sync_state
                WHERE server_id = ?1 AND last_completed_at IS NOT NULL
                ",
                params![server_id.as_str()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map(|value| value.flatten())
            .map_err(StoreError::from)
    }
    pub fn begin_sync(&self, server_id: &ServerId) -> StoreResult<i64> {
        let current = self
            .connection
            .query_row(
                "SELECT generation FROM sync_state WHERE server_id = ?1",
                params![server_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let generation = current + 1;
        self.connection.execute(
            "
            INSERT INTO sync_state (
                server_id, generation, status, last_started_at, last_error
            )
            VALUES (?1, ?2, 'running', CURRENT_TIMESTAMP, NULL)
            ON CONFLICT(server_id) DO UPDATE SET
                generation = excluded.generation,
                status = excluded.status,
                last_started_at = excluded.last_started_at,
                last_error = NULL
            ",
            params![server_id.as_str(), generation],
        )?;
        Ok(generation)
    }
}

pub(super) fn save_server_on_connection(
    connection: &Connection,
    saved: &SavedServer,
) -> StoreResult<()> {
    connection.execute(
        "
        INSERT INTO servers (
            server_id, provider, name, base_url, user_id, username,
            trust_invalid_cert, updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, CURRENT_TIMESTAMP)
        ON CONFLICT(server_id) DO UPDATE SET
            provider = excluded.provider,
            name = excluded.name,
            base_url = excluded.base_url,
            user_id = excluded.user_id,
            username = excluded.username,
            trust_invalid_cert = excluded.trust_invalid_cert,
            updated_at = excluded.updated_at
        ",
        params![
            saved.server.id.as_str(),
            saved.server.provider,
            saved.server.name,
            saved.server.base_url,
            saved.user_id,
            saved.username,
            bool_to_i64(saved.trust_invalid_cert),
        ],
    )?;
    connection.execute(
        "
        INSERT OR IGNORE INTO sync_state (server_id)
        VALUES (?1)
        ",
        params![saved.server.id.as_str()],
    )?;
    Ok(())
}

pub(super) fn clear_server_identity_cache_on_connection(
    connection: &Connection,
    server_id: &ServerId,
) -> StoreResult<()> {
    clear_library_cache_on_connection(connection, server_id)?;
    connection.execute(
        "DELETE FROM queue_snapshots WHERE server_id = ?1",
        params![server_id.as_str()],
    )?;
    connection.execute(
        "DELETE FROM server_library_preferences WHERE server_id = ?1",
        params![server_id.as_str()],
    )?;
    connection.execute(
        "
        UPDATE sync_state
        SET generation = 0,
            status = 'idle',
            last_started_at = NULL,
            last_completed_at = NULL,
            last_error = NULL
        WHERE server_id = ?1
        ",
        params![server_id.as_str()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_op_migration(_store: &Store) -> StoreResult<()> {
        Ok(())
    }

    #[test]
    fn schema_migration_path_requires_adjacent_steps() {
        static MIGRATIONS: &[SchemaMigration] = &[
            SchemaMigration {
                from_version: 1,
                run: no_op_migration,
            },
            SchemaMigration {
                from_version: 2,
                run: no_op_migration,
            },
        ];

        let path = schema_migration_path(1, 3, MIGRATIONS).expect("migration path");
        assert_eq!(
            path.iter()
                .map(|migration| migration.to_version())
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(schema_migration_path(1, 4, MIGRATIONS).is_none());
        assert!(schema_migration_path(4, 3, MIGRATIONS).is_none());
    }
}
