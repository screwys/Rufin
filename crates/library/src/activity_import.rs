//! Activity import: listens from Rufin's exports, ListenBrainz exports and Last.fm CSVs, and
//! play counts from other players, put on the library tracks they name. An entry that names
//! no library track counts nowhere, so it is skipped.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::Path;

use serde::Deserialize;
use unicode_normalization::UnicodeNormalization;

use crate::catalog::views::Context;
use crate::{ActivityRecord, Catalog, LibraryError, LibraryResult, PlayCount, TrackKey};

/// Two listens of a track this close together are one: a scrobble of a play Rufin recorded,
/// or the same listen in two exports.
const SAME_LISTEN_SECONDS: i64 = 60;
const WRITE_ROWS: usize = 5000;

/// What an import did: listens it added, listens Rufin already had, tracks whose play count it
/// set, entries that name no library track, and lines it could not read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityImportReport {
    pub listens: u64,
    pub known: u64,
    pub play_counts: u64,
    pub unmatched: u64,
    pub unreadable: u64,
}

/// A track as a file names it: by URI or path when it has one, and by its names.
#[derive(Default)]
struct Named {
    uri: Option<String>,
    artist: String,
    title: String,
    album: String,
    duration_ms: Option<i64>,
}

struct Heard {
    track: Named,
    started_at: i64,
    external_id: Option<String>,
    skipped: bool,
}

struct Counted {
    track: Named,
    plays: i64,
    skips: i64,
    last_played_at: Option<i64>,
}

#[derive(Default)]
struct Entries {
    heard: Vec<Heard>,
    counted: Vec<Counted>,
    unreadable: u64,
}

impl Catalog {
    /// Imports an activity file in one Write: a ListenBrainz export ZIP or its JSONL, Rufin's
    /// JSONL, or a CSV of listens or of play counts. Listens join the library's; a play count
    /// replaces its track's lifetime count. Relative paths are relative to `base`.
    pub async fn import_activity(
        &self,
        input: &[u8],
        base: Option<&Path>,
    ) -> LibraryResult<ActivityImportReport> {
        let entries = read(input, base.unwrap_or(Path::new("/")))?;
        let mut report = ActivityImportReport {
            unreadable: entries.unreadable,
            ..ActivityImportReport::default()
        };
        self.loaded().await;
        let mut matcher = Matcher::new(self);
        let mut write = self.write().await?;
        let uris = entries
            .heard
            .iter()
            .map(|entry| &entry.track)
            .chain(entries.counted.iter().map(|entry| &entry.track))
            .filter_map(|track| track.uri.clone())
            .collect::<Vec<_>>();
        if !uris.is_empty() {
            matcher.uris = sqlx::query_as::<_, (String, TrackKey)>(
                "SELECT media_uri,track_key FROM tracks WHERE indexed=1 AND media_uri IN(SELECT value FROM json_each(?1))",
            )
            .bind(serde_json::to_string(&uris)?)
            .fetch_all(write.connection().await?)
            .await?
            .into_iter()
            .collect();
        }

        let mut heard_at = HashMap::<TrackKey, Vec<i64>>::new();
        if !entries.heard.is_empty() {
            let rows: Vec<(TrackKey, i64)> = sqlx::query_as(
                "SELECT track.track_key,listen.started_at FROM listens listen JOIN tracks track USING(media_uri)",
            )
            .fetch_all(write.connection().await?)
            .await?;
            for (track, started_at) in rows {
                heard_at.entry(track).or_default().push(started_at);
            }
        }
        let mut rows = Vec::new();
        for entry in entries.heard {
            let Some(track) = matcher.pick(&entry.track) else {
                report.unmatched += 1;
                continue;
            };
            let times = heard_at.entry(track.key).or_default();
            if times
                .iter()
                .any(|time| (time - entry.started_at).abs() <= SAME_LISTEN_SECONDS)
            {
                report.known += 1;
                continue;
            }
            times.push(entry.started_at);
            let duration = entry
                .track
                .duration_ms
                .unwrap_or(i64::from(track.duration_ms));
            let id = entry
                .external_id
                .unwrap_or_else(|| import_id(&entry.track, entry.started_at));
            rows.push(serde_json::json!([
                id,
                track.key,
                entry.track.title,
                entry.track.artist,
                entry.track.album,
                entry.started_at,
                local_period(entry.started_at),
                duration,
                entry.skipped,
            ]));
        }
        let mut heard_uris = Vec::new();
        for chunk in rows.chunks(WRITE_ROWS) {
            let added: Vec<String> = sqlx::query_scalar(
                "INSERT INTO listens(external_id,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped)
                 SELECT row.value->>0,source.object_id,track.media_uri,row.value->>2,row.value->>3,row.value->>4,row.value->>5,row.value->>6,row.value->>7,row.value->>7,row.value->>8
                 FROM json_each(?1) row JOIN tracks track ON track.track_key=row.value->>1
                 LEFT JOIN sources source ON source.source_key=track.source_key
                 WHERE true ON CONFLICT DO NOTHING RETURNING media_uri",
            )
            .bind(serde_json::to_string(chunk)?)
            .fetch_all(write.connection().await?)
            .await?;
            report.listens += added.len() as u64;
            report.known += (chunk.len() - added.len()) as u64;
            heard_uris.extend(added);
        }
        heard_uris.sort_unstable();
        heard_uris.dedup();
        write
            .recount(&heard_uris.iter().map(String::as_str).collect::<Vec<_>>())
            .await?;

        let mut counted = HashMap::new();
        for entry in entries.counted {
            match matcher.pick(&entry.track) {
                Some(track) => {
                    counted.insert(track.key, entry);
                }
                None => report.unmatched += 1,
            }
        }
        if !counted.is_empty() {
            let keys = counted.keys().copied().collect::<Vec<_>>();
            let uris: Vec<(TrackKey, String)> = sqlx::query_as(
                "SELECT track_key,media_uri FROM tracks WHERE track_key IN(SELECT value FROM json_each(?1))",
            )
            .bind(serde_json::to_string(&keys)?)
            .fetch_all(write.connection().await?)
            .await?;
            let counts = uris
                .into_iter()
                .filter_map(|(key, media_uri)| {
                    let entry = counted.get(&key)?;
                    Some(PlayCount {
                        media_uri,
                        period: "lifetime".into(),
                        plays: entry.plays,
                        skips: entry.skips,
                        last_played_at: entry.last_played_at,
                    })
                })
                .collect::<Vec<_>>();
            report.play_counts = counts.len() as u64;
            crate::activity::write_play_counts(&mut write, &counts).await?;
        }
        write.commit().await?;
        Ok(report)
    }
}

/// A listen's play id when its file has none, the same on every device that imports it.
fn import_id(track: &Named, started_at: i64) -> String {
    let hash = blake3::hash(
        format!(
            "{}\0{}\0{started_at}",
            name_key(&track.artist),
            name_key(&track.title)
        )
        .as_bytes(),
    );
    format!("import:{}", hash.to_hex())
}

fn local_period(started_at: i64) -> String {
    chrono::DateTime::from_timestamp(started_at, 0)
        .unwrap_or_default()
        .with_timezone(&chrono::Local)
        .format("%Y-%m")
        .to_string()
}

/// Names compare by their letters and digits, whatever their case, width or punctuation; a
/// name of punctuation only, such as "&", by its characters.
fn name_key(name: &str) -> String {
    let lower = name.nfkc().flat_map(char::to_lowercase).collect::<String>();
    let key = lower
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>();
    if key.is_empty() {
        lower.split_whitespace().collect()
    } else {
        key
    }
}

#[derive(Clone)]
struct Candidate {
    key: TrackKey,
    album: String,
    duration_ms: u32,
    listens: bool,
}

/// The library's listed tracks by artist and title, read from the resident Catalog once per
/// import, and by URI for the entries that name one.
struct Matcher {
    names: HashMap<(String, String), Vec<Candidate>>,
    uris: HashMap<String, TrackKey>,
    durations: HashMap<TrackKey, u32>,
}

impl Matcher {
    fn new(catalog: &Catalog) -> Self {
        let snapshot = catalog.snapshot();
        let context = Context::new(&snapshot);
        let mut names = HashMap::<(String, String), Vec<Candidate>>::new();
        let mut durations = HashMap::new();
        for (slot, record) in snapshot.tracks.iter() {
            if context.visible(slot).is_none() {
                continue;
            }
            let title = name_key(snapshot.text(record.title));
            if title.is_empty() {
                continue;
            }
            let key = TrackKey::from_raw(i64::from(record.key));
            durations.insert(key, record.duration_ms);
            let candidate = Candidate {
                key,
                album: name_key(snapshot.text(record.album_title)),
                duration_ms: record.duration_ms,
                listens: context.counts_listens(record),
            };
            let mut artists = vec![name_key(snapshot.text(record.artist))];
            for artist in snapshot.track_runs(record)[0] {
                if let Some(artist) = snapshot.artists.live(*artist) {
                    artists.push(name_key(snapshot.text(artist.name)));
                }
            }
            artists.sort_unstable();
            artists.dedup();
            for artist in artists.into_iter().filter(|artist| !artist.is_empty()) {
                names
                    .entry((artist, title.clone()))
                    .or_default()
                    .push(candidate.clone());
            }
        }
        Self {
            names,
            uris: HashMap::new(),
            durations,
        }
    }

    /// The track an entry names: its URI's, else the one with its artist and title, preferring
    /// a track whose plays are Rufin's listens, then its album, then its duration.
    fn pick(&self, named: &Named) -> Option<Candidate> {
        if let Some(key) = named.uri.as_ref().and_then(|uri| self.uris.get(uri)) {
            return Some(Candidate {
                key: *key,
                album: String::new(),
                duration_ms: self.durations.get(key).copied().unwrap_or_default(),
                listens: true,
            });
        }
        let candidates = self
            .names
            .get(&(name_key(&named.artist), name_key(&named.title)))?;
        let album = name_key(&named.album);
        candidates
            .iter()
            .min_by_key(|candidate| {
                (
                    !candidate.listens,
                    !album.is_empty() && candidate.album != album,
                    named.duration_ms.map_or(0, |duration| {
                        (i64::from(candidate.duration_ms) - duration).abs()
                    }),
                    candidate.key.raw(),
                )
            })
            .cloned()
    }
}

fn read(input: &[u8], base: &Path) -> LibraryResult<Entries> {
    let mut entries = Entries::default();
    if input.starts_with(b"PK\x03\x04") {
        // A ListenBrainz export: listens/<year>/<month>.jsonl beside the user's other data.
        let mut archive = zip::ZipArchive::new(Cursor::new(input)).map_err(invalid)?;
        for index in 0..archive.len() {
            let mut file = archive.by_index(index).map_err(invalid)?;
            let name = file.name().map_err(invalid)?;
            if name.starts_with("listens/") && name.ends_with(".jsonl") && file.is_file() {
                let mut text = Vec::new();
                file.read_to_end(&mut text)?;
                json_lines(&text, &mut entries);
            }
        }
        return Ok(entries);
    }
    let text = input.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(input);
    match text.iter().find(|byte| !byte.is_ascii_whitespace()) {
        Some(b'{') => json_lines(text, &mut entries),
        Some(_) => csv_rows(text, base, &mut entries),
        None => {}
    }
    Ok(entries)
}

fn invalid(error: impl std::fmt::Display) -> LibraryError {
    LibraryError::InvalidRequest(error.to_string())
}

/// A JSONL line: a ListenBrainz listen or a listen Rufin exported.
#[derive(Deserialize)]
#[serde(untagged)]
enum JsonListen {
    Brainz(BrainzListen),
    Rufin(Box<ActivityRecord>),
}

#[derive(Deserialize)]
struct BrainzListen {
    listened_at: f64,
    track_metadata: BrainzTrack,
}

#[derive(Deserialize)]
struct BrainzTrack {
    artist_name: String,
    track_name: String,
    #[serde(default)]
    release_name: Option<String>,
    #[serde(default)]
    additional_info: BrainzInfo,
}

#[derive(Default, Deserialize)]
struct BrainzInfo {
    duration_ms: Option<i64>,
    #[serde(default)]
    skipped: bool,
}

fn json_lines(text: &[u8], entries: &mut Entries) {
    for line in text.split(|byte| *byte == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        json_entry(serde_json::from_slice(line), entries);
    }
}

fn json_entry(listen: serde_json::Result<JsonListen>, entries: &mut Entries) {
    match listen {
        Ok(JsonListen::Brainz(listen)) if listen.listened_at >= 0.0 => {
            let track = listen.track_metadata;
            entries.heard.push(Heard {
                track: Named {
                    uri: None,
                    artist: track.artist_name,
                    title: track.track_name,
                    album: track.release_name.unwrap_or_default(),
                    duration_ms: track.additional_info.duration_ms,
                },
                started_at: listen.listened_at as i64,
                external_id: None,
                skipped: track.additional_info.skipped,
            });
        }
        Ok(JsonListen::Rufin(record)) if record.version == 1 && record.listen.started_at >= 0 => {
            let listen = record.listen;
            entries.heard.push(Heard {
                track: Named {
                    uri: Some(record.current_media_uri.unwrap_or(listen.media_uri)),
                    artist: listen.artist,
                    title: listen.title,
                    album: listen.album,
                    duration_ms: Some(listen.duration_millis),
                },
                started_at: listen.started_at,
                external_id: listen.external_id,
                skipped: listen.skipped,
            });
        }
        _ => entries.unreadable += 1,
    }
}

/// Where each field is in a CSV row, from its header or, without one, from the column count.
#[derive(Default)]
struct Columns {
    artist: Option<usize>,
    title: Option<usize>,
    album: Option<usize>,
    time: Option<usize>,
    plays: Option<usize>,
    skips: Option<usize>,
    last_played: Option<usize>,
    path: Option<usize>,
}

impl Columns {
    fn from_header(header: &csv::StringRecord) -> Option<Self> {
        let mut columns = Self::default();
        let mut known = false;
        for (index, name) in header.iter().enumerate() {
            let name = name.trim().to_lowercase().replace([' ', '-'], "_");
            let field = match name.as_str() {
                "artist" | "artist_name" | "artists" => &mut columns.artist,
                "track" | "title" | "name" | "track_name" | "song" => &mut columns.title,
                "album" | "release" | "release_name" | "album_name" => &mut columns.album,
                "time" | "date" | "timestamp" | "uts" | "utc_time" | "listened_at"
                | "played_at" | "scrobble_time" => &mut columns.time,
                "plays" | "playcount" | "play_count" => &mut columns.plays,
                "skips" | "skipcount" | "skip_count" => &mut columns.skips,
                "last_played" | "lastplayed" | "last_played_at" => &mut columns.last_played,
                "path" | "location" | "file" | "uri" => &mut columns.path,
                _ => continue,
            };
            field.get_or_insert(index);
            known = true;
        }
        known.then_some(columns)
    }

    /// Rufin's Last.fm CSV, and Last.fm exports without a header: artist, album, title and
    /// time, or artist, title and time.
    fn positional(len: usize) -> Self {
        match len {
            4 => Self {
                artist: Some(0),
                album: Some(1),
                title: Some(2),
                time: Some(3),
                ..Self::default()
            },
            3 => Self {
                artist: Some(0),
                title: Some(1),
                time: Some(2),
                ..Self::default()
            },
            _ => Self::default(),
        }
    }

    fn row(&self, row: &csv::StringRecord, base: &Path, entries: &mut Entries) {
        let field = |index: Option<usize>| {
            index
                .and_then(|index| row.get(index))
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let track = Named {
            uri: field(self.path).and_then(|path| crate::playlist_locator(path, base)),
            artist: field(self.artist).unwrap_or_default().to_owned(),
            title: field(self.title).unwrap_or_default().to_owned(),
            album: field(self.album).unwrap_or_default().to_owned(),
            duration_ms: None,
        };
        if track.uri.is_none() && (track.artist.is_empty() || track.title.is_empty()) {
            entries.unreadable += 1;
            return;
        }
        if self.plays.is_some() {
            let number = |index| field(index).and_then(|value| value.parse::<i64>().ok());
            match number(self.plays) {
                Some(plays) if plays >= 0 => entries.counted.push(Counted {
                    track,
                    plays,
                    skips: number(self.skips).unwrap_or(0).max(0),
                    last_played_at: field(self.last_played).and_then(moment),
                }),
                _ => entries.unreadable += 1,
            }
            return;
        }
        match field(self.time).and_then(moment) {
            Some(started_at) if started_at >= 0 => entries.heard.push(Heard {
                track,
                started_at,
                external_id: None,
                skipped: false,
            }),
            _ => entries.unreadable += 1,
        }
    }
}

fn csv_rows(text: &[u8], base: &Path, entries: &mut Entries) {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text);
    let mut rows = reader.records();
    let Some(Ok(first)) = rows.next() else {
        entries.unreadable += 1;
        return;
    };
    let columns = match Columns::from_header(&first) {
        Some(columns) => columns,
        None => {
            let columns = Columns::positional(first.len());
            columns.row(&first, base, entries);
            columns
        }
    };
    for row in rows {
        match row {
            Ok(row) => columns.row(&row, base, entries),
            Err(_) => entries.unreadable += 1,
        }
    }
}

/// Unix seconds or milliseconds, RFC 3339, or a UTC date as Last.fm and spreadsheets write it.
fn moment(text: &str) -> Option<i64> {
    if let Ok(number) = text.parse::<f64>() {
        let seconds = if number > 100_000_000_000.0 {
            number / 1000.0
        } else {
            number
        };
        return Some(seconds as i64);
    }
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(time.timestamp());
    }
    [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%d %b %Y %H:%M",
        "%d %b %Y, %H:%M",
    ]
    .iter()
    .find_map(|format| chrono::NaiveDateTime::parse_from_str(text, format).ok())
    .map(|time| time.and_utc().timestamp())
}
