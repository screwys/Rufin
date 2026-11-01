//! The provider merge. A sync batch is compared with the rows the Store holds and this Write's
//! staged rows, and only what differs is staged. A walk keeps the set of rows it saw in memory:
//! it leaves rows a live change wrote while it ran alone, and its completion hides the rows it
//! did not see. A reference batch never overrides native facts and patches only fields that
//! differ from the default. Provider playlists, lyrics, Home rows and file tables stay plain
//! SQL in the same Write.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{Map, Value};

use super::families::Owned;
use super::records::*;
use super::write::{Family, Scope, SourcePhase, Write};
use super::*;
use crate::metadata::{track_details_value, track_metadata_sql};
use crate::tags::{TAG_TABLES, TagKind};
use crate::{
    AlbumKey, ArtistCredit, ArtistKey, ArtistMetadata, HomeEntryInput, NativeUserState,
    TagMetadata, TagReference, TrackKey, TrackMetadata, TrackObservation, TrackReference,
};

const PAGE: usize = 128;
/// Completion and other passes over a whole source stage at most this many records per Write.
pub(crate) const COMPLETION_PAGE: usize = 512;
/// A hidden track is forgotten this long after it was hidden, unless something holds it.
const EXPIRY_SECONDS: i64 = 60 * 24 * 60 * 60;

/// A reference batch patches only the fields that differ from the default.
pub(crate) fn reference_patch<T: serde::Serialize + Default>(metadata: &T) -> LibraryResult<Value> {
    fn retain(value: &mut Value, default: &Value) -> bool {
        if let Value::Object(fields) = value {
            fields.retain(|key, value| retain(value, &default[key]));
            !fields.is_empty()
        } else {
            !value.is_null() && value != default
        }
    }
    let mut patch = serde_json::to_value(metadata)?;
    retain(&mut patch, &serde_json::to_value(T::default())?);
    Ok(patch)
}

/// RFC 7396, as SQLite's json_patch applies it.
pub(super) fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(fields) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let Value::Object(target) = target else {
        unreachable!("the target is an object")
    };
    for (key, value) in fields {
        if value.is_null() {
            target.remove(key);
        } else {
            merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

/// Authoritative metadata replaces every known field and keeps fields it does not know.
fn replace_known(stored: Option<Value>, metadata: Value) -> Value {
    match (stored, metadata) {
        (Some(Value::Object(mut stored)), Value::Object(fields)) => {
            stored.extend(fields);
            Value::Object(stored)
        }
        (_, metadata) => metadata,
    }
}

pub(super) fn parsed<T: serde::de::DeserializeOwned + Default>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

/// Stored JSON of rows that are about to change, read by key.
pub(super) async fn stored_json(
    write: &mut Write<'_>,
    table: &str,
    key: &str,
    column: &str,
    keys: &[i64],
) -> LibraryResult<HashMap<i64, Value>> {
    let mut stored = HashMap::new();
    for page in keys.chunks(PAGE) {
        let rows: Vec<(i64, Option<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {key},json({column}) FROM {table} WHERE {key} IN(SELECT value FROM json_each(?1))"
        )))
        .bind(serde_json::to_string(page)?)
        .fetch_all(write.connection().await?)
        .await?;
        for (key, json) in rows {
            if let Some(json) = json {
                stored.insert(key, serde_json::from_str(&json)?);
            }
        }
    }
    Ok(stored)
}

/// What the Store holds of tracks beside their rows: the whole metadata JSON and the details
/// JSON.
struct StoredTrack {
    metadata: Value,
    details: Value,
}

async fn stored_tracks(
    write: &mut Write<'_>,
    keys: &[i64],
) -> LibraryResult<HashMap<i64, StoredTrack>> {
    let mut stored = HashMap::new();
    for page in keys.chunks(PAGE) {
        let rows: Vec<(i64, String, Option<String>)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT track.track_key,{},json(details.metadata) FROM tracks track LEFT JOIN track_details details USING(track_key) WHERE track.track_key IN(SELECT value FROM json_each(?1))",
                track_metadata_sql("track", "details")
            )))
            .bind(serde_json::to_string(page)?)
            .fetch_all(write.connection().await?)
            .await?;
        for (key, metadata, details) in rows {
            stored.insert(
                key,
                StoredTrack {
                    metadata: serde_json::from_str(&metadata)?,
                    details: details
                        .map(|details| serde_json::from_str(&details))
                        .transpose()?
                        .unwrap_or_else(|| Value::Object(Map::new())),
                },
            );
        }
    }
    Ok(stored)
}

pub(super) fn dedupe<T: Eq + std::hash::Hash + Copy>(items: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut known = HashSet::new();
    items
        .into_iter()
        .filter(|item| known.insert(*item))
        .collect()
}

/// What a batch staged, for album resolution.
#[derive(Default)]
pub(crate) struct Touched {
    /// Records whose facts changed: their albums are resolved again.
    pub(crate) tracks: BTreeSet<i64>,
    pub(crate) albums: BTreeSet<i64>,
    pub(crate) artists: BTreeSet<i64>,
}

/// The kinds providers name in completion and removal.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    Track,
    Album,
    Artist,
    Tag(TagKind),
    Folder,
    Playlist,
}

impl Kind {
    fn parse(name: &str) -> LibraryResult<Self> {
        Ok(match name {
            "track" => Self::Track,
            "album" => Self::Album,
            "artist" => Self::Artist,
            "folder" => Self::Folder,
            "playlist" => Self::Playlist,
            _ => Self::Tag(
                TAG_TABLES
                    .iter()
                    .find(|table| table.name == name)
                    .ok_or_else(|| {
                        LibraryError::InvalidRequest(format!("unknown catalog kind {name}"))
                    })?
                    .kind,
            ),
        })
    }
}

async fn find<R: Owned>(
    write: &mut Write<'_>,
    source: SourceKey,
    tag: Option<TagKind>,
    object: &str,
) -> LibraryResult<Option<R>> {
    write.by_object::<R>(source, tag, object).await
}

/// The entity word a kind uses in source URIs.
fn entity_uri(source_id: &str, kind: &str, object: &str) -> String {
    crate::source_entity_uri(&SourceId::new(source_id), kind, object)
}

/// The media URI a source track observation is stored under.
fn track_uri(source_id: &str, row: &TrackObservation) -> String {
    let direct = row.media_uri.as_deref().and_then(|uri| {
        if crate::cue_media_parts(uri).is_some() || crate::source_entity_parts(uri).is_some() {
            Some(uri.to_owned())
        } else {
            crate::normalize_direct_media_uri(uri)
        }
    });
    match direct {
        Some(uri) if crate::cue_media_parts(&uri).is_none() => {
            match (row.cue_start_millis, row.cue_end_millis) {
                (Some(start), Some(end)) => crate::cue_media_uri(&row.object_id, &uri, start, end),
                _ => uri,
            }
        }
        Some(uri) => uri,
        None => entity_uri(source_id, "track", &row.object_id),
    }
}

/// Artists named by credits. A credit fills the name, sort text and MusicBrainz id it carries,
/// and leaves native artists alone.
pub(crate) async fn index_credits(
    write: &mut Write<'_>,
    source: SourceKey,
    source_id: &str,
    credits: &[&ArtistCredit],
) -> LibraryResult<()> {
    let mut names = BTreeMap::<&str, ArtistMetadata>::new();
    for credit in credits {
        let metadata = names.entry(&credit.object_id).or_default();
        if !credit.name.is_empty() {
            metadata.name.clone_from(&credit.name);
        }
        if credit.sort_text.is_some() {
            metadata.sort_text.clone_from(&credit.sort_text);
        }
        if credit.musicbrainz_artist_id.is_some() {
            metadata
                .musicbrainz_artist_id
                .clone_from(&credit.musicbrainz_artist_id);
        }
    }
    write
        .read_objects::<Artist>(source, None, &names.keys().copied().collect::<Vec<_>>())
        .await?;
    let mut inserted = Vec::new();
    for (object, credit) in names {
        match find::<Artist>(write, source, None, object).await? {
            Some(artist) if artist.native => {}
            Some(mut artist) => {
                let metadata = ArtistMetadata {
                    name: if credit.name.is_empty() {
                        artist.metadata.name.clone()
                    } else {
                        credit.name.clone()
                    },
                    sort_text: credit
                        .sort_text
                        .clone()
                        .or_else(|| artist.metadata.sort_text.clone()),
                    musicbrainz_artist_id: credit
                        .musicbrainz_artist_id
                        .clone()
                        .or_else(|| artist.metadata.musicbrainz_artist_id.clone()),
                };
                if metadata == artist.metadata {
                    continue;
                }
                artist.metadata = metadata;
                // The artist candidate inside the native binding follows the name.
                artist.binding = Some(super::artwork::artist_binding(
                    &artist.metadata.name,
                    artist.metadata.musicbrainz_artist_id.as_deref(),
                    artist.binding.as_deref(),
                )?);
                write.put(artist);
            }
            None => {
                let mut artist =
                    Artist::new(source, object, entity_uri(source_id, "artist", object));
                artist.binding = Some(super::artwork::artist_binding(
                    &credit.name,
                    credit.musicbrainz_artist_id.as_deref(),
                    None,
                )?);
                artist.metadata = credit;
                inserted.push(artist);
            }
        }
    }
    write.insert(inserted).await?;
    Ok(())
}

/// Tags named by tracks and albums. A reference replaces the name of a tag no source
/// observed natively.
pub(crate) async fn index_tags(
    write: &mut Write<'_>,
    source: SourceKey,
    source_id: &str,
    kind: TagKind,
    tags: &[&TagReference],
) -> LibraryResult<()> {
    let objects = tags
        .iter()
        .map(|tag| tag.object_id.as_str())
        .collect::<Vec<_>>();
    write
        .read_objects::<Tag>(source, Some(kind), &objects)
        .await?;
    let mut inserted = Vec::<Tag>::new();
    for tag in tags {
        let metadata = TagMetadata {
            name: tag.name.clone(),
            sort_text: tag.sort_text.clone(),
        };
        if let Some(pending) = inserted
            .iter_mut()
            .find(|pending| pending.object == tag.object_id)
        {
            pending.metadata = metadata;
            continue;
        }
        match find::<Tag>(write, source, Some(kind), &tag.object_id).await? {
            Some(known) if known.native || known.metadata == metadata => {}
            Some(mut known) => {
                known.metadata = metadata;
                write.put(known);
            }
            None => {
                let mut tag = Tag::new(
                    kind,
                    source,
                    &tag.object_id,
                    entity_uri(source_id, kind.table().name, &tag.object_id),
                );
                tag.metadata = metadata;
                inserted.push(tag);
            }
        }
    }
    write.insert(inserted).await?;
    Ok(())
}

/// Credit and tag runs of a track, in metadata order. A repeated credit counts once.
async fn credit_runs(
    write: &mut Write<'_>,
    source: SourceKey,
    metadata: &TrackMetadata,
) -> LibraryResult<(Vec<ArtistKey>, Vec<ArtistKey>, Vec<(TagKind, i64)>)> {
    let mut runs = [Vec::new(), Vec::new()];
    for (run, credits) in runs
        .iter_mut()
        .zip([&metadata.artists, &metadata.album_artists])
    {
        for credit in credits {
            if let Some(artist) = find::<Artist>(write, source, None, &credit.object_id).await? {
                run.push(artist.key);
            }
        }
    }
    let mut tags = Vec::new();
    for table in &TAG_TABLES {
        let mut keys = Vec::new();
        for tag in (table.track_tags)(metadata) {
            if let Some(tag) = find::<Tag>(write, source, Some(table.kind), &tag.object_id).await? {
                keys.push(tag.key);
            }
        }
        tags.extend(dedupe(keys).into_iter().map(|key| (table.kind, key)));
    }
    let [artists, album_artists] = runs;
    Ok((dedupe(artists), dedupe(album_artists), tags))
}

/// Indexes the credits and tags of these tracks, then sets their runs.
async fn index_track_credits(
    write: &mut Write<'_>,
    source: SourceKey,
    source_id: &str,
    tracks: &mut [(Track, TrackMetadata)],
) -> LibraryResult<()> {
    if tracks.is_empty() {
        return Ok(());
    }
    let credits = tracks
        .iter()
        .flat_map(|(_, metadata)| metadata.artists.iter().chain(&metadata.album_artists))
        .collect::<Vec<_>>();
    index_credits(write, source, source_id, &credits).await?;
    for table in &TAG_TABLES {
        let tags = tracks
            .iter()
            .flat_map(|(_, metadata)| (table.track_tags)(metadata))
            .collect::<Vec<_>>();
        index_tags(write, source, source_id, table.kind, &tags).await?;
    }
    for (track, metadata) in tracks {
        (track.artists, track.album_artists, track.tags) =
            credit_runs(write, source, metadata).await?;
    }
    Ok(())
}

/// Tracks whose observed file keeps its picture when an observation names none.
async fn keeps_file_pictures(write: &mut Write<'_>, tracks: &[i64]) -> LibraryResult<HashSet<i64>> {
    if tracks.is_empty() {
        return Ok(HashSet::new());
    }
    let keys: Vec<i64> = sqlx::query_scalar("SELECT track.track_key FROM tracks track JOIN track_details details USING(track_key) JOIN files file ON file.source_key=track.source_key AND file.path=details.source_path WHERE track.track_key IN(SELECT value FROM json_each(?1)) AND(file.artist_pictures IS NULL OR file.picture_index>=0)")
        .bind(serde_json::to_string(tracks)?).fetch_all(write.connection().await?).await?;
    Ok(keys.into_iter().collect())
}

/// Merges source track observations. Authoritative rows replace the observed facts; reference
/// rows patch tracks no source observed natively.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn tracks(
    write: &mut Write<'_>,
    source: SourceKey,
    source_id: &str,
    rows: &[TrackObservation],
    admit: bool,
    authoritative: bool,
    touched: &mut Touched,
) -> LibraryResult<()> {
    for page in rows.chunks(PAGE) {
        track_page(
            write,
            source,
            source_id,
            page,
            admit,
            authoritative,
            touched,
        )
        .await?;
    }
    Ok(())
}

struct Candidate<'r> {
    row: &'r TrackObservation,
    before: Option<Track>,
    track: Track,
    metadata: TrackMetadata,
    /// The details JSON, when it differs from the stored one.
    details: Option<Value>,
}

#[allow(clippy::too_many_arguments)]
async fn track_page(
    write: &mut Write<'_>,
    source: SourceKey,
    source_id: &str,
    page: &[TrackObservation],
    admit: bool,
    authoritative: bool,
    touched: &mut Touched,
) -> LibraryResult<()> {
    let uris = page
        .iter()
        .map(|row| track_uri(source_id, row))
        .collect::<Vec<_>>();
    let albums = page
        .iter()
        .filter_map(|row| row.album_object_id.as_deref())
        .collect::<Vec<_>>();
    write.read_objects::<Album>(source, None, &albums).await?;
    let objects = page
        .iter()
        .map(|row| row.object_id.as_str())
        .collect::<Vec<_>>();
    write.read_objects::<Track>(source, None, &objects).await?;
    write
        .read_naturals::<Track>(&uris.iter().map(String::as_str).collect::<Vec<_>>())
        .await?;
    let mut stubs = Vec::new();
    let mut named = HashSet::new();
    for object in albums {
        if named.insert(object) && find::<Album>(write, source, None, object).await?.is_none() {
            let uri = entity_uri(source_id, "album", object);
            stubs.push(Album::new(source, object, uri));
        }
    }
    write.insert(stubs).await?;

    // A known identity that moved to another URI gives it up first, so the new URI can take it.
    let mut moved = false;
    let mut skipped = HashSet::new();
    for (index, (row, uri)) in page.iter().zip(&uris).enumerate() {
        let Some(known) = find::<Track>(write, source, None, &row.object_id).await? else {
            continue;
        };
        if write.newer(Family::Track, known.key.raw()) || (!authoritative && known.native) {
            skipped.insert(index);
        } else if known.uri != *uri {
            if !admit {
                // Without admission the identity stays where it is.
                skipped.insert(index);
                continue;
            }
            let mut known = known;
            known.source = None;
            known.object = None;
            known.indexed = false;
            touched.tracks.insert(known.key.raw());
            write.put(known);
            moved = true;
        }
    }
    if moved {
        write.connection().await?;
    }

    // A page that repeats an identity or a URI keeps the last row, as an upsert would.
    let mut objects = HashMap::new();
    let mut naturals = HashMap::new();
    for (index, (row, uri)) in page.iter().zip(&uris).enumerate() {
        objects.insert(row.object_id.as_str(), index);
        naturals.insert(uri.as_str(), index);
    }
    let mut candidates = Vec::new();
    for (index, (row, uri)) in page.iter().zip(&uris).enumerate() {
        if skipped.contains(&index)
            || objects[row.object_id.as_str()] != index
            || naturals[uri.as_str()] != index
        {
            continue;
        }
        let before = write.by_natural::<Track>(uri).await?;
        if let Some(old) = &before
            && ((!authoritative && old.native)
                || (old.source == Some(source) && write.newer(Family::Track, old.key.raw())))
        {
            continue;
        }
        let mut track = before
            .clone()
            .unwrap_or_else(|| Track::new(None, None, uri.clone()));
        track.source = Some(source);
        track.object = Some(row.object_id.clone());
        track.album = match row.album_object_id.as_deref() {
            Some(object) => find::<Album>(write, source, None, object)
                .await?
                .map(|album| album.key),
            None => None,
        };
        track.source_path.clone_from(&row.source_path);
        track.cue_path.clone_from(&row.cue_path);
        track.cue_start = row.cue_start_millis;
        track.cue_end = row.cue_end_millis;
        // The audio hash stays while the audio revision does, unless the row knows it.
        let revision = row.audio_revision.map(|revision| revision.to_vec());
        track.audio_hash = match row.audio_hash {
            Some(hash) => Some(hash.to_vec()),
            None if track.audio_revision == revision => track.audio_hash.take(),
            None => None,
        };
        track.audio_revision = revision;
        track.first_seen = track.first_seen.or(row.first_seen_at);
        track.indexed |= admit;
        track.native |= authoritative;
        candidates.push(Candidate {
            row,
            before,
            track,
            metadata: TrackMetadata::default(),
            details: None,
        });
    }

    // Reference rows patch the stored JSON, and authoritative rows keep its unknown fields,
    // so both read it by key.
    let reads = candidates
        .iter()
        .filter_map(|candidate| candidate.before.as_ref().map(|before| before.key.raw()))
        .collect::<Vec<_>>();
    let stored = stored_tracks(write, &reads).await?;
    for candidate in &mut candidates {
        let old = candidate
            .before
            .as_ref()
            .and_then(|before| stored.get(&before.key.raw()));
        let json = if authoritative {
            candidate.metadata = candidate.row.metadata.clone();
            replace_known(
                old.map(|old| old.metadata.clone()),
                serde_json::to_value(&candidate.row.metadata)?,
            )
        } else {
            let mut json = match old {
                Some(old) => old.metadata.clone(),
                None => serde_json::to_value(TrackMetadata::default())?,
            };
            merge_patch(&mut json, &reference_patch(&candidate.row.metadata)?);
            candidate.metadata = parsed(&json);
            json
        };
        candidate.track.facts = TrackFacts::of(&candidate.metadata);
        let details = track_details_value(json);
        if old.is_none_or(|old| old.details != details) {
            candidate.details = Some(details);
        }
    }

    // The observed picture: an authoritative row replaces it, unless the file it came from
    // still has one; a reference row only fills a missing one.
    let cleared = candidates
        .iter()
        .filter(|candidate| {
            authoritative
                && candidate.row.artwork_binding.is_none()
                && candidate
                    .before
                    .as_ref()
                    .is_some_and(|before| before.binding.is_some())
        })
        .filter_map(|candidate| candidate.before.as_ref().map(|before| before.key.raw()))
        .collect::<Vec<_>>();
    let kept = keeps_file_pictures(write, &cleared).await?;
    for candidate in &mut candidates {
        let incoming = candidate.row.artwork_binding.clone();
        let keep = authoritative
            && incoming.is_none()
            && candidate
                .before
                .as_ref()
                .is_some_and(|before| kept.contains(&before.key.raw()));
        if authoritative && !keep {
            candidate.track.binding = incoming;
        } else if !authoritative && candidate.track.binding.is_none() {
            candidate.track.binding = incoming;
        }
    }

    // Credits are indexed only for tracks whose observed facts changed.
    let mut accepted = Vec::new();
    let mut artwork_only = Vec::new();
    for candidate in candidates {
        let changed = match &candidate.before {
            None => true,
            Some(before) => {
                let mut compared = candidate.track.clone();
                compared.binding.clone_from(&before.binding);
                compared != *before || candidate.details.is_some()
            }
        };
        if changed {
            accepted.push(candidate);
        } else if candidate.before.as_ref().map(|before| &before.binding)
            != Some(&candidate.track.binding)
        {
            artwork_only.push(candidate);
        }
    }
    let mut credited = accepted
        .iter()
        .map(|candidate| (candidate.track.clone(), candidate.metadata.clone()))
        .collect::<Vec<_>>();
    index_track_credits(write, source, source_id, &mut credited).await?;
    let mut inserted = Vec::new();
    for (candidate, (mut track, _)) in accepted.into_iter().zip(credited) {
        track.writes.metadata = candidate.details;
        for album in candidate
            .before
            .as_ref()
            .and_then(|before| before.album)
            .into_iter()
            .chain(track.album)
        {
            touched.albums.insert(album.raw());
        }
        match &candidate.before {
            Some(_) => {
                touched.tracks.insert(track.key.raw());
                write.see(&track);
                write.put(track);
            }
            None => inserted.push(track),
        }
    }
    for candidate in artwork_only {
        write.see(&candidate.track);
        write.put(candidate.track);
    }
    for track in write.insert(inserted).await? {
        touched.tracks.insert(track.key.raw());
        write.see(&track);
    }

    // Every current track of the page: its folders, native state and lyrics.
    let mut lyrics = Vec::new();
    for (row, uri) in page.iter().zip(&uris) {
        let Some(mut track) = find::<Track>(write, source, None, &row.object_id).await? else {
            continue;
        };
        if write.newer(Family::Track, track.key.raw()) {
            continue;
        }
        if let Some(folders) = &row.folders {
            let mut keys = Vec::new();
            for folder in folders {
                if let Some(folder) = find::<Folder>(write, source, None, folder).await? {
                    keys.push(folder.key);
                }
            }
            track.folders = dedupe(keys);
        }
        if track.uri == *uri
            && authoritative
            && let Some(payload) = &row.source_lyrics
        {
            lyrics.push((uri.as_str(), payload.as_str()));
        }
        track.state.merge(&row.user_data);
        write.see(&track);
        write.put(track);
    }
    crate::lyrics::put_source_lyrics_on(write, &lyrics).await
}

/// Merges the favorites, ratings and play counts a source reports into its own rows.
async fn native_states(
    write: &mut Write<'_>,
    source: SourceKey,
    kind: &str,
    rows: &[(&str, &NativeUserState)],
) -> LibraryResult<()> {
    async fn merge<R: Owned>(
        write: &mut Write<'_>,
        source: SourceKey,
        rows: &[(&str, &NativeUserState)],
        state: fn(&mut R) -> &mut NativeState,
    ) -> LibraryResult<()> {
        let rows = rows
            .iter()
            .filter(|(_, incoming)| **incoming != NativeUserState::default())
            .collect::<Vec<_>>();
        let objects = rows.iter().map(|(object, _)| *object).collect::<Vec<_>>();
        write.read_objects::<R>(source, None, &objects).await?;
        for (object, incoming) in rows {
            let Some(mut record) = find::<R>(write, source, None, object).await? else {
                continue;
            };
            if write.newer(R::FAMILY, record.key()) {
                continue;
            }
            state(&mut record).merge(incoming);
            write.put(record);
        }
        Ok(())
    }
    match Kind::parse(kind)? {
        Kind::Track => merge::<Track>(write, source, rows, |track| &mut track.state).await,
        Kind::Album => merge::<Album>(write, source, rows, |album| &mut album.state).await,
        Kind::Artist => merge::<Artist>(write, source, rows, |artist| &mut artist.state).await,
        _ => Ok(()),
    }
}

/// One observed header of an album, artist, tag or folder.
pub(crate) struct Header<'r, M> {
    pub(crate) object: &'r str,
    pub(crate) metadata: &'r M,
    pub(crate) artwork: Option<&'r [u8]>,
}

/// Merges observed headers of one family and their pictures. A native header replaces the
/// stored one; a reference header patches records no source observed natively.
#[allow(clippy::too_many_arguments)]
async fn headers<R: Owned>(
    write: &mut Write<'_>,
    source: SourceKey,
    source_id: &str,
    tag: Option<TagKind>,
    word: &'static str,
    rows: &[Header<'_, R::Metadata>],
    admit: bool,
    native: bool,
    touched: &mut Touched,
) -> LibraryResult<()> {
    // A batch that repeats a header keeps the last one, as an upsert would.
    let mut last = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        last.insert(row.object, index);
    }
    let rows = rows
        .iter()
        .enumerate()
        .filter(|(index, row)| last[row.object] == *index)
        .map(|(_, row)| row)
        .collect::<Vec<_>>();
    for page in rows.chunks(PAGE) {
        let objects = page.iter().map(|row| row.object).collect::<Vec<_>>();
        write.read_objects::<R>(source, tag, &objects).await?;
        let mut current = Vec::new();
        for row in page {
            let record = find::<R>(write, source, tag, row.object).await?;
            current.push(match record {
                Some(record)
                    if !write.newer(R::FAMILY, record.key()) && (native || !record.native()) =>
                {
                    Ok(record)
                }
                other => Err(other),
            });
        }
        // Album headers are not in the row, so their stored headers are read by key.
        let keys = current
            .iter()
            .filter_map(|record| record.as_ref().ok())
            .filter(|record| record.header().is_none())
            .map(|record| record.key())
            .collect::<Vec<_>>();
        let stored = stored_json(write, "albums", "album_key", "input_metadata", &keys).await?;
        let mut inserted = Vec::new();
        for (row, current) in page.iter().zip(current) {
            let (mut record, header, mut changed) = match current {
                // A live change or native facts own this record.
                Err(Some(record)) => {
                    if !write.newer(R::FAMILY, record.key()) {
                        write.see(&record);
                    }
                    continue;
                }
                Err(None) => {
                    let mut json = serde_json::to_value(row.metadata)?;
                    if !native {
                        json = serde_json::to_value(R::Metadata::default())?;
                        merge_patch(&mut json, &reference_patch(row.metadata)?);
                    }
                    let mut record = R::create(
                        source,
                        tag,
                        row.object,
                        entity_uri(source_id, word, row.object),
                    );
                    record.set_indexed(admit);
                    record.set_native(native);
                    (record, Some(json), true)
                }
                Ok(record) => {
                    let before = record.clone();
                    let stored = record
                        .header()
                        .or_else(|| stored.get(&record.key()).cloned());
                    let header = if native {
                        let json = serde_json::to_value(row.metadata)?;
                        (!R::keeps_header(row.metadata) && stored.as_ref() != Some(&json))
                            .then_some(json)
                    } else {
                        let mut json = stored.clone().unwrap_or_else(|| Value::Object(Map::new()));
                        merge_patch(&mut json, &reference_patch(row.metadata)?);
                        (stored.as_ref() != Some(&json)).then_some(json)
                    };
                    let mut record = record;
                    record.set_indexed(before.indexed() || admit);
                    record.set_native(before.native() || native);
                    let changed = header.is_some()
                        || record.indexed() != before.indexed()
                        || record.native() != before.native();
                    (record, header, changed)
                }
            };
            let header_changed = header.is_some();
            if let Some(json) = header {
                record.set_header(&parsed(&json), json);
            }
            let picture = record.picture(row.artwork, native)?;
            let next = if native || R::BUILT_PICTURE {
                picture
            } else {
                record.binding().map(<[u8]>::to_vec).or(picture)
            };
            if next.as_deref() != record.binding() {
                record.set_binding(next);
                changed = true;
            }
            if !changed {
                write.see(&record);
                continue;
            }
            if record.key() == 0 {
                inserted.push(record);
            } else {
                record.touch(touched, header_changed);
                write.see(&record);
                write.put(record);
            }
        }
        for record in write.insert(inserted).await? {
            record.touch(touched, true);
            write.see(&record);
        }
    }
    Ok(())
}

/// Folder membership that folder observations list, page by page. The observation keeps the
/// listed pairs, so completion removes members it did not list.
async fn memberships(
    write: &mut Write<'_>,
    source: SourceKey,
    folders: &[crate::FolderObservation],
) -> LibraryResult<Vec<(i64, i64)>> {
    let mut listed = Vec::new();
    for folder in folders.iter().filter(|folder| !folder.tracks.is_empty()) {
        let Some(record) = find::<Folder>(write, source, None, &folder.object_id).await? else {
            continue;
        };
        if write.newer(Family::Folder, record.key.raw()) {
            continue;
        }
        for object in &folder.tracks {
            let Some(mut track) = find::<Track>(write, source, None, object).await? else {
                continue;
            };
            listed.push((record.key.raw(), track.key.raw()));
            if !track.folders.contains(&record.key) {
                track.folders.push(record.key);
                write.put(track);
            }
        }
    }
    Ok(listed)
}

/// Resolves the albums of what a batch staged.
pub(crate) async fn resolve(write: &mut Write<'_>, touched: &mut Touched) -> LibraryResult<()> {
    if touched.tracks.is_empty() && touched.albums.is_empty() && touched.artists.is_empty() {
        return Ok(());
    }
    let mut albums = touched.albums.clone();
    write
        .read_keys::<Track>(&touched.tracks.iter().copied().collect::<Vec<_>>())
        .await?;
    for key in &touched.tracks {
        if let Some(album) = write
            .get::<Track>(*key)
            .await?
            .and_then(|track| track.album)
        {
            albums.insert(album.raw());
        }
    }
    // An artist's name shows in the albums that credit it.
    if !touched.artists.is_empty() {
        let credited: Vec<i64> = sqlx::query_scalar("SELECT album_key FROM album_links WHERE role=0 AND target IN(SELECT value FROM json_each(?1)) UNION SELECT track.album_key FROM track_links link JOIN tracks track USING(track_key) WHERE link.role=1 AND link.target IN(SELECT value FROM json_each(?1)) AND track.album_key IS NOT NULL")
            .bind(serde_json::to_string(&touched.artists)?)
            .fetch_all(write.connection().await?)
            .await?;
        albums.extend(credited);
    }
    super::albums::resolve(write, &albums).await?;
    Ok(())
}

/// Applies one provider batch in one Write.
pub(crate) async fn batch(
    write: &mut Write<'_>,
    source: SourceKey,
    source_id: &str,
    batch: &CatalogBatch,
    admit: bool,
    touched: &mut Touched,
) -> LibraryResult<Vec<(i64, i64)>> {
    let native = !batch.reference;
    let albums = batch
        .albums
        .iter()
        .map(|row| Header {
            object: &row.object_id,
            metadata: &row.metadata,
            artwork: row.artwork_binding.as_deref(),
        })
        .collect::<Vec<_>>();
    headers::<Album>(
        write, source, source_id, None, "album", &albums, admit, native, touched,
    )
    .await?;
    let artists = batch
        .artists
        .iter()
        .map(|row| Header {
            object: &row.object_id,
            metadata: &row.metadata,
            artwork: row.artwork_binding.as_deref(),
        })
        .collect::<Vec<_>>();
    headers::<Artist>(
        write, source, source_id, None, "artist", &artists, admit, native, touched,
    )
    .await?;
    for table in &TAG_TABLES {
        let tags = batch
            .tags
            .iter()
            .filter(|row| row.kind == table.kind)
            .map(|row| Header {
                object: &row.object_id,
                metadata: &row.metadata,
                artwork: row.artwork_binding.as_deref(),
            })
            .collect::<Vec<_>>();
        headers::<Tag>(
            write,
            source,
            source_id,
            Some(table.kind),
            table.name,
            &tags,
            admit,
            native,
            touched,
        )
        .await?;
    }
    let folders = batch
        .folders
        .iter()
        .map(|row| Header {
            object: &row.object_id,
            metadata: &row.metadata,
            artwork: row.artwork_binding.as_deref(),
        })
        .collect::<Vec<_>>();
    headers::<Folder>(
        write, source, source_id, None, "folder", &folders, admit, native, touched,
    )
    .await?;
    tracks(
        write,
        source,
        source_id,
        &batch.tracks,
        admit,
        native,
        touched,
    )
    .await?;
    let albums = batch
        .albums
        .iter()
        .map(|row| (row.object_id.as_str(), &row.user_data))
        .collect::<Vec<_>>();
    native_states(write, source, "album", &albums).await?;
    let artists = batch
        .artists
        .iter()
        .map(|row| (row.object_id.as_str(), &row.user_data))
        .collect::<Vec<_>>();
    native_states(write, source, "artist", &artists).await?;
    // Album headers name credits and tags too, and resolution reads them.
    let credits = batch
        .albums
        .iter()
        .flat_map(|album| &album.metadata.album_artists)
        .collect::<Vec<_>>();
    index_credits(write, source, source_id, &credits).await?;
    for table in &TAG_TABLES {
        if let Some(tags) = table.album_tags {
            let tags = batch
                .albums
                .iter()
                .flat_map(|album| tags(&album.metadata))
                .collect::<Vec<_>>();
            index_tags(write, source, source_id, table.kind, &tags).await?;
        }
    }
    // Resolution reads the names of credited artists and tags, so the batch's albums follow
    // a changed one. Changed headers and tracks touched their own albums already.
    if write.staged::<Artist>() || write.staged::<Tag>() {
        for album in &batch.albums {
            if let Some(record) = find::<Album>(write, source, None, &album.object_id).await? {
                touched.albums.insert(record.key.raw());
            }
        }
    }
    let listed = memberships(write, source, &batch.folders).await?;
    super::collections::playlists(write, source, &batch.playlists, admit, batch.reference).await?;
    resolve(write, touched).await?;
    Ok(listed)
}

/// Withdraws records a provider reports as removed, unless a live change wrote them since the
/// walk began.
async fn withdraw<R: Owned>(
    write: &mut Write<'_>,
    source: SourceKey,
    tag: Option<TagKind>,
    ids: &[String],
) -> LibraryResult<Vec<R>> {
    let objects = ids.iter().map(String::as_str).collect::<Vec<_>>();
    write.read_objects::<R>(source, tag, &objects).await?;
    let mut withdrawn = Vec::new();
    for id in ids {
        let Some(mut record) = find::<R>(write, source, tag, id).await? else {
            continue;
        };
        if write.newer(R::FAMILY, record.key()) {
            continue;
        }
        write.see(&record);
        if record.indexed() {
            record.set_indexed(false);
            withdrawn.push(record.clone());
            write.put(record);
        }
    }
    Ok(withdrawn)
}

/// Withdraws records a provider reports as removed.
pub(crate) async fn remove_items(
    write: &mut Write<'_>,
    source: SourceKey,
    kind: &str,
    ids: &[String],
    touched: &mut Touched,
) -> LibraryResult<()> {
    match Kind::parse(kind)? {
        Kind::Playlist => {
            let rows: Vec<i64> = sqlx::query_scalar("SELECT playlist_key FROM playlists WHERE source_key=?1 AND object_id IN(SELECT value FROM json_each(?2))")
                .bind(source).bind(serde_json::to_string(ids)?).fetch_all(write.connection().await?).await?;
            let current = rows
                .into_iter()
                .filter(|key| !write.newer(Family::Playlist, *key))
                .collect::<Vec<_>>();
            withdraw_playlists(write, &current).await?;
            for key in current {
                write.see_playlist(key, source);
            }
        }
        Kind::Track => {
            let tracks = withdraw::<Track>(write, source, None, ids).await?;
            withdrawn_tracks(&tracks, touched).await?;
        }
        Kind::Album => {
            let albums = withdraw::<Album>(write, source, None, ids).await?;
            touched
                .albums
                .extend(albums.iter().map(|album| album.key.raw()));
        }
        Kind::Artist => {
            let artists = withdraw::<Artist>(write, source, None, ids).await?;
            touched
                .artists
                .extend(artists.iter().map(|artist| artist.key.raw()));
        }
        Kind::Tag(tag) => {
            withdraw::<Tag>(write, source, Some(tag), ids).await?;
        }
        Kind::Folder => {
            withdraw::<Folder>(write, source, None, ids).await?;
        }
    }
    Ok(())
}

/// Hides provider playlists.
async fn withdraw_playlists(write: &mut Write<'_>, keys: &[i64]) -> LibraryResult<()> {
    let rows: Vec<super::write::PlaylistRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!("UPDATE playlists SET indexed=0 WHERE indexed=1 AND playlist_key IN(SELECT value FROM json_each(?1)) RETURNING {}", super::write::PLAYLIST_COLUMNS)))
        .bind(serde_json::to_string(keys)?).fetch_all(write.connection().await?).await?;
    for row in rows {
        write.put_playlist(row);
    }
    Ok(())
}

/// The albums of withdrawn tracks are resolved again.
async fn withdrawn_tracks(tracks: &[Track], touched: &mut Touched) -> LibraryResult<()> {
    let albums = tracks
        .iter()
        .filter_map(|track| track.album)
        .collect::<Vec<_>>();
    touched
        .albums
        .extend(albums.iter().map(|album| album.raw()));
    touched
        .tracks
        .extend(tracks.iter().map(|track| track.key.raw()));
    Ok(())
}

/// Completion withdraws a source's records of one kind that the walk did not see.
async fn complete_kind(
    write: &mut Write<'_>,
    kind: Kind,
    keys: &[i64],
    touched: &mut Touched,
) -> LibraryResult<()> {
    async fn withdraw_unseen<R: Owned>(
        write: &mut Write<'_>,
        keys: &[i64],
    ) -> LibraryResult<Vec<R>> {
        let mut withdrawn = Vec::new();
        write.read_keys::<R>(keys).await?;
        for key in keys {
            // A Write that committed since the keys were read may have seen the row.
            if write.saw(R::FAMILY, *key) {
                continue;
            }
            if let Some(mut record) = write.get::<R>(*key).await?.filter(Owned::indexed) {
                record.set_indexed(false);
                withdrawn.push(record.clone());
                write.put(record);
            }
        }
        Ok(withdrawn)
    }
    match kind {
        Kind::Track => {
            let tracks = withdraw_unseen::<Track>(write, keys).await?;
            withdrawn_tracks(&tracks, touched).await?;
        }
        Kind::Album => touched.albums.extend(
            withdraw_unseen::<Album>(write, keys)
                .await?
                .iter()
                .map(|album| album.key.raw()),
        ),
        // Album resolution never reads whether an artist is indexed.
        Kind::Artist => {
            withdraw_unseen::<Artist>(write, keys).await?;
        }
        Kind::Tag(_) => {
            withdraw_unseen::<Tag>(write, keys).await?;
        }
        Kind::Folder => {
            withdraw_unseen::<Folder>(write, keys).await?;
        }
        Kind::Playlist => {
            let keys = keys
                .iter()
                .copied()
                .filter(|key| !write.saw(Family::Playlist, *key))
                .collect::<Vec<_>>();
            withdraw_playlists(write, &keys).await?;
        }
    }
    Ok(())
}

/// Forgets a source's tracks that were hidden longer than the expiry and that no playlist
/// entry, queue entry or download holds. Albums, artists, tags and folders the expired tracks
/// named go with them when nothing else names them.
async fn expire(write: &mut Write<'_>, source: SourceKey) -> LibraryResult<bool> {
    let cutoff = chrono::Utc::now().timestamp() - EXPIRY_SECONDS;
    let expired: Vec<i64> = sqlx::query_scalar("SELECT track_key FROM tracks track WHERE source_key=?1 AND indexed=0 AND hidden_since<?2
            AND NOT EXISTS(SELECT 1 FROM playlist_entries entry WHERE entry.track_key=track.track_key)
            AND NOT EXISTS(SELECT 1 FROM queue_entries entry WHERE entry.track_key=track.track_key)
            AND NOT EXISTS(SELECT 1 FROM downloads download WHERE download.media_uri=track.media_uri)
            ORDER BY track_key LIMIT ?3")
        .bind(source)
        .bind(cutoff)
        .bind(COMPLETION_PAGE as i64)
        .fetch_all(write.connection().await?)
        .await?;
    if expired.is_empty() {
        return Ok(false);
    }
    let (mut albums, mut artists, mut tags, mut folders) = (
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    write.read_keys::<Track>(&expired).await?;
    for key in &expired {
        if let Some(track) = write.get::<Track>(*key).await? {
            albums.extend(track.album.map(AlbumKey::raw));
            artists.extend(
                track
                    .artists
                    .iter()
                    .chain(&track.album_artists)
                    .map(|artist| artist.raw()),
            );
            tags.extend(track.tags.iter().map(|(_, tag)| *tag));
            folders.extend(track.folders.iter().map(|folder| folder.raw()));
        }
        write.remove::<Track>(*key);
    }
    let empty = |table: &'static str, key: &'static str, named: &'static str| {
        format!(
            "SELECT {key} FROM {table} WHERE {key} IN(SELECT value FROM json_each(?1)) AND NOT({named})"
        )
    };
    let empty_albums: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(empty(
        "albums",
        "album_key",
        "EXISTS(SELECT 1 FROM tracks WHERE tracks.album_key=albums.album_key)",
    )))
    .bind(serde_json::to_string(&albums)?)
    .fetch_all(write.connection().await?)
    .await?;
    for key in empty_albums {
        write.remove::<Album>(key);
    }
    let empty_artists: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(empty(
        "artists",
        "artist_key",
        "EXISTS(SELECT 1 FROM track_links WHERE role IN(0,1) AND target=artists.artist_key) OR EXISTS(SELECT 1 FROM album_links WHERE role=0 AND target=artists.artist_key)",
    )))
    .bind(serde_json::to_string(&artists)?)
    .fetch_all(write.connection().await?)
    .await?;
    for key in empty_artists {
        write.remove::<Artist>(key);
    }
    let empty_tags: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(empty(
        "tags",
        "tag_key",
        "EXISTS(SELECT 1 FROM track_links WHERE role IN(2,3) AND target=tags.tag_key) OR EXISTS(SELECT 1 FROM album_links WHERE role=2 AND target=tags.tag_key)",
    )))
    .bind(serde_json::to_string(&tags)?)
    .fetch_all(write.connection().await?)
    .await?;
    for key in empty_tags {
        write.remove::<Tag>(key);
    }
    let empty_folders: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(empty(
        "folders",
        "folder_key",
        "EXISTS(SELECT 1 FROM track_links WHERE role=4 AND target=folders.folder_key)",
    )))
    .bind(serde_json::to_string(&folders)?)
    .fetch_all(write.connection().await?)
    .await?;
    for key in empty_folders {
        write.remove::<Folder>(key);
    }
    Ok(true)
}

impl Catalog {
    /// Keys a completion of this kind withdraws: the source's listed records the walk did not
    /// see.
    async fn unseen_keys(&self, source: SourceKey, kind: Kind, walk: u64) -> Vec<i64> {
        self.loaded().await;
        let snapshot = self.snapshot();
        let keys = match kind {
            Kind::Track => snapshot.indexed_keys(&snapshot.tracks, source, |_| true),
            Kind::Album => snapshot.indexed_keys(&snapshot.albums, source, |_| true),
            Kind::Artist => {
                // An artist stays while an indexed track or album still credits it.
                let credited = snapshot.credited_artists(source);
                snapshot.indexed_keys(&snapshot.artists, source, |artist| {
                    !credited.contains(&i64::from(artist.key))
                })
            }
            Kind::Tag(tag) => {
                snapshot.indexed_keys(&snapshot.tags, source, |record| record.kind() == tag)
            }
            Kind::Folder => snapshot.indexed_keys(&snapshot.folders, source, |_| true),
            Kind::Playlist => snapshot.indexed_keys(&snapshot.playlists, source, |playlist| {
                playlist.flags & super::snapshot::SOURCE_OWNED != 0
            }),
        };
        drop(snapshot);
        let family = match kind {
            Kind::Track => Family::Track,
            Kind::Album => Family::Album,
            Kind::Artist => Family::Artist,
            Kind::Tag(_) => Family::Tag,
            Kind::Folder => Family::Folder,
            Kind::Playlist => Family::Playlist,
        };
        let mut keys = self.unseen(walk, family, keys);
        keys.sort_unstable();
        keys
    }

    /// Writes a source's own reading of an unsourced file track.
    pub async fn write_direct_track_metadata(
        &self,
        uri: &str,
        metadata: &TrackMetadata,
    ) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let existing = write.by_natural::<Track>(uri).await?;
        let details = track_details_value(serde_json::to_value(metadata)?);
        if let Some(track) = &existing {
            if track.source.is_some() {
                return Ok(false);
            }
            let stored = stored_tracks(&mut write, &[track.key.raw()]).await?;
            if track.native
                && track.facts == TrackFacts::of(metadata)
                && stored
                    .get(&track.key.raw())
                    .is_some_and(|stored| stored.details == details)
            {
                return Ok(false);
            }
        }
        let mut track = existing
            .clone()
            .unwrap_or_else(|| Track::new(None, None, uri.to_owned()));
        track.set_header(metadata, details);
        track.native = true;
        if existing.is_some() {
            write.put(track);
        } else {
            write.insert(vec![track]).await?;
        }
        write.commit().await?;
        Ok(true)
    }

    pub async fn reconcile_source(
        &self,
        source_id: &SourceId,
        kind: &str,
        name: &str,
    ) -> LibraryResult<CachedSource> {
        // A source whose row already matches needs no Write.
        self.loaded().await;
        if let Some(source) = self
            .snapshot()
            .sources
            .values()
            .find(|source| source.object == source_id.as_str())
            .filter(|source| source.kind.as_deref() == Some(kind) && source.name == name)
        {
            return Ok(CachedSource {
                source: source.key,
                object_id: source.object.clone(),
                display_name: source.name.clone(),
                catalog_complete: source.complete,
            });
        }
        let mut write = self.write().await?;
        let key = write.source_identity(source_id.as_str()).await?;
        let mut source = write
            .get::<Source>(key.raw())
            .await?
            .ok_or_else(|| LibraryError::InvalidStore("source identity has no row".into()))?;
        let reconciled = Source {
            kind: Some(kind.to_owned()),
            name: name.to_owned(),
            ..source.clone()
        };
        if reconciled != source {
            source = reconciled;
            write.put(source.clone());
        }
        write.commit().await?;
        Ok(CachedSource {
            source: source.key,
            object_id: source.object,
            display_name: source.name,
            catalog_complete: source.complete,
        })
    }

    /// Forgets a source in one Write: its catalog, favorites, ratings, lyrics, download records,
    /// queue entries, Home rows and playlists go. A track a Rufin playlist still holds stays as
    /// a hidden row without a source. Listens stay, since they carry their own copy.
    pub async fn remove_source(&self, source_id: &SourceId) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let key = write
            .source_by_object(source_id.as_str())
            .await?
            .map(|source| source.key);
        let prefixes = serde_json::to_string(
            &crate::keys::source_entity_kinds()
                .map(|kind| crate::keys::source_entity_prefix(source_id, kind))
                .collect::<Vec<_>>(),
        )?;
        // URIs under the source's entity prefixes, or of its tracks.
        let owned = "(EXISTS(SELECT 1 FROM json_each(?1) prefix WHERE substr(media_uri,1,length(prefix.value))=prefix.value) OR media_uri IN(SELECT media_uri FROM tracks WHERE source_key=?2))";
        let states: Vec<super::write::StateRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "DELETE FROM user_state WHERE {owned}{}",
            super::write::STATE_DELETED
        )))
        .bind(&prefixes)
        .bind(key)
        .fetch_all(write.connection().await?)
        .await?;
        write.set_states(states);
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM outbox WHERE {owned}"
        )))
        .bind(&prefixes)
        .bind(key)
        .execute(write.connection().await?)
        .await?;
        let lyrics: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "DELETE FROM lyrics_cache WHERE {owned} RETURNING lyrics_key"
        )))
        .bind(&prefixes)
        .bind(key)
        .fetch_all(write.connection().await?)
        .await?;
        crate::lyrics::unindex_lyrics(write.connection().await?, &lyrics).await?;
        sqlx::query("DELETE FROM downloads WHERE source_id=?1")
            .bind(source_id.as_str())
            .execute(write.connection().await?)
            .await?;
        let Some(source) = key else {
            write.keep();
            write.commit().await?;
            return Ok(false);
        };
        self.close_walks(source);
        let connection = write.connection().await?;
        // The queue loses the source's tracks; the saved position follows the entry it named.
        let queued = "track_key IN(SELECT track_key FROM tracks WHERE source_key=?1)";
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "WITH play AS(SELECT row_number() OVER(ORDER BY CASE WHEN (SELECT shuffled FROM queue_state WHERE singleton=1) THEN coalesce(shuffle_rank,rank) ELSE rank END,entry_id)-1 position,{queued} gone FROM queue_entries)
            UPDATE queue_state SET
                current_index=(SELECT CASE WHEN count(*) FILTER(WHERE NOT gone AND position<queue_state.current_index)<count(*) FILTER(WHERE NOT gone) THEN count(*) FILTER(WHERE NOT gone AND position<queue_state.current_index) END FROM play),
                progress_millis=CASE WHEN EXISTS(SELECT 1 FROM play WHERE gone AND position=queue_state.current_index) THEN 0 ELSE progress_millis END
            WHERE singleton=1 AND current_index IS NOT NULL AND EXISTS(SELECT 1 FROM play WHERE gone)"
        )))
        .bind(source)
        .execute(&mut *connection)
        .await?;
        let contexts: Vec<Option<i64>> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "DELETE FROM queue_entries WHERE {queued} RETURNING context_key"
        )))
        .bind(source)
        .fetch_all(&mut *connection)
        .await?;
        if !contexts.is_empty() {
            sqlx::query("DELETE FROM queue_contexts WHERE context_key IN(SELECT value FROM json_each(?1)) AND NOT EXISTS(SELECT 1 FROM queue_entries WHERE context_key=queue_contexts.context_key)")
                .bind(serde_json::to_string(&contexts)?)
                .execute(&mut *connection)
                .await?;
            write.changed(Scope::Queue);
        }
        let connection = write.connection().await?;
        // Provider playlists go first, so their entries hold none of the source's tracks.
        sqlx::query("DELETE FROM playlists WHERE source_key=?1")
            .bind(source)
            .execute(&mut *connection)
            .await?;
        let kept: Vec<(i64, String)> = sqlx::query_as("UPDATE tracks SET source_key=NULL,object_id=NULL,album_key=NULL,indexed=0,hidden_since=CASE WHEN indexed THEN unixepoch() ELSE hidden_since END
            WHERE source_key=?1 AND EXISTS(SELECT 1 FROM playlist_entries entry WHERE entry.track_key=tracks.track_key)
            RETURNING track_key,media_uri")
            .bind(source)
            .fetch_all(&mut *connection)
            .await?;
        // Links name their targets without a foreign key, so they go with the source's records.
        for (links, roles, table, column) in [
            ("track_links", "0,1", "artists", "artist_key"),
            ("track_links", "2,3", "tags", "tag_key"),
            ("track_links", "4", "folders", "folder_key"),
            ("album_links", "0", "artists", "artist_key"),
            ("album_links", "2", "tags", "tag_key"),
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {links} WHERE role IN({roles}) AND target IN(SELECT {column} FROM {table} WHERE source_key=?1)")))
                .bind(source)
                .execute(&mut *connection)
                .await?;
        }
        sqlx::query("DELETE FROM tracks WHERE source_key=?1")
            .bind(source)
            .execute(&mut *connection)
            .await?;
        // Albums, artists, tags, folders, file rows, locators and Home rows go with the source.
        sqlx::query("DELETE FROM sources WHERE source_key=?1")
            .bind(source)
            .execute(&mut *connection)
            .await?;
        // Kept tracks lose their carried-over counts and their downloads.
        let uris = kept.iter().map(|(_, uri)| uri.as_str()).collect::<Vec<_>>();
        write.recount(&uris).await?;
        write.read_downloads(&uris).await?;
        write.forget_source(
            source,
            kept.iter()
                .map(|(key, _)| TrackKey::from_raw(*key))
                .collect(),
        );
        write.commit().await?;
        Ok(true)
    }

    /// The source's configuration changed: open walks stop and the next refresh lists
    /// everything again.
    pub async fn invalidate_source_observation(&self, source_id: &SourceId) -> LibraryResult<()> {
        let mut write = self.write().await?;
        if let Some(mut source) = write.source_by_object(source_id.as_str()).await? {
            self.close_walks(source.key);
            if source.freshness.is_some() {
                source.freshness = None;
                write.put(source);
            }
        }
        write.commit().await?;
        Ok(())
    }
}

impl Write<'_> {
    /// Remembers these references, reading their tracks first in one statement per kind.
    pub(crate) async fn put_references(
        &mut self,
        references: &[TrackReference],
    ) -> LibraryResult<Vec<TrackKey>> {
        let mut objects = BTreeMap::<String, Vec<String>>::new();
        for reference in references {
            if let Some((source, kind, object)) = crate::source_entity_parts(&reference.media_uri)
                && kind == "track"
            {
                objects
                    .entry(source.as_str().to_owned())
                    .or_default()
                    .push(object);
            }
        }
        for (source, objects) in objects {
            let source = self.source_identity(&source).await?;
            let objects = objects.iter().map(String::as_str).collect::<Vec<_>>();
            self.read_objects::<Track>(source, None, &objects).await?;
        }
        let uris = references
            .iter()
            .map(|reference| reference.media_uri.as_str())
            .collect::<Vec<_>>();
        self.read_naturals::<Track>(&uris).await?;
        let mut keys = Vec::with_capacity(references.len());
        for reference in references {
            keys.push(self.put_reference(reference).await?);
        }
        Ok(keys)
    }

    /// Remembers a track some other record names: a playlist entry, a queue item or a listen.
    /// It patches only tracks no source observed natively.
    pub(crate) async fn put_reference(
        &mut self,
        reference: &TrackReference,
    ) -> LibraryResult<TrackKey> {
        let mut uri = reference.media_uri.clone();
        let mut identity = None;
        if let Some((source, kind, object)) = crate::source_entity_parts(&uri)
            && kind == "track"
        {
            let source = self.source_identity(source.as_str()).await?;
            if let Some(known) = find::<Track>(self, source, None, &object).await? {
                uri = known.uri;
            }
            identity = Some((source, object));
        }
        let existing = self.by_natural::<Track>(&uri).await?;
        if reference.metadata == TrackMetadata::default()
            && reference.artwork_binding.is_none()
            && let Some(existing) = &existing
        {
            return Ok(existing.key);
        }
        let patch = reference_patch(&reference.metadata)?;
        let artwork = reference.artwork_binding.clone();
        let (mut track, json, stored) = match existing {
            Some(track) if track.native => return Ok(track.key),
            Some(mut track) => {
                let stored = stored_tracks(self, &[track.key.raw()])
                    .await?
                    .remove(&track.key.raw());
                let old = stored.as_ref().map_or_else(
                    || Value::Object(Map::new()),
                    |stored| stored.metadata.clone(),
                );
                let mut json = old.clone();
                merge_patch(&mut json, &patch);
                let binding = track.binding.clone().or(artwork);
                if json == old && binding == track.binding {
                    return Ok(track.key);
                }
                track.binding = binding;
                (track, json, stored.map(|stored| stored.details))
            }
            None => {
                let (source, object) = identity.map_or((None, None), |(source, object)| {
                    (Some(source), Some(object))
                });
                let mut track = Track::new(source, object, uri.clone());
                track.binding = artwork;
                (track, patch, None)
            }
        };
        let metadata = parsed::<TrackMetadata>(&json);
        track.facts = TrackFacts::of(&metadata);
        let details = track_details_value(json);
        if stored.as_ref() != Some(&details) {
            track.writes.metadata = Some(details);
        }
        if let (Some(source), Some(_)) = (track.source, track.object.as_deref()) {
            let source_id = self
                .get::<Source>(source.raw())
                .await?
                .map(|source| source.object)
                .unwrap_or_default();
            let mut credited = vec![(track, metadata)];
            index_track_credits(self, source, &source_id, &mut credited).await?;
            track = credited.remove(0).0;
        }
        Ok(if track.key.raw() == 0 {
            self.insert(vec![track]).await?.remove(0).key
        } else {
            let key = track.key;
            self.put(track);
            key
        })
    }
}

/// One Home row of a source section.
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
struct HomeEntry {
    position: i64,
    kind: String,
    entity: i64,
    title: String,
    subtitle: String,
    section_title: Option<String>,
}

impl Observation<'_> {
    /// Starts a walk: a listing whose completion hides what it did not see. Returns the stamp
    /// the walk's file rows carry.
    pub async fn start_observation(&mut self) -> LibraryResult<()> {
        if let Some(walk) = self.walk.take() {
            self.catalog.close_walk(walk);
        }
        self.memberships.clear();
        self.walk = Some(self.catalog.open_walk(self.source));
        Ok(())
    }

    /// Whether this observation changed what the catalog lists show.
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// Commits one Write of this observation.
    pub(crate) async fn commit(&mut self, write: Write<'_>) -> LibraryResult<()> {
        if let Some(change) = write.commit().await? {
            self.changed |= change.scopes.iter().any(|scope| {
                matches!(
                    scope,
                    Scope::Reload | Scope::Tracks { .. } | Scope::Collections { .. }
                )
            });
        }
        Ok(())
    }

    pub async fn apply_catalog(&mut self, batch: &CatalogBatch, admit: bool) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let mut touched = Touched::default();
        let listed = self::batch(
            &mut write,
            self.source,
            self.source_id,
            batch,
            admit,
            &mut touched,
        )
        .await?;
        self.commit(write).await?;
        for (folder, track) in listed {
            self.memberships.entry(folder).or_default().insert(track);
        }
        Ok(())
    }

    /// Completes the walk for these kinds: what it did not see is hidden. With no kinds the
    /// whole listing completed: the source is marked complete with its freshness and sync time,
    /// and once a day its tracks hidden longer than the expiry are forgotten.
    pub async fn complete_source_observation(
        &mut self,
        kinds: &[&str],
        freshness: Option<&Freshness>,
        synced_at: Option<i64>,
    ) -> LibraryResult<()> {
        let walk = self.walk()?;
        for name in kinds {
            let kind = Kind::parse(name)?;
            let keys = self.catalog.unseen_keys(self.source, kind, walk).await;
            for page in keys.chunks(COMPLETION_PAGE) {
                let mut write = self.write().await?;
                let mut touched = Touched::default();
                complete_kind(&mut write, kind, page, &mut touched).await?;
                resolve(&mut write, &mut touched).await?;
                self.commit(write).await?;
            }
        }
        if kinds.is_empty() {
            if self
                .catalog
                .expiry_due(self.source, chrono::Utc::now().timestamp())
            {
                loop {
                    let mut write = self.write().await?;
                    if !expire(&mut write, self.source).await? {
                        break;
                    }
                    self.commit(write).await?;
                }
            }
            let mut write = self.write().await?;
            if let Some(mut source) = write.get::<Source>(self.source.raw()).await? {
                source.complete = true;
                source.freshness = freshness.map(|marker| marker.as_bytes().to_vec());
                source.synced_at = synced_at.or(source.synced_at);
                write.put(source);
            }
            // Clients hear of a completion only when the walk changed something.
            if self.changed {
                write.changed(Scope::Source {
                    key: self.source,
                    phase: SourcePhase::Completed,
                });
            }
            write.commit().await?;
        }
        Ok(())
    }

    /// A catch-up that is not a listing advances the sync time.
    pub async fn set_synced_at(&mut self, at: i64) -> LibraryResult<()> {
        let mut write = self.write().await?;
        if let Some(mut source) = write.get::<Source>(self.source.raw()).await? {
            source.synced_at = Some(at);
            write.put(source);
        }
        write.commit().await?;
        Ok(())
    }

    pub async fn remove_source_items(&mut self, kind: &str, ids: &[String]) -> LibraryResult<()> {
        for page in ids.chunks(PAGE) {
            let mut write = self.write().await?;
            let mut touched = Touched::default();
            remove_items(&mut write, self.source, kind, page, &mut touched).await?;
            resolve(&mut write, &mut touched).await?;
            self.commit(write).await?;
        }
        Ok(())
    }

    /// Indexed tracks of an album that the walk has not seen yet.
    pub async fn omitted_album_track_page(
        &self,
        id: &str,
        after: Option<&str>,
        limit: usize,
    ) -> LibraryResult<Vec<String>> {
        let walk = self.walk()?;
        self.catalog.loaded().await;
        let limit = limit.min(128);
        let mut connection = self.catalog.acquire_reader().await?;
        let mut after = after.unwrap_or_default().to_owned();
        let mut omitted = Vec::new();
        loop {
            let page: Vec<(i64, String)> = sqlx::query_as("SELECT track.track_key,track.object_id FROM tracks track JOIN albums album USING(album_key) WHERE track.source_key=?1 AND album.object_id=?2 AND track.indexed=1 AND track.object_id>?3 ORDER BY track.object_id LIMIT 128")
                .bind(self.source).bind(id).bind(&after).fetch_all(&mut *connection).await?;
            let Some((_, last)) = page.last() else {
                return Ok(omitted);
            };
            after.clone_from(last);
            let keys = page.iter().map(|(key, _)| *key).collect();
            let unseen = self.catalog.unseen(walk, Family::Track, keys);
            for (key, object) in page {
                if unseen.contains(&key) {
                    omitted.push(object);
                    if omitted.len() == limit {
                        return Ok(omitted);
                    }
                }
            }
        }
    }

    /// Members of an album the walk did not list leave it.
    pub async fn complete_album_observation(&mut self, id: &str) -> LibraryResult<()> {
        self.walk()?;
        loop {
            let mut write = self.write().await?;
            let Some(album) = find::<Album>(&mut write, self.source, None, id).await? else {
                return Ok(());
            };
            let members: Vec<i64> = sqlx::query_scalar("SELECT track_key FROM tracks WHERE source_key=?1 AND album_key=?2 ORDER BY track_key")
                .bind(self.source).bind(album.key).fetch_all(write.connection().await?).await?;
            let omitted = members
                .into_iter()
                .filter(|key| !write.saw(Family::Track, *key))
                .take(COMPLETION_PAGE)
                .collect::<Vec<_>>();
            if omitted.is_empty() {
                return Ok(());
            }
            let mut touched = Touched::default();
            write.read_keys::<Track>(&omitted).await?;
            for key in omitted {
                let Some(mut track) = write.get::<Track>(key).await? else {
                    continue;
                };
                touched.tracks.insert(key);
                track.album = None;
                write.see(&track);
                write.put(track);
            }
            touched.albums.insert(album.key.raw());
            resolve(&mut write, &mut touched).await?;
            self.commit(write).await?;
        }
    }

    /// Tracks the walk did not list in a folder leave it.
    pub async fn complete_source_folder_membership(&mut self, id: &str) -> LibraryResult<()> {
        self.walk()?;
        let mut write = self.write().await?;
        let Some(folder) = find::<Folder>(&mut write, self.source, None, id).await? else {
            return Ok(());
        };
        let members: Vec<i64> =
            sqlx::query_scalar("SELECT track_key FROM track_links WHERE role=?1 AND target=?2")
                .bind(crate::schema::LINK_FOLDER)
                .bind(folder.key)
                .fetch_all(write.connection().await?)
                .await?;
        let listed = self
            .memberships
            .remove(&folder.key.raw())
            .unwrap_or_default();
        let members = members
            .into_iter()
            .filter(|key| !listed.contains(key) && !write.newer(Family::Track, *key))
            .collect::<Vec<_>>();
        write.read_keys::<Track>(&members).await?;
        for key in members {
            if let Some(mut track) = write.get::<Track>(key).await? {
                track.folders.retain(|member| *member != folder.key);
                write.put(track);
            }
        }
        self.commit(write).await?;
        Ok(())
    }

    pub async fn replace_source_home_section(
        &mut self,
        section: &str,
        title: Option<&str>,
        entries: &[HomeEntryInput],
    ) -> LibraryResult<()> {
        self.write_source_home_page(section, title, entries, -1, true)
            .await
    }

    /// One page of a Home section. Entries in the page's position window that the page does
    /// not list go; listed entries whose item is known come in.
    pub(crate) async fn write_source_home_page(
        &mut self,
        section: &str,
        title: Option<&str>,
        entries: &[HomeEntryInput],
        after: i64,
        last: bool,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let source = self.source;
        let playlists = entries
            .iter()
            .filter(|entry| entry.kind == crate::HomeEntryKind::Playlist)
            .map(|entry| entry.entity_object_id.as_str())
            .collect::<Vec<_>>();
        let playlists: HashMap<String, i64> = if playlists.is_empty() {
            HashMap::new()
        } else {
            sqlx::query_as::<_, (String, i64)>("SELECT object_id,playlist_key FROM playlists WHERE source_key=?1 AND object_id IN(SELECT value FROM json_each(?2))")
                .bind(source).bind(serde_json::to_string(&playlists)?).fetch_all(write.connection().await?).await?
                .into_iter().collect()
        };
        let mut expected = Vec::new();
        for entry in entries {
            let object = entry.entity_object_id.as_str();
            let entity = match entry.kind {
                crate::HomeEntryKind::Track => find::<Track>(&mut write, source, None, object)
                    .await?
                    .map(|track| track.key.raw()),
                crate::HomeEntryKind::Album => find::<Album>(&mut write, source, None, object)
                    .await?
                    .map(|album| album.key.raw()),
                crate::HomeEntryKind::Artist => find::<Artist>(&mut write, source, None, object)
                    .await?
                    .map(|artist| artist.key.raw()),
                crate::HomeEntryKind::Playlist => playlists.get(object).copied(),
            };
            if let Some(entity) = entity {
                expected.push(HomeEntry {
                    position: entry.position,
                    kind: entry.kind.as_str().to_owned(),
                    entity,
                    title: entry.title.clone(),
                    subtitle: entry.subtitle.clone(),
                    section_title: title.map(str::to_owned),
                });
            }
        }
        let end = (!last)
            .then(|| entries.last().map(|entry| entry.position))
            .flatten();
        let current: Vec<HomeEntry> = sqlx::query_as("SELECT position,entity_kind kind,entity_key entity,title,subtitle,section_title FROM home_entries WHERE source_key=?1 AND section_id=?2 ORDER BY position")
            .bind(source).bind(section).fetch_all(write.connection().await?).await?;
        let mut next = current
            .iter()
            .filter(|entry| {
                entry.position <= after
                    || end.is_some_and(|end| entry.position > end)
                    || expected.contains(entry)
            })
            .cloned()
            .collect::<Vec<_>>();
        for entry in expected {
            if !next.iter().any(|known| known.position == entry.position) {
                next.push(entry);
            }
        }
        next.sort_by_key(|entry| entry.position);
        if next != current {
            let connection = write.connection().await?;
            sqlx::query("DELETE FROM home_entries WHERE source_key=?1 AND section_id=?2")
                .bind(source)
                .bind(section)
                .execute(&mut *connection)
                .await?;
            for entry in &next {
                sqlx::query("INSERT INTO home_entries(source_key,section_id,position,entity_kind,entity_key,title,subtitle,section_title) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")
                    .bind(source).bind(section).bind(entry.position).bind(&entry.kind).bind(entry.entity)
                    .bind(&entry.title).bind(&entry.subtitle).bind(&entry.section_title)
                    .execute(&mut *connection).await?;
            }
            write.changed(Scope::Home { source });
        }
        self.commit(write).await?;
        Ok(())
    }

    pub async fn complete_source_home_sections(
        &mut self,
        prefix: &str,
        sections: &[String],
        retained_prefixes: &[String],
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let known: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT section_id FROM home_entries WHERE source_key=?1")
                .bind(self.source)
                .fetch_all(write.connection().await?)
                .await?;
        for section in known {
            if section.starts_with(prefix)
                && !sections.contains(&section)
                && !retained_prefixes
                    .iter()
                    .any(|retained| section.starts_with(retained.as_str()))
            {
                sqlx::query("DELETE FROM home_entries WHERE source_key=?1 AND section_id=?2")
                    .bind(self.source)
                    .bind(&section)
                    .execute(write.connection().await?)
                    .await?;
                write.changed(Scope::Home {
                    source: self.source,
                });
            }
        }
        self.commit(write).await?;
        Ok(())
    }

    pub async fn write_native_user_data(
        &mut self,
        kind: &str,
        rows: &[(String, NativeUserState)],
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let rows = rows
            .iter()
            .map(|(object, state)| (object.as_str(), state))
            .collect::<Vec<_>>();
        native_states(&mut write, self.source, kind, &rows).await?;
        self.commit(write).await?;
        Ok(())
    }

    /// One Write: the pictures found in files (path, cover index, artist pictures) and the
    /// artwork of albums, tracks or artists.
    pub async fn set_source_artwork(
        &mut self,
        images: &[(String, Option<i64>, String)],
        bindings: &[(&str, &[(String, Option<Vec<u8>>)])],
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        if !images.is_empty() {
            let recorded = sqlx::query("UPDATE files SET picture_index=json_extract(item.value,'$[1]'),artist_pictures=json_extract(item.value,'$[2]') FROM json_each(?2) item WHERE files.source_key=?1 AND files.path=json_extract(item.value,'$[0]') AND(files.picture_index,files.artist_pictures) IS NOT(json_extract(item.value,'$[1]'),json_extract(item.value,'$[2]'))")
                .bind(self.source).bind(serde_json::to_string(images)?).execute(write.connection().await?).await?.rows_affected();
            // File rows are not resident and no client shows them.
            if recorded != 0 {
                write.keep();
            }
        }
        for (kind, rows) in bindings {
            pictures(&mut write, self.source, kind, rows).await?;
        }
        self.commit(write).await?;
        Ok(())
    }
}

/// Native pictures a source reports apart from headers, as local artwork scans do.
pub(crate) async fn pictures(
    write: &mut Write<'_>,
    source: SourceKey,
    kind: &str,
    rows: &[(String, Option<Vec<u8>>)],
) -> LibraryResult<()> {
    async fn entity<R: Owned>(
        write: &mut Write<'_>,
        source: SourceKey,
        tag: Option<TagKind>,
        rows: &[(String, Option<Vec<u8>>)],
    ) -> LibraryResult<()> {
        for (object, artwork) in rows {
            let Some(mut record) = find::<R>(write, source, tag, object).await? else {
                continue;
            };
            if write.newer(R::FAMILY, record.key()) {
                continue;
            }
            let picture = record.picture(artwork.as_deref(), true)?;
            if picture.as_deref() == record.binding() {
                continue;
            }
            record.set_binding(picture);
            write.see(&record);
            write.put(record);
        }
        Ok(())
    }
    match Kind::parse(kind)? {
        Kind::Track => {
            let mut tracks = Vec::new();
            for (object, artwork) in rows {
                if let Some(track) = find::<Track>(write, source, None, object).await?
                    && !write.newer(Family::Track, track.key.raw())
                {
                    tracks.push((track, artwork));
                }
            }
            let cleared = tracks
                .iter()
                .filter(|(track, artwork)| artwork.is_none() && track.binding.is_some())
                .map(|(track, _)| track.key.raw())
                .collect::<Vec<_>>();
            let kept = keeps_file_pictures(write, &cleared).await?;
            for (mut track, artwork) in tracks {
                if artwork.is_none() && kept.contains(&track.key.raw()) {
                    continue;
                }
                if *artwork == track.binding {
                    continue;
                }
                track.binding.clone_from(artwork);
                write.see(&track);
                write.put(track);
            }
            Ok(())
        }
        Kind::Album => entity::<Album>(write, source, None, rows).await,
        Kind::Artist => entity::<Artist>(write, source, None, rows).await,
        Kind::Tag(tag) => entity::<Tag>(write, source, Some(tag), rows).await,
        Kind::Folder => entity::<Folder>(write, source, None, rows).await,
        Kind::Playlist => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AlbumMetadata, ArtistCredit, FolderMetadata, HomeEntryKind};

    /// Memory must equal a fresh load.
    async fn assert_reloads(catalog: &Catalog) {
        let fresh = super::super::load::snapshot(catalog.path()).unwrap();
        assert_eq!(catalog.snapshot().export(), fresh.export());
    }

    async fn scalar<T>(catalog: &Catalog, sql: &'static str) -> T
    where
        T: for<'row> sqlx::Decode<'row, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
    {
        sqlx::query_scalar(sql)
            .fetch_one(&mut *catalog.acquire_reader().await.unwrap())
            .await
            .unwrap()
    }

    fn track(object: &str, title: &str) -> TrackObservation {
        let credit = ArtistCredit {
            object_id: "a1".into(),
            name: "A".into(),
            ..Default::default()
        };
        TrackObservation {
            object_id: object.into(),
            metadata: TrackMetadata {
                title: title.into(),
                artist: "A".into(),
                // A repeated credit counts once.
                artists: vec![credit.clone(), credit],
                genres: vec![TagReference {
                    object_id: "rock".into(),
                    name: "Rock".into(),
                    ..Default::default()
                }],
                moods: vec![TagReference {
                    object_id: "calm".into(),
                    name: "Calm".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            folders: Some(vec!["f1".into(), "missing".into()]),
            artwork_binding: Some(br#"[{"image":{"kind":"embedded"}}]"#.to_vec()),
            user_data: NativeUserState {
                favorite: Some(true),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// The critical flow for the Catalog's writes: sync, change, reference batch, Home,
    /// removal, reference puts, completion, expiry, aliases and source removal each leave
    /// memory equal to a fresh load.
    #[tokio::test]
    async fn provider_merges_equal_a_reload() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("library.sqlite3"))
            .await
            .unwrap();
        catalog
            .reconcile_source(&SourceId::new("source"), "subsonic", "Source")
            .await
            .unwrap();
        let mut observation = catalog.observe("source").await.unwrap();
        observation.start_observation().await.unwrap();
        let batch = CatalogBatch {
            folders: vec![crate::FolderObservation {
                object_id: "f1".into(),
                metadata: FolderMetadata {
                    name: Some("F".into()),
                    sort_text: None,
                },
                ..Default::default()
            }],
            artists: vec![crate::ArtistObservation {
                object_id: "a2".into(),
                metadata: ArtistMetadata {
                    name: "B".into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
            tags: vec![crate::TagObservation {
                kind: TagKind::Mood,
                object_id: "calm".into(),
                metadata: TagMetadata {
                    name: "Calm".into(),
                    sort_text: None,
                },
                artwork_binding: None,
            }],
            // An observed header owns the first album; file tags derive the second.
            albums: vec![crate::AlbumObservation {
                object_id: "al1".into(),
                metadata: AlbumMetadata {
                    title: "First".into(),
                    artist: "A".into(),
                    year: Some(2001),
                    date_added: Some("2024-02-03".into()),
                    musicbrainz_release_group_id: Some("rg1".into()),
                    album_artists: vec![ArtistCredit {
                        object_id: "a1".into(),
                        name: "A".into(),
                        ..Default::default()
                    }],
                    genres: vec![TagReference {
                        object_id: "rock".into(),
                        name: "Rock".into(),
                        ..Default::default()
                    }],
                    release_types: vec!["album".into()],
                    ..Default::default()
                },
                artwork_binding: Some(br#"[{"image":{"kind":"album"}}]"#.to_vec()),
                ..Default::default()
            }],
            tracks: vec![
                TrackObservation {
                    album_object_id: Some("al1".into()),
                    ..track("t1", "One")
                },
                TrackObservation {
                    album_object_id: Some("al1".into()),
                    ..track("t2", "Two")
                },
                {
                    let mut third = track("t3", "Three");
                    third.album_object_id = Some("al2".into());
                    third.source_path = Some("/music/second/03.flac".into());
                    third.metadata.album = "Second".into();
                    third.metadata.release_date = Some("2003-04-05".into());
                    third.metadata.musicbrainz_album_id = Some("r2".into());
                    third.metadata.album_artists = vec![ArtistCredit {
                        object_id: "a2".into(),
                        name: "B".into(),
                        ..Default::default()
                    }];
                    third.metadata.album_tags = Some(crate::AlbumTags {
                        sort_text: "second".into(),
                        is_compilation: None,
                        release_types: vec!["EP".into(), "ep".into()],
                    });
                    third
                },
            ],
            ..Default::default()
        };
        observation.apply_catalog(&batch, true).await.unwrap();
        assert_reloads(&catalog).await;
        assert_eq!(
            scalar::<Option<String>>(
                &catalog,
                "SELECT release_identity FROM albums WHERE object_id='al1'"
            )
            .await
            .as_deref(),
            Some("release-group:rg1")
        );
        assert_eq!(
            scalar::<String>(
                &catalog,
                "SELECT json(metadata->'$.release_types') FROM albums WHERE object_id='al2'"
            )
            .await,
            r#"["EP","ep"]"#
        );

        // Applying the same batch again writes nothing.
        let revision = catalog.snapshot().revision;
        observation.apply_catalog(&batch, true).await.unwrap();
        assert_eq!(catalog.snapshot().revision, revision);

        // The header changes, a track moves to the derived album and loses its picture.
        let mut changed = batch.clone();
        changed.albums[0].metadata.title = "First (deluxe)".into();
        changed.tracks[0].metadata.title = "One (edit)".into();
        changed.tracks[1].artwork_binding = None;
        changed.tracks[1].album_object_id = Some("al2".into());
        changed.tracks[2].metadata.artists.clear();
        observation.apply_catalog(&changed, true).await.unwrap();
        assert_reloads(&catalog).await;
        assert_eq!(
            scalar::<String>(&catalog, "SELECT title FROM albums WHERE object_id='al1'").await,
            "First (deluxe)"
        );

        // A lookup result for the current identity replaces the release types.
        let (album, source): (i64, i64) =
            sqlx::query_as("SELECT album_key,source_key FROM albums WHERE object_id='al1'")
                .fetch_one(&mut *catalog.acquire_reader().await.unwrap())
                .await
                .unwrap();
        assert!(
            catalog
                .accept_album_release_result(
                    SourceKey::from_raw(source),
                    AlbumKey::from_raw(album),
                    "release-group:rg1",
                    crate::AlbumReleaseResult::Found {
                        release_types: vec!["Compilation".into()],
                    },
                )
                .await
                .unwrap()
                .is_some()
        );
        assert_reloads(&catalog).await;

        let reference = CatalogBatch {
            reference: true,
            tracks: vec![track("t4", "Reference")],
            ..Default::default()
        };
        observation.apply_catalog(&reference, true).await.unwrap();
        observation
            .replace_source_home_section(
                "recent",
                Some("Recent"),
                &[HomeEntryInput {
                    position: 0,
                    kind: HomeEntryKind::Track,
                    entity_object_id: "t1".into(),
                    title: "One".into(),
                    subtitle: "A".into(),
                }],
            )
            .await
            .unwrap();
        observation
            .remove_source_items("track", &["t2".into()])
            .await
            .unwrap();
        assert_reloads(&catalog).await;

        let mut write = catalog.write().await.unwrap();
        write
            .put_reference(&TrackReference {
                media_uri: "file:///music/x.flac".into(),
                metadata: TrackMetadata {
                    title: "X".into(),
                    ..Default::default()
                },
                artwork_binding: None,
            })
            .await
            .unwrap();
        write
            .put_reference(&TrackReference {
                media_uri: crate::source_entity_uri(&SourceId::new("source"), "track", "t9"),
                metadata: TrackMetadata {
                    title: "Nine".into(),
                    artists: vec![ArtistCredit {
                        object_id: "a3".into(),
                        name: "C".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                artwork_binding: None,
            })
            .await
            .unwrap();
        write.commit().await.unwrap();
        assert_reloads(&catalog).await;

        drop(observation);
        let mut later = catalog.observe("source").await.unwrap();
        later.start_observation().await.unwrap();
        later
            .apply_catalog(
                &CatalogBatch {
                    tracks: vec![track("t1", "One (edit)")],
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        later
            .complete_source_observation(
                &["track", "artist", "genre", "mood", "folder"],
                None,
                None,
            )
            .await
            .unwrap();
        assert_reloads(&catalog).await;
        assert_eq!(
            scalar::<String>(
                &catalog,
                "SELECT group_concat(object_id) FROM tracks WHERE indexed=1"
            )
            .await,
            "t1"
        );

        // Hidden tracks past the expiry go with the album nothing else holds, unless a
        // playlist holds them.
        let held = crate::source_entity_uri(&SourceId::new("source"), "track", "t4");
        catalog
            .create_playlist(
                "Held",
                &crate::MemberSelection::Uris(vec![held.clone()].into()),
            )
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE tracks SET hidden_since=0 WHERE indexed=0")
            .execute(&mut *catalog.writer().await.unwrap())
            .await
            .unwrap();
        later
            .complete_source_observation(&[], None, None)
            .await
            .unwrap();
        assert_reloads(&catalog).await;
        assert_eq!(
            scalar::<String>(
                &catalog,
                "SELECT group_concat(object_id ORDER BY object_id) FROM tracks WHERE source_key IS NOT NULL"
            )
            .await,
            "t1,t4"
        );
        assert_eq!(
            scalar::<i64>(
                &catalog,
                "SELECT count(*) FROM albums WHERE object_id='al2'"
            )
            .await,
            0
        );

        // An alias renames a track, or folds it into the row that holds the new URI.
        let mut write = later.write().await.unwrap();
        super::super::files::merge_local_file_uris_on(
            &mut write,
            &[
                (
                    "file:///music/x.flac".into(),
                    "file:///music/y.flac".into(),
                    "y".into(),
                ),
                (
                    crate::source_entity_uri(&SourceId::new("source"), "track", "t1"),
                    "file:///music/y.flac".into(),
                    "t1".into(),
                ),
            ],
            &mut Touched::default(),
        )
        .await
        .unwrap();
        later.commit(write).await.unwrap();
        assert_reloads(&catalog).await;
        let (folded, indexed, object): (i64, bool, Option<String>) = sqlx::query_as(
            "SELECT track_key,indexed,object_id FROM tracks WHERE media_uri='file:///music/y.flac'",
        )
        .fetch_one(&mut *catalog.acquire_reader().await.unwrap())
        .await
        .unwrap();
        assert!(indexed);
        assert_eq!(object.as_deref(), Some("t1"));
        assert_eq!(
            scalar::<i64>(
                &catalog,
                "SELECT entity_key FROM home_entries WHERE section_id='recent'"
            )
            .await,
            folded
        );

        // A track that moves to another URI leaves its old row without a source, still naming
        // the source's album, artists, tags and folders.
        for uri in [
            "file:///music/y.flac",
            "https://example.test/one-moved.flac",
        ] {
            let moved = TrackObservation {
                media_uri: Some(uri.into()),
                album_object_id: Some("al1".into()),
                ..track("t1", "One (edit)")
            };
            later
                .apply_catalog(
                    &CatalogBatch {
                        tracks: vec![moved],
                        ..Default::default()
                    },
                    true,
                )
                .await
                .unwrap();
            assert_reloads(&catalog).await;
        }
        let (orphan_source, orphan_album): (Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT source_key,album_key FROM tracks WHERE media_uri='file:///music/y.flac'",
        )
        .fetch_one(&mut *catalog.acquire_reader().await.unwrap())
        .await
        .unwrap();
        assert!(orphan_source.is_none() && orphan_album.is_some());

        // Forgetting the source keeps the tracks playlists hold as hidden rows without a
        // source, and drops everything else.
        let moved = "https://example.test/one-moved.flac";
        catalog
            .create_playlist(
                "Moved",
                &crate::MemberSelection::Uris(vec![moved.to_owned()].into()),
            )
            .await
            .unwrap()
            .unwrap();
        drop(later);
        assert!(
            catalog
                .remove_source(&SourceId::new("source"))
                .await
                .unwrap()
        );
        assert_reloads(&catalog).await;
        assert_eq!(
            scalar::<String>(
                &catalog,
                "SELECT group_concat(media_uri ORDER BY media_uri) FROM tracks WHERE media_uri NOT LIKE 'file:%'"
            )
            .await,
            format!("{moved},{held}")
        );
        let snapshot = catalog.snapshot();
        assert!(snapshot.source(SourceKey::from_raw(source)).is_none());
        assert_eq!(snapshot.albums.iter().count(), 0);
        assert_eq!(snapshot.artists.iter().count(), 0);
        assert_eq!(snapshot.tags.iter().count(), 0);
        assert!(snapshot.tracks.iter().all(|(_, track)| {
            use super::super::snapshot::Packed;
            let links = snapshot.track_links(track);
            snapshot.source_key(track.source()).is_none()
                && !track.indexed()
                && snapshot.albums.live(track.album).is_none()
                && links.artists.is_empty()
                && links.tags.is_empty()
                && links.folders.is_empty()
        }));
    }
}
