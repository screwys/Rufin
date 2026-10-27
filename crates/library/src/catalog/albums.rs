//! Album resolution. An album takes its credits, release types and resolved metadata from its
//! observed header, or else from its indexed members in path, disc and track order. Member
//! facts that are not in the track rows are read by key, once per page of albums.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{Map, Value, json};

use super::merge::{dedupe, merge_patch, parsed};
use super::records::*;
use super::write::Write;
use crate::metadata::album_details_value;
use crate::tags::TagKind;
use crate::{AlbumMetadata, AlbumTags, ArtistCredit, LibraryResult, TagReference};

const PAGE: usize = 128;

/// An album member: its row and the facts the details row holds.
struct Member {
    track: Track,
    release: Option<String>,
    group: Option<String>,
    album_tags: Option<AlbumTags>,
}

/// The stored metadata, header and accepted release lookup of an album.
#[derive(Default)]
struct Stored {
    metadata: Option<Value>,
    input: Option<Value>,
    accepted: Option<(String, Vec<String>)>,
}

/// Resolves these albums again.
pub(crate) async fn resolve(write: &mut Write<'_>, keys: &BTreeSet<i64>) -> LibraryResult<()> {
    let keys = keys.iter().copied().collect::<Vec<_>>();
    for page in keys.chunks(PAGE) {
        let mut albums = Vec::new();
        for key in page {
            if let Some(album) = write.get::<Album>(*key).await? {
                albums.push(album);
            }
        }
        if albums.is_empty() {
            continue;
        }
        let mut members = members(write, page).await?;
        let mut stored = stored(write, &albums).await?;
        for album in albums {
            let key = album.key.raw();
            resolve_album(
                write,
                album,
                &members.remove(&key).unwrap_or_default(),
                stored.remove(&key).unwrap_or_default(),
            )
            .await?;
        }
    }
    Ok(())
}

/// Resolves every album, a page per Write.
pub(crate) async fn resolve_all(catalog: &crate::Catalog) -> LibraryResult<()> {
    catalog.loaded().await;
    let keys = {
        use super::snapshot::Packed;
        let snapshot = catalog.snapshot();
        snapshot
            .albums
            .iter()
            .map(|(_, album)| album.key())
            .collect::<Vec<_>>()
    };
    for page in keys.chunks(super::merge::COMPLETION_PAGE) {
        let mut write = catalog.write().await?;
        resolve(&mut write, &page.iter().copied().collect()).await?;
        write.commit().await?;
    }
    Ok(())
}

/// Every member of these albums in member order, with the facts resolution reads.
async fn members(
    write: &mut Write<'_>,
    albums: &[i64],
) -> LibraryResult<HashMap<i64, Vec<Member>>> {
    type Row = (i64, i64, Option<String>, Option<String>, Option<String>);
    let rows: Vec<Row> = sqlx::query_as("SELECT track.album_key,track.track_key,details.metadata->>'$.musicbrainz_album_id',details.metadata->>'$.musicbrainz_release_group_id',json(details.metadata->'$.album_tags') FROM tracks track LEFT JOIN track_details details USING(track_key) WHERE track.album_key IN(SELECT value FROM json_each(?1))")
        .bind(serde_json::to_string(albums)?)
        .fetch_all(write.connection().await?)
        .await?;
    let mut members = HashMap::<i64, Vec<Member>>::new();
    for (album, key, release, group, album_tags) in rows {
        if let Some(track) = write.get::<Track>(key).await? {
            members.entry(album).or_default().push(Member {
                track,
                release,
                group,
                album_tags: album_tags.and_then(|tags| serde_json::from_str(&tags).ok()),
            });
        }
    }
    for members in members.values_mut() {
        members.sort_by(|left, right| order(&left.track).cmp(&order(&right.track)));
    }
    Ok(members)
}

fn order(track: &Track) -> (Option<&str>, i64, i64, Option<&str>, i64) {
    (
        track.member_path(),
        track.facts.disc,
        track.facts.number,
        track.object.as_deref(),
        track.key.raw(),
    )
}

/// Metadata, headers and accepted lookups of these albums.
async fn stored(write: &mut Write<'_>, albums: &[Album]) -> LibraryResult<HashMap<i64, Stored>> {
    let keys = albums
        .iter()
        .map(|album| album.key.raw())
        .collect::<Vec<_>>();
    let rows: Vec<(i64, Option<String>, Option<String>, Option<String>, Option<String>)> = sqlx::query_as("SELECT album.album_key,json(album.metadata),json(album.input_metadata),accepted.lookup_identity,json(accepted.release_types) FROM albums album LEFT JOIN album_release_observations accepted USING(album_key) WHERE album.album_key IN(SELECT value FROM json_each(?1))")
        .bind(serde_json::to_string(&keys)?)
        .fetch_all(write.connection().await?)
        .await?;
    let mut stored = HashMap::new();
    for (key, metadata, input, lookup, types) in rows {
        let accepted = match (lookup, types) {
            (Some(lookup), Some(types)) => Some((lookup, serde_json::from_str(&types)?)),
            _ => None,
        };
        stored.insert(
            key,
            Stored {
                metadata: metadata.and_then(|metadata| serde_json::from_str(&metadata).ok()),
                input: input
                    .map(|input| serde_json::from_str(&input))
                    .transpose()?,
                accepted,
            },
        );
    }
    Ok(stored)
}

async fn resolve_album(
    write: &mut Write<'_>,
    mut album: Album,
    members: &[Member],
    stored: Stored,
) -> LibraryResult<()> {
    let indexed = members
        .iter()
        .filter(|member| member.track.indexed)
        .collect::<Vec<_>>();
    let tagged = |member: &&Member| member.album_tags.is_some();
    let mut extra = Map::new();
    // A cached local header lasts while an indexed member has no album tags of its own.
    if album.cached_local && indexed.iter().all(tagged) {
        album.input = false;
        album.indexed = false;
        album.cached_local = false;
        extra.insert("input_metadata".into(), Value::Null);
        extra.insert("accepted_audio".into(), Value::Null);
    }
    // Observed file tags own the album even when its header is derived.
    if !album.native && !album.cached_local && indexed.iter().any(tagged) {
        album.native = true;
        album.input = false;
        extra.insert("input_metadata".into(), Value::Null);
    }
    // A file source lists an album only through its indexed members, whatever header an
    // imported Store gave it.
    if indexed.is_empty()
        && write
            .get::<Source>(album.source.raw())
            .await?
            .is_some_and(|source| source.file_source())
    {
        album.indexed = false;
    }
    let input = album.input.then_some(stored.input).flatten();
    let header = input.as_ref().map(parsed::<AlbumMetadata>);
    let first = |fact: fn(&Member) -> Option<&str>| {
        indexed
            .iter()
            .find_map(|member| fact(member))
            .map(str::to_owned)
    };
    let filled = |fact: fn(&Member) -> Option<&str>| {
        indexed
            .iter()
            .filter_map(|member| fact(member))
            .find(|text| !text.is_empty())
            .map(str::to_owned)
    };

    let source = album.source;
    album.artists = match &header {
        Some(header) => {
            let mut keys = Vec::new();
            for credit in &header.album_artists {
                if let Some(artist) = write
                    .by_object::<Artist>(source, None, &credit.object_id)
                    .await?
                {
                    keys.push(artist.key);
                }
            }
            dedupe(keys)
        }
        None => dedupe(
            indexed
                .iter()
                .flat_map(|member| member.track.album_artists.iter().copied()),
        ),
    };
    album.genres = match &header {
        Some(header) => {
            let mut keys = Vec::new();
            for tag in &header.genres {
                if let Some(tag) = write
                    .by_object::<Tag>(source, Some(TagKind::Genre), &tag.object_id)
                    .await?
                {
                    keys.push(tag.key);
                }
            }
            dedupe(keys)
        }
        None => dedupe(indexed.iter().flat_map(|member| {
            member
                .track
                .tags
                .iter()
                .filter(|(kind, _)| *kind == TagKind::Genre)
                .map(|(_, key)| *key)
        })),
    };
    let mut credited = Vec::new();
    for key in &album.artists {
        if let Some(artist) = write.get::<Artist>(key.raw()).await? {
            credited.push(artist);
        }
    }
    let sort_text = |artist: &Artist| {
        artist
            .metadata
            .sort_text
            .clone()
            .unwrap_or_else(|| artist.metadata.name.to_lowercase())
    };

    // A lookup result counts only for the identity it was made for.
    let (group, release) = match &header {
        Some(header) => (
            header.musicbrainz_release_group_id.clone(),
            header.musicbrainz_release_id.clone(),
        ),
        None => (
            first(|member| member.group.as_deref()),
            first(|member| member.release.as_deref()),
        ),
    };
    let identity = group
        .map(|id| format!("release-group:{id}"))
        .or_else(|| release.map(|id| format!("release:{id}")));
    album.release_lookup_identity = album
        .release_lookup_identity
        .take()
        .filter(|lookup| identity.as_ref() == Some(lookup));
    album.release_identity = identity;
    let accepted = stored
        .accepted
        .filter(|(lookup, _)| album.release_lookup_identity.as_ref() == Some(lookup))
        .map(|(_, types)| types);
    let mut release_types = match (accepted, &header) {
        (Some(types), _) => types,
        (None, Some(header)) => header.release_types.clone(),
        (None, None) => {
            let mut known = HashSet::new();
            indexed
                .iter()
                .filter_map(|member| member.album_tags.as_ref())
                .flat_map(|tags| &tags.release_types)
                .filter(|kind| known.insert(kind.as_str()))
                .cloned()
                .collect()
        }
    };
    // The Store keeps no empty release type.
    release_types.retain(|kind| !kind.is_empty());

    let leader = indexed.first();
    let added = header
        .as_ref()
        .and_then(|header| header.date_added.as_deref())
        .and_then(unix_time)
        .or(album.first_seen)
        .or_else(|| {
            leader.and_then(|member| {
                added(
                    member.track.facts.date_added.as_deref(),
                    member.track.first_seen,
                )
            })
        });

    let mut resolved = match input {
        Some(input) => input,
        None => serde_json::to_value(AlbumMetadata {
            title: filled(|member| Some(member.track.facts.album.as_str())).unwrap_or_default(),
            artist: credited
                .iter()
                .map(|artist| artist.metadata.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            sort_text: filled(|member| Some(member.album_tags.as_ref()?.sort_text.as_str()))
                .unwrap_or_default(),
            year: indexed.iter().find_map(|member| member.track.facts.year),
            release_date: filled(|member| member.track.facts.release_date.as_deref()),
            date_added: None,
            musicbrainz_release_id: filled(|member| member.release.as_deref()),
            musicbrainz_release_group_id: filled(|member| member.group.as_deref()),
            is_compilation: indexed
                .iter()
                .find_map(|member| member.album_tags.as_ref()?.is_compilation),
            album_artists: credited
                .iter()
                .map(|artist| ArtistCredit {
                    object_id: artist.object.clone(),
                    name: artist.metadata.name.clone(),
                    sort_text: Some(sort_text(artist)),
                    musicbrainz_artist_id: artist.metadata.musicbrainz_artist_id.clone(),
                })
                .collect(),
            genres: {
                let mut genres = Vec::new();
                for key in &album.genres {
                    if let Some(tag) = write.get::<Tag>(*key).await? {
                        genres.push(TagReference {
                            sort_text: Some(
                                tag.metadata
                                    .sort_text
                                    .unwrap_or_else(|| tag.metadata.name.to_lowercase()),
                            ),
                            object_id: tag.object,
                            name: tag.metadata.name,
                        });
                    }
                }
                genres
            },
            release_types: Vec::new(),
        })?,
    };
    // Kept to the second: the album's date added orders albums, and readers show its date.
    let date_added = added
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        .map(|time| time.format("%Y-%m-%dT%H:%M:%S").to_string());
    merge_patch(
        &mut resolved,
        &json!({"date_added": date_added, "release_types": release_types}),
    );
    let metadata = parsed::<AlbumMetadata>(&resolved);
    album.facts = AlbumFacts::of(&metadata);
    let details = album_details_value(resolved);
    if stored.metadata.as_ref() != Some(&details) {
        extra.insert("metadata".into(), details);
    }
    write.put(album, extra);
    Ok(())
}
