//! Builds a Snapshot from the Store's thin columns. Tracks and their links are read and packed
//! in key ranges on reader connections and threads of their own, then joined in key order;
//! every other family is small and read on one connection. No JSON column is read, except
//! smart playlist definitions. No Write runs during a load, because every Write needs the
//! Catalog, so the connections see one state.

use std::collections::HashMap;
use std::path::Path;

use sqlx::{Connection, SqliteConnection};

use super::records::*;
use super::rows::{Handle, Rows};
use super::snapshot::{Entry, Names, Snapshot, Stats, TrackInput, TrackPart, UserItem};
use super::write;
use crate::schema::{
    HAS_PICTURE, LINK_ALBUM_ARTIST, LINK_ARTIST, LINK_FOLDER, LINK_GENRE, LINK_MOOD,
};
use crate::{
    AlbumKey, ArtistKey, ArtistMetadata, FolderKey, FolderMetadata, LibraryError, LibraryResult,
    SourceKey, TagMetadata, TrackKey,
};

/// Libraries smaller than this load in one range.
const RANGE_TRACKS: i64 = 8_192;
const MAX_RANGES: usize = 4;

const TRACKS: &str = "SELECT track_key,source_key,album_key,indexed,native_metadata,flags,title,display_artist,display_album,sort_text,comment,source_format,duration_millis,disc_number,track_number,year,released,added_at,bpm,native_favorite,native_rating,native_play_count,native_last_played FROM tracks WHERE track_key>?1 AND track_key<=?2 ORDER BY track_key";
const LINKS: &str = "SELECT track_key,role,target FROM track_links WHERE track_key>?1 AND track_key<=?2 ORDER BY track_key,role,position";

fn native_state(row: &Rows<'_>, start: i32) -> NativeState {
    NativeState {
        favorite: row.optional_int(start).map(|value| value != 0),
        rating: row.optional_int(start + 1),
        play_count: row.optional_int(start + 2),
        last_played: row.optional_int(start + 3),
    }
}

/// Reads the tracks with keys in (after, last] and their links, and packs each row as it
/// steps; a row's texts stay valid until the next step of its statement.
async fn read_range(
    path: &Path,
    names: &Names<'_>,
    after: i64,
    last: i64,
) -> LibraryResult<TrackPart> {
    let mut part = TrackPart::default();
    let mut connection = SqliteConnection::connect_with(&crate::db::reader_options(path)).await?;
    {
        let handle = Handle::lock(&mut connection).await?;
        let mut tracks = handle.prepare(TRACKS)?;
        let mut links = handle.prepare(LINKS)?;
        tracks.bind(1, after)?.bind(2, last)?;
        links.bind(1, after)?.bind(2, last)?;
        let mut link = links.step()?;
        let mut runs: [Vec<i64>; 4] = Default::default();
        let mut packing = part.packing();
        while tracks.step()? {
            let key = tracks.int(0);
            runs.iter_mut().for_each(Vec::clear);
            while link && links.int(0) < key {
                link = links.step()?;
            }
            while link && links.int(0) == key {
                let run = match links.int(1) {
                    LINK_ARTIST => Some(0),
                    LINK_ALBUM_ARTIST => Some(1),
                    LINK_GENRE | LINK_MOOD => Some(2),
                    LINK_FOLDER => Some(3),
                    _ => None,
                };
                if let Some(run) = run {
                    runs[run].push(links.int(2));
                }
                link = links.step()?;
            }
            let flags = tracks.int(5);
            packing.pack(
                names,
                TrackInput {
                    key,
                    source: tracks.optional_int(1).map(SourceKey::from_raw),
                    album: tracks.optional_int(2),
                    indexed: tracks.flag(3),
                    native: tracks.flag(4),
                    picture: flags & HAS_PICTURE != 0,
                    cue: flags & crate::schema::TRACK_CUE != 0,
                    title: tracks.text(6),
                    artist: tracks.text(7),
                    album_title: tracks.text(8),
                    sort: tracks.text(9),
                    comment: tracks.optional_text(10),
                    format: tracks.optional_text(11),
                    duration_ms: tracks.int(12),
                    disc: tracks.int(13),
                    number: tracks.int(14),
                    year: tracks.optional_int(15),
                    release: tracks.int(16) as u32,
                    added: tracks.optional_int(17),
                    bpm: tracks.optional_int(18),
                    state: &native_state(&tracks, 19),
                    links: [&runs[0], &runs[1], &runs[2], &runs[3]],
                },
            );
        }
    }
    connection.close().await?;
    Ok(part)
}

/// Reads and packs every track in key ranges on threads of their own, and adds each range's
/// tracks to the Snapshot in key order as it finishes.
fn tracks(path: &Path, snapshot: &mut Snapshot, first: i64, last: i64) -> LibraryResult<()> {
    let span = last - first + 1;
    let ranges = if span < RANGE_TRACKS {
        1
    } else {
        std::thread::available_parallelism()
            .map_or(1, usize::from)
            .clamp(1, MAX_RANGES)
    } as i64;
    let width = (span + ranges - 1) / ranges;
    let (names, mut packing) = snapshot.split();
    let names = &names;
    std::thread::scope(|scope| {
        let mut readers = Vec::new();
        for range in 0..ranges {
            let after = first - 1 + range * width;
            let end = (after + width).min(last);
            readers.push(
                std::thread::Builder::new()
                    .name("rufin-catalog-range".into())
                    .spawn_scoped(scope, move || {
                        futures_executor::block_on(read_range(path, names, after, end))
                    })?,
            );
        }
        for reader in readers {
            let part = reader.join().map_err(|_| {
                LibraryError::InvalidStore("a Catalog range reader stopped".into())
            })??;
            packing.append(part);
        }
        Ok(())
    })
}

/// Link targets by owner, in role and position order.
fn links(
    handle: &Handle<'_>,
    table: &str,
    owner: &str,
) -> LibraryResult<HashMap<i64, Vec<(i64, i64)>>> {
    let mut rows = handle.prepare(&format!(
        "SELECT {owner},role,target FROM {table} ORDER BY {owner},role,position"
    ))?;
    let mut links = HashMap::<i64, Vec<(i64, i64)>>::new();
    while rows.step()? {
        links
            .entry(rows.int(0))
            .or_default()
            .push((rows.int(1), rows.int(2)));
    }
    Ok(links)
}

/// Sources, folders, artists, tags and albums. Returns the first and last track key.
fn small_families(handle: &Handle<'_>, snapshot: &mut Snapshot) -> LibraryResult<(i64, i64)> {
    let mut counts = handle.prepare(
        "SELECT (SELECT count(*) FROM tracks),(SELECT count(*) FROM albums),(SELECT count(*) FROM artists),(SELECT count(*) FROM tags),(SELECT count(*) FROM folders),(SELECT count(*) FROM track_links),coalesce((SELECT min(track_key) FROM tracks),1),coalesce((SELECT max(track_key) FROM tracks),0)",
    )?;
    counts.step()?;
    let count = |column| usize::try_from(counts.int(column)).unwrap_or_default();
    snapshot.reserve([count(0), count(1), count(2), count(3), count(4)]);
    snapshot.reserve_edges(count(5));
    let keys = (counts.int(6), counts.int(7));
    drop(counts);
    let mut rows = handle.prepare(
        "SELECT source_key,object_id,kind,display_name,catalog_complete,freshness FROM sources ORDER BY source_key",
    )?;
    while rows.step()? {
        snapshot.put_source(Source {
            key: SourceKey::from_raw(rows.int(0)),
            object: rows.text(1).to_owned(),
            kind: rows.optional_text(2).map(str::to_owned),
            name: rows.text(3).to_owned(),
            complete: rows.flag(4),
            freshness: rows.blob(5),
        });
    }
    let mut rows = handle.prepare(
        "SELECT folder_key,source_key,indexed,native_metadata,name,sort_text,flags,object_id FROM folders ORDER BY folder_key",
    )?;
    while rows.step()? {
        let mut folder = Folder::new(
            SourceKey::from_raw(rows.int(1)),
            rows.text(7),
            String::new(),
        );
        folder.key = FolderKey::from_raw(rows.int(0));
        folder.indexed = rows.flag(2);
        folder.native = rows.flag(3);
        folder.metadata = FolderMetadata {
            name: rows.optional_text(4).map(str::to_owned),
            sort_text: rows.optional_text(5).map(str::to_owned),
        };
        snapshot.pack_folder(&folder, rows.int(6) & HAS_PICTURE != 0);
    }
    let mut rows = handle.prepare(
        "SELECT artist_key,source_key,indexed,native_metadata,name,sort_text,native_favorite,native_rating,native_play_count,native_last_played,flags,object_id FROM artists ORDER BY artist_key",
    )?;
    while rows.step()? {
        let mut artist = Artist::new(
            SourceKey::from_raw(rows.int(1)),
            rows.text(11),
            String::new(),
        );
        artist.key = ArtistKey::from_raw(rows.int(0));
        artist.indexed = rows.flag(2);
        artist.native = rows.flag(3);
        artist.metadata = ArtistMetadata {
            name: rows.text(4).to_owned(),
            sort_text: rows.optional_text(5).map(str::to_owned),
            musicbrainz_artist_id: None,
        };
        artist.state = native_state(&rows, 6);
        snapshot.pack_artist(&artist, rows.int(10) & HAS_PICTURE != 0);
    }
    let mut rows = handle.prepare(
        "SELECT tag_key,source_key,kind,indexed,native_metadata,name,sort_text,flags,object_id FROM tags ORDER BY tag_key",
    )?;
    while rows.step()? {
        let mut tag = Tag::new(
            super::families::tag_kind(rows.int(2)),
            SourceKey::from_raw(rows.int(1)),
            rows.text(8),
            String::new(),
        );
        tag.key = rows.int(0);
        tag.indexed = rows.flag(3);
        tag.native = rows.flag(4);
        tag.metadata = TagMetadata {
            name: rows.text(5).to_owned(),
            sort_text: rows.optional_text(6).map(str::to_owned),
        };
        snapshot.pack_tag(&tag, rows.int(7) & HAS_PICTURE != 0);
    }
    let mut album_links = links(handle, "album_links", "album_key")?;
    let mut rows = handle.prepare(
        "SELECT album_key,source_key,indexed,native_metadata,cached_local,title,display_artist,sort_text,year,released,added_at,is_compilation,native_favorite,native_rating,native_play_count,native_last_played,input_metadata IS NOT NULL,flags,object_id FROM albums ORDER BY album_key",
    )?;
    while rows.step()? {
        let key = rows.int(0);
        let mut album = Album::new(
            SourceKey::from_raw(rows.int(1)),
            rows.text(18),
            String::new(),
        );
        album.key = AlbumKey::from_raw(key);
        album.indexed = rows.flag(2);
        album.native = rows.flag(3);
        album.cached_local = rows.flag(4);
        album.facts = AlbumFacts {
            title: rows.text(5).to_owned(),
            artist: rows.text(6).to_owned(),
            sort: rows.text(7).to_owned(),
            year: rows.optional_int(8),
            compilation: rows.optional_int(11).map(|value| value != 0),
            ..AlbumFacts::default()
        };
        album.state = native_state(&rows, 12);
        album.input = rows.flag(16);
        album.class = crate::AlbumReleaseClass::stored(rows.int(17));
        for (role, target) in album_links.remove(&key).unwrap_or_default() {
            match role {
                LINK_ARTIST => album.artists.push(ArtistKey::from_raw(target)),
                LINK_GENRE => album.genres.push(target),
                _ => {}
            }
        }
        snapshot.pack_album(
            &album,
            rows.int(17) & HAS_PICTURE != 0,
            rows.int(9) as u32,
            rows.optional_int(10),
        );
    }
    Ok(keys)
}

/// Favorites, ratings, plays, downloads and playlists, once every track is resident.
fn user_data(handle: &Handle<'_>, snapshot: &mut Snapshot) -> LibraryResult<()> {
    let mut rows = handle.prepare(
        "SELECT 0,track.track_key,state.favorite,state.rating FROM user_state state JOIN tracks track USING(media_uri)
         UNION ALL SELECT 1,album.album_key,state.favorite,state.rating FROM user_state state JOIN albums album USING(media_uri)
         UNION ALL SELECT 2,artist.artist_key,state.favorite,state.rating FROM user_state state JOIN artists artist USING(media_uri)",
    )?;
    while rows.step()? {
        let key = rows.int(1);
        let item = match rows.int(0) {
            0 => UserItem::Track(key),
            1 => UserItem::Album(key),
            _ => UserItem::Artist(key),
        };
        let favorite = rows.optional_int(2).map(|value| value != 0);
        snapshot.set_user_state(item, write::user_state(favorite, rows.optional_int(3)));
    }
    let mut listened = HashMap::<i64, (i64, i64, Option<i64>)>::new();
    let mut rows = handle.prepare(
        "SELECT track.track_key,counted.plays,counted.skips,counted.last FROM(SELECT media_uri,sum(skipped=0) plays,sum(skipped) skips,max(started_at) last FROM listens GROUP BY media_uri) counted JOIN tracks track USING(media_uri)",
    )?;
    while rows.step()? {
        listened.insert(
            rows.int(0),
            (rows.int(1), rows.int(2), rows.optional_int(3)),
        );
    }
    let mut rows = handle.prepare(
        "SELECT track.track_key,legacy.play_count,legacy.skip_count,legacy.last_played_at FROM legacy_activity legacy JOIN sources source ON source.object_id=legacy.source_id JOIN tracks track ON track.source_key=source.source_key AND track.object_id=legacy.track_object_id WHERE legacy.period='lifetime' AND legacy.item_kind='track'",
    )?;
    while rows.step()? {
        let counted = listened.entry(rows.int(0)).or_insert((0, 0, None));
        counted.0 += rows.int(1);
        counted.1 += rows.int(2);
        counted.2 = counted.2.max(rows.optional_int(3));
    }
    for (key, (plays, skips, last)) in listened {
        snapshot.set_listened(TrackKey::from_raw(key), Stats::listened(plays, skips, last));
    }
    let mut rows = handle.prepare(
        "SELECT DISTINCT track.track_key FROM downloads download JOIN tracks track USING(media_uri)",
    )?;
    while rows.step()? {
        snapshot.set_downloaded(TrackKey::from_raw(rows.int(0)), true);
    }
    let mut entries = HashMap::<i64, Vec<Entry<TrackKey>>>::new();
    let mut rows = handle.prepare(
        "SELECT playlist_key,playlist_entry_key,rank,track_key FROM playlist_entries ORDER BY playlist_key,rank,playlist_entry_key",
    )?;
    while rows.step()? {
        entries.entry(rows.int(0)).or_default().push(Entry {
            key: rows.int(1),
            rank: rows.blob(2).unwrap_or_default().into(),
            track: TrackKey::from_raw(rows.int(3)),
        });
    }
    let mut rows = handle.prepare(&format!(
        "SELECT {} FROM playlists ORDER BY playlist_key",
        write::PLAYLIST_COLUMNS
    ))?;
    while rows.step()? {
        let key = rows.int(0);
        let row = (
            key,
            rows.optional_int(1),
            rows.flag(2),
            rows.flag(3),
            rows.text(4).to_owned(),
            rows.optional_int(5),
            rows.int(6),
            rows.flag(7),
            rows.flag(8),
            rows.flag(9),
            rows.text(10).to_owned(),
        );
        snapshot.put_playlist(&write::playlist(
            row,
            entries.remove(&key).unwrap_or_default(),
        ));
    }
    let mut rows = handle.prepare(&format!(
        "SELECT {} FROM smart_playlists",
        write::SMART_COLUMNS
    ))?;
    while rows.step()? {
        let smart = write::smart_playlist((
            rows.int(0),
            rows.text(1).to_owned(),
            rows.optional_int(2),
            rows.int(3),
            rows.text(4).to_owned(),
            rows.text(5).to_owned(),
        ));
        snapshot.smart_playlists.insert(smart.key, smart);
    }
    let mut rows =
        handle.prepare("SELECT folder_key,object_id,name,smart FROM playlist_folders")?;
    while rows.step()? {
        let key = rows.int(0);
        snapshot.playlist_folders.insert(
            key,
            super::snapshot::PlaylistFolder {
                key,
                object: rows.text(1).to_owned(),
                name: rows.text(2).to_owned(),
                smart: rows.flag(3),
            },
        );
    }
    Ok(())
}

/// Loads a Snapshot. It blocks the calling thread and runs the range readers beside it.
pub(crate) fn snapshot(path: &Path) -> LibraryResult<Snapshot> {
    let mut snapshot = Snapshot::default();
    let mut connection = futures_executor::block_on(SqliteConnection::connect_with(
        &crate::db::reader_options(path),
    ))?;
    let loaded = futures_executor::block_on(async {
        let handle = Handle::lock(&mut connection).await?;
        let (first, last) = small_families(&handle, &mut snapshot)?;
        if first <= last {
            tracks(path, &mut snapshot, first, last)?;
        }
        user_data(&handle, &mut snapshot)
    });
    futures_executor::block_on(connection.close())?;
    loaded?;
    snapshot.shrink();
    Ok(snapshot)
}
