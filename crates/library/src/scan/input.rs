use crate::AudioProperties;

fn upsert_sql(table: &str, conflict: &str, fields: &[(&str, String)], from: &str) -> String {
    let columns = fields.iter().map(|(column, _)| *column).collect::<Vec<_>>();
    let values = fields
        .iter()
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    let assignments = columns
        .iter()
        .map(|column| format!("{column}=excluded.{column}"))
        .collect::<Vec<_>>();
    let previous = columns
        .iter()
        .map(|column| format!("{table}.{column}"))
        .collect::<Vec<_>>();
    let incoming = columns
        .iter()
        .map(|column| format!("excluded.{column}"))
        .collect::<Vec<_>>();
    format!(
        "INSERT INTO {table}({}) SELECT {} {from} ON CONFLICT({conflict}) DO UPDATE SET {} WHERE ({}) IS NOT ({})",
        columns.join(","),
        values.join(","),
        assignments.join(","),
        previous.join(","),
        incoming.join(",")
    )
}

// Each stored field owns its type, binding and replacement expression. SQL is built once.
macro_rules! record_input {
    (
        $(#[$attribute:meta])*
        $name:ident <$lifetime:lifetime> ($input:ident; $($argument:ident: $argument_type:ty),*);
        context { $($parameter:ident,)* }
        table $table:literal, conflict $conflict:literal, from $from:literal;
        fields { $($field:ident: $ty:ty $(=> $column:ident)? $(= $binding:expr)? $(; $value:expr)? ,)* }
        derived { $($derived:ident: $derived_value:expr,)* }
        extra { $(pub $extra:ident: $extra_type:ty,)* }
    ) => {
        $(#[$attribute])*
        #[derive(Clone, Debug, Default, PartialEq)]
        pub struct $name {
            $(pub $field: $ty,)*
            $(pub $extra: $extra_type,)*
        }

        impl $name {
            pub(super) fn write_rows<$lifetime>(rows: impl IntoIterator<Item = (&$lifetime Self, $($argument_type),*)>)
                -> sqlx::QueryBuilder<sqlx::Sqlite>
            {
                static SQL: std::sync::LazyLock<(String, String)> = std::sync::LazyLock::new(|| {
                    let parameters = [$(stringify!($field),)* $(stringify!($parameter),)*];
                    let fields = [
                        $((record_input!(@column $field $(, $column)?), record_input!(@value $field $(, $value)?)),)*
                        $((stringify!($derived), $derived_value.to_string()),)*
                    ];
                    (format!("WITH incoming({}) AS (", parameters.join(",")),
                     format!(") {}", upsert_sql($table, $conflict, &fields, $from)))
                });
                let mut query = sqlx::QueryBuilder::new(&SQL.0);
                query.push_values(rows, |mut row, ($input, $($argument),*)| {
                    $(row.push_bind(record_input!(@binding $input, $field $(, $binding)?));)*
                    $(row.push_bind($parameter);)*
                });
                query.push(&SQL.1);
                query
            }
        }
    };
    (@column $field:ident) => { stringify!($field) };
    (@column $field:ident, $column:ident) => { stringify!($column) };
    (@value $field:ident) => { concat!("incoming.", stringify!($field)).to_string() };
    (@value $field:ident, $value:expr) => { $value.to_string() };
    (@binding $input:ident, $field:ident) => { &$input.$field };
    (@binding $input:ident, $field:ident, $binding:expr) => { $binding };
}

record_input! {
    TrackInput<'a>(input; source: i64, canonical_uri: &'a str, normalized_search: &'a str, audio_properties: &'a str);
    context { source, normalized_search, }
    table "tracks", conflict "media_uri", from "FROM incoming LEFT JOIN tracks previous ON previous.media_uri=incoming.media_uri WHERE true";
    fields {
        object_id: String,
        album_object_id: Option<String> => album_key; "(SELECT album_key FROM albums WHERE source_key=incoming.source AND object_id=incoming.album_object_id)",
        title: String,
        display_album: String,
        display_artist: String,
        sort_text: String,
        duration_millis: i64,
        disc_number: i64,
        track_number: i64,
        year: Option<i64>,
        release_date: Option<String>,
        date_added: Option<String>,
        media_uri: Option<String> = canonical_uri,
        source_format: Option<String>,
        comment: Option<String>,
        bpm: Option<i64>,
        musicbrainz_recording_id: Option<String>,
        musicbrainz_release_track_id: Option<String>,
        cue_path: Option<String>,
        cue_start_millis: Option<i64>,
        cue_end_millis: Option<i64>,
        first_seen_at: Option<i64>; "COALESCE(previous.first_seen_at,incoming.first_seen_at)",
        source_path: Option<String>,
        loudness_analysis_key: [u8; 32] => source_loudness_analysis_key = input.loudness_analysis_key.as_slice(),
        audio_properties: AudioProperties = audio_properties,
    }
    derived {
        source_key: "incoming.source",
        indexed: "1",
        normalized_search: "incoming.normalized_search",
        loudness_analysis_key: "COALESCE((SELECT access.loudness_analysis_key FROM local_access_files access
            WHERE access.media_uri=incoming.media_uri
            ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,
                access.local_access_file_key LIMIT 1),incoming.loudness_analysis_key)",
    }
    extra {
        pub artwork_binding: Option<Vec<u8>>,
        pub favorite: Option<bool>,
        pub rating: Option<i64>,
        pub source_play_count: Option<i64>,
        pub source_skip_count: Option<i64>,
        pub source_last_played: Option<i64>,
        pub relations: Option<TrackRelationsInput>,
        pub source_loudness: Option<LoudnessInput>,
        pub album_loudness: Option<LoudnessInput>,
        pub folders: Option<Vec<String>>,
    }
}

macro_rules! album_input {
    (
        first { $($first:ident: $first_type:ty,)* }
        supplied { $($field:ident: $ty:ty = $bit:literal,)* }
        compilation $compilation_true:literal, $compilation_false:literal;
        rating $rating:literal;
    ) => {
        record_input! {
            AlbumInput<'a>(input; source: i64, media_uri: String, normalized_title: String, first: bool);
            context { source, media_uri, normalized_title, first, }
            table "albums", conflict "source_key,object_id", from "FROM incoming LEFT JOIN albums previous
                ON previous.source_key=incoming.source AND previous.object_id=incoming.object_id
                JOIN temp.scan_seen seen ON seen.kind='album' AND seen.object_id=incoming.object_id WHERE true";
            fields {
                object_id: String,
                $($first: $first_type; concat!("CASE WHEN incoming.first THEN incoming.", stringify!($first), " ELSE previous.", stringify!($first), " END"),)*
                $($field: $ty; concat!("CASE WHEN seen.supplied & ", stringify!($bit), "=0 THEN COALESCE(incoming.", stringify!($field), ",previous.", stringify!($field), ") ELSE previous.", stringify!($field), " END"),)*
                is_compilation: Option<bool>; "CASE WHEN previous.album_key IS NULL THEN incoming.is_compilation ELSE previous.is_compilation END",
                first_seen_at: Option<i64>; "COALESCE(previous.first_seen_at,incoming.first_seen_at)",
            }
            derived {
                source_key: "incoming.source",
                media_uri: "incoming.media_uri",
                normalized_title: "CASE WHEN incoming.first THEN incoming.normalized_title ELSE previous.normalized_title END",
            }
            extra {
                pub artwork_binding: Option<Vec<u8>>,
                pub favorite: Option<bool>,
                pub rating: Option<i64>,
                pub relations: Option<AlbumRelationsInput>,
            }
        }

        impl AlbumInput {
            pub(super) const RATING: i64 = $rating;

            pub(super) fn merge(&mut self, later: &Self) {
                $(if self.$field.is_none() { self.$field.clone_from(&later.$field); })*
                self.first_seen_at = self.first_seen_at.or(later.first_seen_at);
                self.favorite = later.favorite.or(self.favorite);
                self.rating = self.rating.filter(|rating| (0..=10).contains(rating))
                    .or(later.rating.filter(|rating| (0..=10).contains(rating)));
                if later.artwork_binding.is_some() {
                    self.artwork_binding.clone_from(&later.artwork_binding);
                }
            }


            pub(super) fn supplied(&self, rating: Option<i64>) -> i64 {
                $(i64::from(self.$field.is_some()) * $bit |)*
                i64::from(self.is_compilation == Some(true)) * $compilation_true |
                i64::from(self.is_compilation == Some(false)) * $compilation_false |
                i64::from(rating.is_some()) * $rating
            }

            pub(super) const SUPPLIED: &str = concat!(
                $("(album.", stringify!($field), " IS NOT NULL)*", stringify!($bit), " | ",)*
                "COALESCE(album.is_compilation=1,0)*", stringify!($compilation_true),
                " | COALESCE(album.is_compilation=0,0)*", stringify!($compilation_false),
                " | (album.source_rating IS NOT NULL)*", stringify!($rating)
            );

            pub(super) fn finish_sql() -> &'static str {
                static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
                    let fields = [
                        $((stringify!($field), concat!("CASE WHEN seen.supplied & ", stringify!($bit), " THEN album.", stringify!($field), " END")),)*
                        ("is_compilation", concat!("CASE WHEN seen.supplied & ", stringify!($compilation_true), " THEN 1 WHEN seen.supplied & ", stringify!($compilation_false), " THEN 0 END")),
                    ];
                    let assignments = fields.iter().map(|(field, value)| format!("{field}={value}")).collect::<Vec<_>>();
                    let previous = fields.iter().map(|(field, _)| format!("album.{field}")).collect::<Vec<_>>();
                    let values = fields.iter().map(|(_, value)| *value).collect::<Vec<_>>();
                    format!("UPDATE albums AS album SET {} FROM temp.scan_seen seen
                        WHERE album.source_key=?1 AND seen.kind='album' AND seen.object_id=album.object_id AND seen.written
                        AND ({}) IS NOT ({})", assignments.join(","), previous.join(","), values.join(","))
                });
                SQL.as_str()
            }
        }
    };
}

// The first file supplies names. Optional album facts can come from any child file.
album_input! {
    first {
        title: String,
        display_artist: String,
        sort_text: String,
    }
    supplied {
        year: Option<i64> = 1,
        release_date: Option<String> = 2,
        date_added: Option<String> = 4,
        musicbrainz_release_id: Option<String> = 8,
        musicbrainz_release_group_id: Option<String> = 16,
    }
    compilation 32, 64;
    rating 128;
}

const CREDIT_NAME: &str = "CASE WHEN incoming.sort_text IS NULL AND previous.normalized_name=incoming.normalized_name THEN previous.name ELSE incoming.name END";
const CREDIT_SORT: &str =
    "COALESCE(incoming.sort_text,previous.sort_text,incoming.normalized_name)";

record_input! {
    #[derive(Eq)]
    ArtistInput<'a>(input; source: i64, media_uri: String, normalized_name: String);
    context { source, media_uri, normalized_name, }
    table "artists", conflict "source_key,object_id", from "FROM incoming LEFT JOIN artists previous
        ON previous.source_key=incoming.source AND previous.object_id=incoming.object_id WHERE true";
    fields {
        object_id: String,
        name: String; CREDIT_NAME,
        sort_text: Option<String>; CREDIT_SORT,
        musicbrainz_artist_id: Option<String>; "CASE WHEN incoming.sort_text IS NOT NULL THEN incoming.musicbrainz_artist_id ELSE COALESCE(incoming.musicbrainz_artist_id,previous.musicbrainz_artist_id) END",
    }
    derived {
        source_key: "incoming.source",
        media_uri: "incoming.media_uri",
        normalized_name: "incoming.normalized_name",
    }
    extra {
        pub artwork_binding: Option<Vec<u8>>,
        pub favorite: Option<bool>,
        pub rating: Option<i64>,
    }
}

record_input! {
    #[derive(Eq)]
    GenreInput<'a>(input; source: i64, normalized_name: String);
    context { source, normalized_name, }
    table "genres", conflict "source_key,object_id", from "FROM incoming LEFT JOIN genres previous
        ON previous.source_key=incoming.source AND previous.object_id=incoming.object_id WHERE true";
    fields {
        object_id: String,
        name: String; CREDIT_NAME,
        sort_text: Option<String>; CREDIT_SORT,
    }
    derived {
        source_key: "incoming.source",
        normalized_name: "incoming.normalized_name",
    }
    extra {
        pub artwork_binding: Option<Vec<u8>>,
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrackRelationsInput {
    pub artists: Vec<ArtistInput>,
    pub album_artists: Vec<ArtistInput>,
    pub genres: Vec<GenreInput>,
    pub moods: Vec<MoodInput>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MoodInput {
    pub object_id: String,
    pub name: String,
    pub sort_text: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FolderInput {
    pub object_id: String,
    pub name: Option<String>,
    pub sort_text: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlaylistInput {
    pub object_id: String,
    pub name: String,
    pub sort_text: String,
    pub artwork_binding: Option<Vec<u8>>,
    pub writable: Option<bool>,
    pub metadata_writable: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaylistEntryInput {
    pub object_id: String,
    pub track_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RecordInput {
    Track(Box<TrackInput>),
    Album(AlbumInput),
    Artist(ArtistInput),
    Genre(GenreInput),
    Folder(FolderInput),
    Playlist(PlaylistInput),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AlbumRelationsInput {
    pub artists: Vec<ArtistInput>,
    pub album_artists: Vec<ArtistInput>,
    pub genres: Vec<GenreInput>,
    pub release_types: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoudnessInput {
    pub integrated_lufs: Option<f64>,
    pub true_peak: Option<f64>,
    pub replay_gain_db: Option<f64>,
    pub replay_gain_peak: Option<f64>,
}
