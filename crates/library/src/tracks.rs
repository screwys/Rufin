//! Owns complete Track orders, bounded final rows, and details.
//! Each bounded row query includes its ordered artist and genre links.

use sqlx::{Connection, FromRow, QueryBuilder, Sqlite, SqliteConnection};

use crate::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, LibraryError, LibraryResult,
    ReadCancellation, SourceKey, TrackKey,
};

// Provider dates take precedence over the preserved first-seen timestamp.
macro_rules! added_at {
    ($row:literal) => {
        concat!(
            "COALESCE(unixepoch(",
            $row,
            ".date_added),",
            $row,
            ".first_seen_at)"
        )
    };
}
pub(crate) use added_at;

const TRACK_ROW_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TrackSort {
    Position,
    Title,
    TrackNumber,
    Artist,
    AlbumArtist,
    Album,
    Year,
    ReleaseDate,
    DateAdded,
    LastPlayed,
    PlayCount,
    UserRating,
    Genre,
    Bpm,
    Duration,
    Favorite,
}

impl TrackSort {
    pub(crate) fn order_terms(self, descending: bool, playlist: bool) -> Vec<String> {
        self.order_by(descending, |field| match field {
            "rating" => "COALESCE((SELECT rating FROM user_media_state WHERE media_uri=track.media_uri),track.source_rating)".into(),
            "favorite" => "COALESCE((SELECT favorite FROM user_media_state WHERE media_uri=track.media_uri),track.source_favorite,0)".into(),
            "genre" => "(SELECT genre.sort_text FROM track_genres relation JOIN genres genre USING(genre_key) WHERE relation.track_key=track.track_key ORDER BY relation.position LIMIT 1)".into(),
            "play_count" => displayed_stat("play_count"),
            "last_played" => displayed_stat("last_played"),
            "key" if playlist && self == Self::Position => "entry.position".into(),
            "key" => "track.track_key".into(),
            "artist_sort" => "track.display_artist".into(),
            "album_artist_sort" => "track.sort_album_artist".into(),
            "album_sort" => "track.sort_album".into(),
            "album_year" => "track.sort_album_year".into(),
            "date_added" => added_at!("track").into(),
            _ => format!("track.{field}"),
        })
    }

    pub(crate) fn order_by(self, descending: bool, column: impl Fn(&str) -> String) -> Vec<String> {
        if self == Self::Position {
            return vec![format!(
                "{} {}",
                column("key"),
                if descending { "DESC" } else { "ASC" }
            )];
        }
        let field = match self {
            Self::Position => unreachable!(),
            Self::Title => "sort_text",
            Self::TrackNumber => "disc_number",
            Self::Artist => "artist_sort",
            Self::AlbumArtist => "album_artist_sort",
            Self::Album => "album_sort",
            Self::Year => "year",
            Self::ReleaseDate => "release_date",
            Self::DateAdded => "date_added",
            Self::LastPlayed => "last_played",
            Self::PlayCount => "play_count",
            Self::UserRating => "rating",
            Self::Genre => "genre",
            Self::Bpm => "bpm",
            Self::Duration => "duration_millis",
            Self::Favorite => "favorite",
        };
        let mut fields = vec![field];
        match self {
            Self::Artist | Self::AlbumArtist => {
                fields.extend([
                    "album_year",
                    "album_sort",
                    "album_key",
                    "disc_number",
                    "track_number",
                ]);
            }
            Self::Album => fields.extend(["album_key", "disc_number", "track_number"]),
            Self::TrackNumber => fields.push("track_number"),
            _ => {}
        }
        if self != Self::Title {
            fields.push("sort_text");
        }
        fields.push("key");
        fields
            .into_iter()
            .enumerate()
            .flat_map(|(index, field)| {
                let direction =
                    if descending && (index == 0 || (self == Self::TrackNumber && index == 1)) {
                        "DESC"
                    } else {
                        "ASC"
                    };
                let column = column(field);
                if matches!(field, "album_artist_sort" | "album_year" | "album_key") {
                    return vec![
                        format!("({column} IS NULL) ASC"),
                        format!("{column} {direction}"),
                    ];
                }
                let nulls = if matches!(
                    field,
                    "year"
                        | "release_date"
                        | "date_added"
                        | "last_played"
                        | "rating"
                        | "genre"
                        | "bpm"
                ) {
                    " NULLS LAST"
                } else {
                    ""
                };
                vec![format!("{column} {direction}{nulls}")]
            })
            .collect()
    }
}

#[derive(Clone, Debug, FromRow, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct TrackArtistLink {
    pub artist_key: ArtistKey,
    pub media_uri: String,
    pub name: String,
}

// Included only in bounded row projections, with track and album already joined by identity.
pub(crate) const TRACK_LINK_COLUMNS: &str = "
    album.media_uri album_media_uri,
    (SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY credit.position)
       FROM track_artists credit JOIN artists artist USING(artist_key)
       WHERE credit.track_key=track.track_key) artists,
    (SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY credit.position)
       FROM album_artists credit JOIN artists artist USING(artist_key)
       WHERE credit.album_key=track.album_key) album_artists,";

#[derive(Clone, Debug, FromRow, PartialEq, serde::Deserialize)]
pub struct TrackGenreLink {
    pub genre_key: GenreKey,
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AudioProperties {
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u32>,
    pub channels: Option<u32>,
}

impl sqlx::Type<Sqlite> for AudioProperties {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as sqlx::Type<Sqlite>>::type_info()
    }
}

impl<'row> sqlx::Decode<'row, Sqlite> for AudioProperties {
    fn decode(value: sqlx::sqlite::SqliteValueRef<'row>) -> Result<Self, sqlx::error::BoxDynError> {
        let text = <&str as sqlx::Decode<Sqlite>>::decode(value)?;
        Ok(serde_json::from_str(text)?)
    }
}

#[derive(Clone, Debug, PartialEq, FromRow)]
pub struct TrackRow {
    pub audio_properties: AudioProperties,
    pub source_path: Option<String>,
    pub source_name: String,
    pub track_key: TrackKey,
    pub source_key: Option<SourceKey>,
    pub source_id: Option<String>,
    pub object_id: Option<String>,
    pub album_key: Option<AlbumKey>,
    pub album_media_uri: Option<String>,
    pub title: String,
    pub album: String,
    pub artist: String,
    pub album_display_artist: Option<String>,
    pub duration_millis: i64,
    pub disc_number: i64,
    pub track_number: i64,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub media_uri: String,
    pub source_format: Option<String>,
    pub comment: Option<String>,
    pub bpm: Option<i64>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
    #[sqlx(try_from = "Vec<u8>")]
    pub loudness_analysis_key: [u8; 32],
    pub artwork_binding: Option<Vec<u8>>,
    pub favorite: bool,
    pub rating: Option<i64>,
    pub last_played: Option<i64>,
    pub play_count: i64,
    pub skip_count: i64,
    pub is_downloaded: bool,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub primary_artist_musicbrainz_id: Option<String>,
    #[sqlx(json)]
    pub artists: Vec<TrackArtistLink>,
    #[sqlx(json)]
    pub album_artists: Vec<TrackArtistLink>,
    #[sqlx(json)]
    pub genres: Vec<TrackGenreLink>,
}

#[derive(Clone, Debug)]
pub struct TrackRoutePage<T = TrackRow> {
    pub query: crate::SongQuery,
    pub count: usize,
    pub disc_sections: Vec<(u32, i64)>,
    pub first_row_position: usize,
    pub first_rows: Vec<T>,
}

impl Database {
    pub async fn track_uris(
        &self,
        tracks: &[TrackKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar("SELECT media_uri FROM json_each(?1) requested JOIN tracks ON track_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
            .bind(serde_json::to_string(tracks)?).fetch_all(&mut *connection).await?)
    }

    pub async fn track_uris_for_source(
        &self,
        source: SourceKey,
        tracks: &[TrackKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        if tracks.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(track_key,ordinal) AS (");
        query.push_values(tracks.iter().enumerate(), |mut row, (ordinal, track)| {
            row.push_bind(*track).push_bind(ordinal as i64);
        });
        query
            .push(") SELECT track.media_uri FROM requested JOIN tracks track USING(track_key) WHERE track.source_key=")
            .push_bind(source)
            .push(" ORDER BY requested.ordinal");
        Ok(query
            .build_query_scalar::<String>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?)
    }

    pub async fn track_source_by_uri(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<crate::SourceId>> {
        let mut connection = tokio::select! {
            result = self.acquire_reader() => result?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let source = sqlx::query_scalar::<_, String>(
            "SELECT source.object_id FROM tracks track JOIN sources source USING(source_key)
             WHERE track.media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?;
        Ok(source.map(crate::SourceId::new))
    }

    pub async fn track_row_by_uri(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<TrackRow>> {
        self.track_rows_by_uri(&[media_uri.to_string()], cancellation)
            .await
            .map(|mut rows| rows.pop())
    }

    pub async fn track_rows_by_uri(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackRow>> {
        if media_uris.len() > TRACK_ROW_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Track row reads are limited to {TRACK_ROW_LIMIT} media URIs"
            )));
        }
        if media_uris.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = tokio::select! {
            result = self.acquire_reader() => result?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let mut transaction = connection.begin().await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(media_uri,ordinal) AS (");
        query.push_values(
            media_uris.iter().enumerate(),
            |mut row, (ordinal, media_uri)| {
                row.push_bind(media_uri).push_bind(ordinal as i64);
            },
        );
        query.push(
            ") SELECT track.track_key
               FROM requested JOIN tracks track USING(media_uri) ORDER BY requested.ordinal",
        );
        let keys = query
            .build_query_scalar::<TrackKey>()
            .persistent(false)
            .fetch_all(&mut *transaction)
            .await?;
        let result = load_track_rows(&mut transaction, &keys).await?;
        transaction.commit().await?;
        Ok(result)
    }

    pub async fn track_key_by_uri(
        &self,
        uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(
            sqlx::query_scalar("SELECT track_key FROM tracks WHERE media_uri=?1")
                .bind(uri)
                .fetch_optional(&mut *connection)
                .await?,
        )
    }

    pub async fn track_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(
            sqlx::query_scalar("SELECT track_key FROM tracks WHERE source_key=?1 AND object_id=?2")
                .bind(source)
                .bind(object_id)
                .fetch_optional(&mut *connection)
                .await?,
        )
    }

    pub async fn track_object_count(
        &self,
        source: SourceKey,
        object_ids: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<i64> {
        if object_ids.is_empty() {
            return Ok(0);
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM json_each(?1) requested JOIN tracks track ON track.source_key=?2 AND track.object_id=requested.value",
        ).bind(serde_json::to_string(object_ids)?).bind(source)
            .fetch_one(&mut *connection).await?)
    }

    pub async fn track_rows(
        &self,
        keys: &[TrackKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackRow>> {
        if keys.len() > TRACK_ROW_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Track row reads are limited to {TRACK_ROW_LIMIT} keys"
            )));
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let result = load_track_rows(&mut transaction, keys).await;
        transaction.commit().await?;
        result
    }
}

pub(crate) fn track_query(
    source: SourceKey,
    sort: TrackSort,
    descending: bool,
    favorites_only: bool,
    folder: Option<FolderKey>,
    filter: &str,
) -> crate::source_window::SourceQuery {
    let mut query = crate::source_window::SourceQuery {
        from: "tracks track".into(),
        predicate: format!("track.indexed=1 AND track.source_key={}", source.raw()),
        order: sort.order_terms(descending, false),
        uri: "track.media_uri".into(),
        entry_key: "NULL".into(),
        arguments: Default::default(),
    };
    track_filter(&mut query, folder, filter, favorites_only);

    query
}

pub(crate) fn downloaded_filter(query: &mut crate::source_window::SourceQuery) {
    query.predicate.push_str(" AND EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=track.media_uri AND access.origin='download')");
}

pub(crate) fn track_filter(
    query: &mut crate::source_window::SourceQuery,
    folder: Option<FolderKey>,
    filter: &str,
    favorites_only: bool,
) {
    if let Some(folder) = folder {
        query.predicate.push_str(&format!(" AND EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key={})",folder.raw()));
    }
    if favorites_only {
        query.predicate.push_str(" AND COALESCE((SELECT favorite FROM user_media_state state WHERE state.media_uri=track.media_uri),track.source_favorite)=1");
    }
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
    if !filter.is_empty() {
        let filter = crate::source_window::quote(&filter);
        query
            .predicate
            .push_str(&format!(" AND instr(track.normalized_search,{filter})>0"));
    }
}

pub(crate) fn displayed_stat(field: &str) -> String {
    let local = match field {
        "play_count" => {
            "track.local_play_count+COALESCE((SELECT play_count FROM legacy_activity old JOIN sources origin ON origin.object_id=old.source_id WHERE origin.source_key=track.source_key AND old.track_object_id=track.object_id AND period='lifetime' AND item_kind='track'),0)"
        }
        "skip_count" => {
            "COALESCE((SELECT sum(skipped) FROM listens WHERE media_uri=track.media_uri),0)+COALESCE((SELECT skip_count FROM legacy_activity old JOIN sources origin ON origin.object_id=old.source_id WHERE origin.source_key=track.source_key AND old.track_object_id=track.object_id AND period='lifetime' AND item_kind='track'),0)"
        }
        "last_played" => {
            "(SELECT max(value) FROM (SELECT max(started_at) value FROM listens WHERE media_uri=track.media_uri UNION ALL SELECT last_played_at FROM legacy_activity old JOIN sources origin ON origin.object_id=old.source_id WHERE origin.source_key=track.source_key AND old.track_object_id=track.object_id AND period='lifetime' AND item_kind='track'))"
        }
        _ => unreachable!(),
    };
    let remote = if field == "last_played" {
        format!("track.source_{field}")
    } else {
        format!("COALESCE(track.source_{field},0)")
    };
    format!(
        "CASE WHEN (SELECT kind FROM sources WHERE source_key=track.source_key) IN ('local','files','webdav','smb','documents') OR track.source_key IS NULL THEN {local} ELSE {remote} END"
    )
}

pub(crate) async fn load_track_rows(
    connection: &mut SqliteConnection,
    keys: &[TrackKey],
) -> LibraryResult<Vec<TrackRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!("WITH requested AS (SELECT value track_key,key position FROM json_each(?1))
        SELECT {links} track.track_key,track.source_key,source.object_id source_id,track.object_id,track.album_key,
        COALESCE(track.title,track.media_uri) title,COALESCE(track.display_album,'') album,COALESCE(track.display_artist,'') artist,
        COALESCE(album.display_artist,track.album_display_artist) album_display_artist,
        COALESCE(track.duration_millis,0) duration_millis,COALESCE(track.disc_number,0) disc_number,COALESCE(track.track_number,0) track_number,
        track.year,track.release_date,date({added},'unixepoch') date_added,track.media_uri,
        track.audio_properties,track.source_format,track.source_path,COALESCE(source.display_name,'') source_name,track.comment,track.bpm,
        track.musicbrainz_recording_id,track.musicbrainz_release_track_id,track.cue_path,track.cue_start_millis,track.cue_end_millis,
        track.loudness_analysis_key,track.artwork_binding,
        COALESCE((SELECT favorite FROM user_media_state WHERE media_uri=track.media_uri),track.source_favorite,0) favorite,
        COALESCE((SELECT rating FROM user_media_state WHERE media_uri=track.media_uri),track.source_rating)/10 rating,
        {plays} play_count,{skips} skip_count,{last} last_played,
        EXISTS(SELECT 1 FROM local_access_files WHERE media_uri=track.media_uri AND origin='download') is_downloaded,
        COALESCE(album.musicbrainz_release_id,track.musicbrainz_album_id) musicbrainz_album_id,
        COALESCE(album.musicbrainz_release_group_id,track.musicbrainz_release_group_id) musicbrainz_release_group_id,
        COALESCE((SELECT artist.musicbrainz_artist_id FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),
            (SELECT artist.musicbrainz_artist_id FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=track.album_key ORDER BY credit.position LIMIT 1),track.primary_artist_musicbrainz_id) primary_artist_musicbrainz_id,
        (SELECT json_group_array(json_object('genre_key',genre.genre_key,'name',genre.name) ORDER BY relation.position) FROM track_genres relation JOIN genres genre USING(genre_key) WHERE relation.track_key=track.track_key) genres
        FROM requested JOIN tracks track USING(track_key) LEFT JOIN sources source USING(source_key) LEFT JOIN albums album USING(album_key) ORDER BY requested.position",
        links=TRACK_LINK_COLUMNS,added=added_at!("track"),plays=displayed_stat("play_count"),skips=displayed_stat("skip_count"),last=displayed_stat("last_played"));
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(serde_json::to_string(keys)?)
        .fetch_all(connection)
        .await?)
}

#[derive(Default)]
pub(crate) struct SongMetadata<'a> {
    pub title: Option<&'a str>,
    pub artist: Option<&'a str>,
    pub album: Option<&'a str>,
    pub album_display_artist: Option<&'a str>,
    pub duration_millis: Option<i64>,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub year: Option<i64>,
    pub release_date: Option<&'a str>,
    pub source_format: Option<&'a str>,
    pub audio_properties: Option<&'a AudioProperties>,
    pub musicbrainz_recording_id: Option<&'a str>,
    pub musicbrainz_release_track_id: Option<&'a str>,
    pub musicbrainz_album_id: Option<&'a str>,
    pub musicbrainz_release_group_id: Option<&'a str>,
    pub primary_artist_musicbrainz_id: Option<&'a str>,
}

pub(crate) async fn remember_song(
    connection: &mut SqliteConnection,
    uri: &str,
    facts: SongMetadata<'_>,
) -> LibraryResult<TrackKey> {
    let (source, object) = if let Some((source, kind, object)) =
        crate::source_entity_parts(uri).filter(|(_, kind, _)| kind == "track")
    {
        let _ = kind;
        sqlx::query("INSERT INTO sources(object_id) VALUES(?1) ON CONFLICT(object_id) DO NOTHING")
            .bind(source.as_str())
            .execute(&mut *connection)
            .await?;
        let key =
            sqlx::query_scalar::<_, SourceKey>("SELECT source_key FROM sources WHERE object_id=?1")
                .bind(source.as_str())
                .fetch_one(&mut *connection)
                .await?;
        (Some(key), Some(object))
    } else {
        (None, None)
    };
    let known_uri: Option<String> =
        if let (Some(source), Some(object)) = (source, object.as_deref()) {
            sqlx::query_scalar("SELECT media_uri FROM tracks WHERE source_key=?1 AND object_id=?2")
                .bind(source)
                .bind(object)
                .fetch_optional(&mut *connection)
                .await?
        } else {
            None
        };
    let uri = known_uri.as_deref().unwrap_or(uri);
    let key = sqlx::query_scalar::<_,TrackKey>("INSERT INTO tracks(media_uri,source_key,object_id,indexed,title,display_artist,display_album,album_display_artist,duration_millis,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,musicbrainz_album_id,musicbrainz_release_group_id,primary_artist_musicbrainz_id,audio_properties)
        VALUES(?1,?2,?3,0,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,COALESCE(?19,'{}'))
        ON CONFLICT(media_uri) DO UPDATE SET
        source_key=COALESCE(tracks.source_key,excluded.source_key),object_id=COALESCE(tracks.object_id,excluded.object_id),
        title=COALESCE(tracks.title,excluded.title),display_artist=COALESCE(tracks.display_artist,excluded.display_artist),display_album=COALESCE(tracks.display_album,excluded.display_album),
        album_display_artist=COALESCE(tracks.album_display_artist,excluded.album_display_artist),duration_millis=COALESCE(tracks.duration_millis,excluded.duration_millis),
        disc_number=COALESCE(tracks.disc_number,excluded.disc_number),track_number=COALESCE(tracks.track_number,excluded.track_number),year=COALESCE(tracks.year,excluded.year),release_date=COALESCE(tracks.release_date,excluded.release_date),
        audio_properties=CASE WHEN tracks.indexed=0 AND ?19 IS NOT NULL THEN excluded.audio_properties ELSE tracks.audio_properties END,
        source_format=COALESCE(tracks.source_format,excluded.source_format),musicbrainz_recording_id=COALESCE(tracks.musicbrainz_recording_id,excluded.musicbrainz_recording_id),musicbrainz_release_track_id=COALESCE(tracks.musicbrainz_release_track_id,excluded.musicbrainz_release_track_id),
        musicbrainz_album_id=COALESCE(tracks.musicbrainz_album_id,excluded.musicbrainz_album_id),musicbrainz_release_group_id=COALESCE(tracks.musicbrainz_release_group_id,excluded.musicbrainz_release_group_id),primary_artist_musicbrainz_id=COALESCE(tracks.primary_artist_musicbrainz_id,excluded.primary_artist_musicbrainz_id)
        WHERE (tracks.indexed=0 AND ?19 IS NOT NULL AND tracks.audio_properties IS NOT excluded.audio_properties) OR (tracks.source_key IS NULL AND excluded.source_key IS NOT NULL) OR (tracks.object_id IS NULL AND excluded.object_id IS NOT NULL) OR (tracks.title IS NULL AND excluded.title IS NOT NULL) OR (tracks.display_artist IS NULL AND excluded.display_artist IS NOT NULL) OR (tracks.display_album IS NULL AND excluded.display_album IS NOT NULL) OR (tracks.album_display_artist IS NULL AND excluded.album_display_artist IS NOT NULL) OR (tracks.duration_millis IS NULL AND excluded.duration_millis IS NOT NULL) OR (tracks.disc_number IS NULL AND excluded.disc_number IS NOT NULL) OR (tracks.track_number IS NULL AND excluded.track_number IS NOT NULL) OR (tracks.year IS NULL AND excluded.year IS NOT NULL) OR (tracks.release_date IS NULL AND excluded.release_date IS NOT NULL) OR (tracks.source_format IS NULL AND excluded.source_format IS NOT NULL) OR (tracks.musicbrainz_recording_id IS NULL AND excluded.musicbrainz_recording_id IS NOT NULL) OR (tracks.musicbrainz_release_track_id IS NULL AND excluded.musicbrainz_release_track_id IS NOT NULL) OR (tracks.musicbrainz_album_id IS NULL AND excluded.musicbrainz_album_id IS NOT NULL) OR (tracks.musicbrainz_release_group_id IS NULL AND excluded.musicbrainz_release_group_id IS NOT NULL) OR (tracks.primary_artist_musicbrainz_id IS NULL AND excluded.primary_artist_musicbrainz_id IS NOT NULL) RETURNING track_key")
        .bind(uri).bind(source).bind(object).bind(facts.title).bind(facts.artist).bind(facts.album).bind(facts.album_display_artist).bind(facts.duration_millis).bind(facts.disc_number).bind(facts.track_number).bind(facts.year).bind(facts.release_date).bind(facts.source_format).bind(facts.musicbrainz_recording_id).bind(facts.musicbrainz_release_track_id).bind(facts.musicbrainz_album_id).bind(facts.musicbrainz_release_group_id).bind(facts.primary_artist_musicbrainz_id).bind(facts.audio_properties.map(serde_json::to_string).transpose()?).fetch_optional(&mut *connection).await?;
    let Some(key) = key else {
        return Ok(
            sqlx::query_scalar("SELECT track_key FROM tracks WHERE media_uri=?1")
                .bind(uri)
                .fetch_one(connection)
                .await?,
        );
    };
    refresh_song_search(connection, key).await?;
    Ok(key)
}

pub(crate) async fn refresh_song_search(
    connection: &mut SqliteConnection,
    key: TrackKey,
) -> LibraryResult<()> {
    let (title,artist,album,comment,year): (String,String,String,String,String)=sqlx::query_as("SELECT COALESCE(title,media_uri),COALESCE(display_artist,''),COALESCE(display_album,''),COALESCE(comment,''),COALESCE(CAST(year AS TEXT),'') FROM tracks WHERE track_key=?1").bind(key).fetch_one(&mut *connection).await?;
    let search = format!("{title} {artist} {album} {comment} {year}").to_lowercase();
    sqlx::query("UPDATE tracks SET normalized_search=?2,sort_text=CASE WHEN indexed=0 OR sort_text IS NULL THEN ?3 ELSE sort_text END,sort_album=CASE WHEN indexed=0 OR sort_album IS NULL THEN ?4 ELSE sort_album END WHERE track_key=?1 AND normalized_search IS NOT ?2")
        .bind(key).bind(search).bind(title.to_lowercase()).bind(album.to_lowercase()).execute(connection).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::open_writer, schema};

    #[tokio::test]
    async fn bounded_track_row_window_preserves_order_and_relations() {
        let file = tempfile::NamedTempFile::new().expect("create Track row Store");
        let mut connection = open_writer(file.path())
            .await
            .expect("open Track row Store");
        schema::initialize_durable(&mut connection)
            .await
            .expect("initialize durable Store");
        let source = sqlx::query_scalar::<_, SourceKey>(
            "INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('source','Source','source',zeroblob(32)) RETURNING source_key",
        )
        .fetch_one(&mut connection)
        .await
        .expect("insert source");
        let artist = sqlx::query_scalar::<_, ArtistKey>("INSERT INTO artists(source_key,object_id,media_uri,name,normalized_name,sort_text,source_favorite) VALUES (?1,'artist','rufin:source/artist/%73%6F%75%72%63%65/%61%72%74%69%73%74','Artist','artist','artist',0) RETURNING artist_key")
            .bind(source).fetch_one(&mut connection).await.expect("insert Artist");
        let genre = sqlx::query_scalar::<_, GenreKey>("INSERT INTO genres(source_key,object_id,name,normalized_name,sort_text) VALUES (?1,'genre','Genre','genre','genre') RETURNING genre_key")
            .bind(source).fetch_one(&mut connection).await.expect("insert Genre");
        sqlx::query("WITH RECURSIVE sequence(value) AS (VALUES(0) UNION ALL SELECT value+1 FROM sequence WHERE value<255) INSERT INTO tracks(source_key,object_id,media_uri,title,normalized_search,display_album,display_artist,sort_text,duration_millis,disc_number,track_number,source_favorite) SELECT ?1,'track-'||value,'file:///track-'||value,'Track','track','', 'Artist','track-'||printf('%03d',value),1000,0,value,0 FROM sequence")
            .bind(source).execute(&mut connection).await.expect("insert Track window");
        sqlx::query("INSERT INTO track_artists(track_key,artist_key,position) SELECT track_key,?1,0 FROM tracks WHERE source_key=?2")
            .bind(artist).bind(source).execute(&mut connection).await.expect("insert Track Artists");
        sqlx::query("INSERT INTO track_genres(track_key,genre_key,position) SELECT track_key,?1,0 FROM tracks WHERE source_key=?2")
            .bind(genre).bind(source).execute(&mut connection).await.expect("insert Track Genres");
        let mut keys = sqlx::query_scalar::<_, TrackKey>(
            "SELECT track_key FROM tracks WHERE source_key=?1 ORDER BY track_key",
        )
        .bind(source)
        .fetch_all(&mut connection)
        .await
        .expect("read Track window keys");
        keys.reverse();
        let one = load_track_rows(&mut connection, &keys[..1])
            .await
            .expect("load one Track row");
        let window = load_track_rows(&mut connection, &keys)
            .await
            .expect("load bounded Track row window");
        assert_eq!(one.len(), 1);
        assert_eq!(window.len(), 256);
        assert_eq!(one[0].track_key, keys[0]);
        for (row, key) in window.iter().zip(&keys) {
            assert_eq!(row.track_key, *key);
            assert_eq!(row.artists.len(), 1);
            assert_eq!(row.artists[0].artist_key, artist);
            assert_eq!(row.genres.len(), 1);
            assert_eq!(row.genres[0].genre_key, genre);
        }
        let album = sqlx::query_scalar::<_, AlbumKey>("INSERT INTO albums(source_key,object_id,media_uri,title,normalized_title,display_artist,sort_text,year) VALUES(?1,'album','album:one','Album','album','Artist','album',2000) RETURNING album_key")
            .bind(source).fetch_one(&mut connection).await.unwrap();
        sqlx::query("INSERT INTO album_artists VALUES(?1,?2,0)")
            .bind(album)
            .bind(artist)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("UPDATE tracks SET album_key=?1 WHERE track_number<128")
            .bind(album)
            .execute(&mut connection)
            .await
            .unwrap();
        for sort in [TrackSort::Artist, TrackSort::AlbumArtist, TrackSort::Album] {
            for descending in [false, true] {
                let query = track_query(source, sort, descending, false, None, "");
                let plan =
                    sqlx::query_as::<_, (i64, i64, i64, String)>(sqlx::AssertSqlSafe(format!(
                        "EXPLAIN QUERY PLAN {} LIMIT 48",
                        query.select("track.track_key")
                    )))
                    .fetch_all(&mut connection)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|row| row.3)
                    .collect::<Vec<_>>()
                    .join(" | ");
                assert!(
                    !plan.contains("TEMP B-TREE"),
                    "{sort:?}, descending={descending}: {plan}"
                );
                if sort == TrackSort::AlbumArtist {
                    let sections = crate::scroll_sections::decode_sections(
                        sqlx::query_scalar(sqlx::AssertSqlSafe(
                            crate::scroll_sections::section_sql(
                                &query.select("track.track_key"),
                                crate::scroll_sections::order_text(&query.order[0]),
                            ),
                        ))
                        .fetch_all(&mut connection)
                        .await
                        .unwrap(),
                    )
                    .unwrap();
                    assert_eq!(
                        sections
                            .iter()
                            .map(|section| (section.title.as_str(), section.index))
                            .collect::<Vec<_>>(),
                        [("A", 0), ("#", 128)]
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn object_identity_order_pages_without_losing_a_large_folder() {
        let file = tempfile::NamedTempFile::new().expect("create Track identity Store");
        let mut connection = open_writer(file.path())
            .await
            .expect("open Track identity Store");
        schema::initialize_durable(&mut connection)
            .await
            .expect("initialize durable Store");
        let source = sqlx::query_scalar::<_, SourceKey>(
            "INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('source','Source','source',zeroblob(32)) RETURNING source_key",
        )
        .fetch_one(&mut connection)
        .await
        .expect("insert source");
        sqlx::query("WITH RECURSIVE sequence(value) AS (VALUES(0) UNION ALL SELECT value+1 FROM sequence WHERE value<299) INSERT INTO tracks(source_key,object_id,title,normalized_search,display_album,display_artist,sort_text,duration_millis,disc_number,track_number,media_uri,source_favorite) SELECT ?1,'track-'||printf('%03d',value),'Track','track','','Artist','track-'||printf('%03d',value),1000,0,value,'file:///track-'||printf('%03d',value),0 FROM sequence")
            .bind(source).execute(&mut connection).await.expect("insert large Folder Track order");
        drop(connection);

        let database = Database::open(file.path())
            .await
            .expect("open Track identity Database");
        let object_ids = (0..300)
            .map(|index| format!("track-{index:03}"))
            .collect::<Vec<_>>();
        let cancel = ReadCancellation::new();
        assert_eq!(
            database
                .track_object_count(source, &object_ids, &cancel)
                .await
                .unwrap(),
            300
        );
        let query = crate::SongQuery {
            filter: "track".into(),
            sort: Some(TrackSort::Title),
            ..crate::SongQuery::new(crate::SongScope::Objects {
                source,
                ids: object_ids.into_iter().rev().collect::<Vec<_>>().into(),
            })
        };
        assert_eq!(database.song_count(&query, &cancel).await.unwrap(), 300);
        let mut ordered = Vec::new();
        for offset in (0..300).step_by(64) {
            ordered.extend(
                database
                    .song_rows(&query, offset, 64, &cancel)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|row| row.track_key),
            );
        }
        assert_eq!(
            ordered,
            (0..300)
                .map(|index| TrackKey::from_raw(index + 1))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            database
                .selected_track_keys(&query.clone().selection(), &cancel)
                .await
                .unwrap(),
            ordered
        );
        assert_eq!(
            database
                .selected_track_keys(
                    &crate::SongSelection::Query {
                        query: query.clone(),
                        ranges: Some(vec![1..3, 129..132]),
                    },
                    &cancel
                )
                .await
                .unwrap(),
            [2, 3, 130, 131, 132].map(TrackKey::from_raw),
        );
        assert_eq!(
            database
                .song_sections(&query, &cancel)
                .await
                .unwrap()
                .into_iter()
                .map(|section| (section.title, section.index))
                .collect::<Vec<_>>(),
            [("T".to_string(), 0)],
        );
        assert!(
            database
                .song_disc_sections(
                    &crate::SongQuery {
                        sort: Some(TrackSort::TrackNumber),
                        ..query.clone()
                    },
                    &cancel
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            database
                .song_count(
                    &crate::SongQuery {
                        favorites_only: true,
                        ..query.clone()
                    },
                    &cancel
                )
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            database
                .song_count(
                    &crate::SongQuery {
                        filter: "missing".into(),
                        ..query
                    },
                    &cancel
                )
                .await
                .unwrap(),
            0
        );
    }
}
