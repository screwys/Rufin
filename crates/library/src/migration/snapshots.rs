//! One-time conversion of scalar catalog caches into canonical metadata.
use super::{integer, string};
use crate::{
    AlbumMetadata, ArtistCredit, ArtistMetadata, AudioProperties, LibraryResult, NativeGain,
    TagMetadata, TagReference, TrackMetadata,
};
use sqlx::{Row, SqliteConnection, sqlite::SqliteRow};

async fn exists(
    connection: &mut SqliteConnection,
    prefix: &str,
    table: &str,
) -> LibraryResult<bool> {
    let schema = prefix.trim_end_matches('.');
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT EXISTS(SELECT 1 FROM {schema}.sqlite_schema WHERE type='table' AND name=?1)"
    )))
    .bind(table)
    .fetch_one(connection)
    .await?)
}

async fn credits(
    connection: &mut SqliteConnection,
    prefix: &str,
    relation: &str,
    owner: &str,
    keys: &str,
) -> LibraryResult<std::collections::HashMap<i64, Vec<ArtistCredit>>> {
    if !exists(connection, prefix, relation).await? {
        return Ok(Default::default());
    }
    let rows=sqlx::query(sqlx::AssertSqlSafe(format!("SELECT credit.{owner} owner,artist.object_id,artist.name,artist.sort_text,artist.musicbrainz_artist_id FROM {prefix}{relation} credit JOIN {prefix}artists artist USING(artist_key) WHERE credit.{owner} IN(SELECT value FROM json_each(?1)) ORDER BY credit.{owner},credit.position"))).bind(keys).fetch_all(connection).await?;
    let mut result = std::collections::HashMap::<i64, Vec<ArtistCredit>>::new();
    for row in rows {
        result
            .entry(row.try_get("owner")?)
            .or_default()
            .push(ArtistCredit {
                object_id: row.try_get("object_id")?,
                name: row.try_get("name")?,
                sort_text: string(&row, "sort_text"),
                musicbrainz_artist_id: string(&row, "musicbrainz_artist_id"),
            });
    }
    Ok(result)
}

async fn tags(
    connection: &mut SqliteConnection,
    prefix: &str,
    relation: &str,
    owner: &str,
    kind: &str,
    keys: &str,
) -> LibraryResult<std::collections::HashMap<i64, Vec<TagReference>>> {
    if !exists(connection, prefix, relation).await? {
        return Ok(Default::default());
    }
    let rows=sqlx::query(sqlx::AssertSqlSafe(format!("SELECT credit.{owner} owner,item.object_id,item.name,item.sort_text FROM {prefix}{relation} credit JOIN {prefix}{kind}s item USING({kind}_key) WHERE credit.{owner} IN(SELECT value FROM json_each(?1)) ORDER BY credit.{owner},credit.position"))).bind(keys).fetch_all(connection).await?;
    let mut result = std::collections::HashMap::<i64, Vec<TagReference>>::new();
    for row in rows {
        result
            .entry(row.try_get("owner")?)
            .or_default()
            .push(TagReference {
                object_id: row.try_get("object_id")?,
                name: row.try_get("name")?,
                sort_text: string(&row, "sort_text"),
            });
    }
    Ok(result)
}

async fn gains(
    connection: &mut SqliteConnection,
    prefix: &str,
    kind: &str,
    keys: &str,
) -> LibraryResult<std::collections::HashMap<i64, (Vec<u8>, NativeGain)>> {
    let mut result = std::collections::HashMap::new();
    if exists(connection, prefix, "loudness_measurements").await? {
        let origin=sqlx::query_scalar::<_,bool>(sqlx::AssertSqlSafe(format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{}loudness_measurements','{}') WHERE name='origin')","",prefix.trim_end_matches('.')))).fetch_one(&mut *connection).await?;
        let rows=sqlx::query(sqlx::AssertSqlSafe(format!("SELECT entity_key,analysis_key,integrated_lufs,true_peak FROM {prefix}loudness_measurements WHERE entity_kind=?1 AND entity_key IN(SELECT value FROM json_each(?2)){}",if origin{" AND origin='source'"}else{""}))).bind(kind).bind(keys).fetch_all(&mut *connection).await?;
        for row in rows {
            result.insert(
                row.try_get("entity_key")?,
                (
                    row.try_get("analysis_key")?,
                    NativeGain {
                        integrated_lufs: row.try_get("integrated_lufs")?,
                        true_peak: row.try_get("true_peak")?,
                        ..Default::default()
                    },
                ),
            );
        }
    }
    if exists(connection, prefix, "replay_gain_measurements").await? {
        let rows=sqlx::query(sqlx::AssertSqlSafe(format!("SELECT entity_key,analysis_key,gain_db,peak FROM {prefix}replay_gain_measurements WHERE entity_kind=?1 AND entity_key IN(SELECT value FROM json_each(?2))"))).bind(kind).bind(keys).fetch_all(connection).await?;
        for row in rows {
            let entry = result
                .entry(row.try_get("entity_key")?)
                .or_insert_with(|| (row.get("analysis_key"), NativeGain::default()));
            if entry.0 == row.get::<Vec<u8>, _>("analysis_key") {
                entry.1.replay_gain_db = row.try_get("gain_db")?;
                entry.1.replay_gain_peak = row.try_get("peak")?;
            }
        }
    }
    Ok(result)
}

fn raw_art(row: &SqliteRow) -> LibraryResult<Option<Vec<u8>>> {
    row.try_get::<Option<Vec<u8>>, _>("artwork_binding")
        .ok()
        .flatten()
        .map(|bytes| crate::normalize_artwork_binding(&bytes))
        .transpose()
}

type MetadataInput = (i64, String, Option<Vec<u8>>, bool, [Option<i64>; 5]);

fn native_state(row: &SqliteRow) -> [Option<i64>; 5] {
    [
        "source_favorite",
        "source_rating",
        "source_play_count",
        "source_skip_count",
        "source_last_played",
    ]
    .map(|field| integer(row, field))
}

async fn write_metadata(
    connection: &mut SqliteConnection,
    kind: &str,
    rows: &[MetadataInput],
) -> LibraryResult<()> {
    let key = format!("{kind}_key");
    let mut query = sqlx::QueryBuilder::new("WITH input(entity_key,payload,artwork,native) AS(");
    query.push_values(rows, |mut value, (key, payload, artwork, native, _)| {
        value
            .push_bind(key)
            .push_bind(payload)
            .push_bind(artwork)
            .push_bind(native);
    });
    query.push(format!(") UPDATE {kind}s SET metadata=jsonb(input.payload),native_artwork=input.artwork,native_metadata=input.native FROM input WHERE {kind}s.{key}=input.entity_key"));
    query.build().execute(&mut *connection).await?;
    let states = rows
        .iter()
        .filter(|row| row.4.iter().any(Option::is_some))
        .collect::<Vec<_>>();
    if !states.is_empty() {
        let mut query = sqlx::QueryBuilder::new(
            "WITH input(entity_key,favorite,rating,play_count,skip_count,last_played) AS(",
        );
        query.push_values(states, |mut value, (key, _, _, _, state)| {
            value.push_bind(key);
            for field in state {
                value.push_bind(field);
            }
        });
        query.push(format!(") INSERT INTO native_media_state(media_uri,favorite,rating,play_count,skip_count,last_played) SELECT item.media_uri,input.favorite,input.rating,input.play_count,input.skip_count,input.last_played FROM input JOIN {kind}s item ON item.{key}=input.entity_key WHERE true ON CONFLICT(media_uri) DO UPDATE SET favorite=excluded.favorite,rating=excluded.rating,play_count=excluded.play_count,skip_count=excluded.skip_count,last_played=excluded.last_played"));
        query.build().execute(&mut *connection).await?;
    }
    Ok(())
}

pub(super) async fn populate(connection: &mut SqliteConnection, prefix: &str) -> LibraryResult<()> {
    for kind in ["artist", "genre", "mood", "folder", "track", "album"] {
        if !exists(connection, prefix, &format!("{kind}s")).await? {
            continue;
        }
        let mut after = 0_i64;
        loop {
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT old.*,current.media_uri migration_uri,current.indexed migration_indexed FROM {prefix}{kind}s old JOIN main.{kind}s current USING({kind}_key) WHERE old.{kind}_key>?1 ORDER BY old.{kind}_key LIMIT 128"
            )))
            .bind(after)
            .fetch_all(&mut *connection)
            .await?;
            if rows.is_empty() {
                break;
            }
            let keys = serde_json::to_string(
                &rows
                    .iter()
                    .map(|row| row.get::<i64, _>(format!("{kind}_key").as_str()))
                    .collect::<Vec<_>>(),
            )?;
            let mut artists = if matches!(kind, "album" | "track") {
                credits(
                    connection,
                    prefix,
                    &format!("{kind}_artists"),
                    &format!("{kind}_key"),
                    &keys,
                )
                .await?
            } else {
                Default::default()
            };
            let mut album_artists = if kind == "track" {
                credits(
                    connection,
                    prefix,
                    "track_album_artists",
                    "track_key",
                    &keys,
                )
                .await?
            } else {
                Default::default()
            };
            let mut genres = if matches!(kind, "album" | "track") {
                tags(
                    connection,
                    prefix,
                    &format!("{kind}_genres"),
                    &format!("{kind}_key"),
                    "genre",
                    &keys,
                )
                .await?
            } else {
                Default::default()
            };
            let mut moods = if kind == "track" {
                tags(
                    connection,
                    prefix,
                    "track_moods",
                    "track_key",
                    "mood",
                    &keys,
                )
                .await?
            } else {
                Default::default()
            };
            let mut native = if matches!(kind, "album" | "track") {
                gains(connection, prefix, kind, &keys).await?
            } else {
                Default::default()
            };
            let mut inputs = Vec::with_capacity(rows.len());
            let mut searches = Vec::new();
            let mut album_orders = Vec::new();
            let mut album_audio = Vec::new();
            for row in rows {
                after = row.try_get(format!("{kind}_key").as_str())?;
                let uri: String = row.try_get("migration_uri")?;
                let mut gain = native.remove(&after);
                let expected = row
                    .try_get::<Vec<u8>, _>("source_loudness_analysis_key")
                    .unwrap_or_else(|_| vec![0; 32]);
                if gain.as_ref().is_some_and(|(key, _)| *key != expected) {
                    gain = None;
                }
                let raw = raw_art(&row)?;
                let raw = if kind == "artist" {
                    Some(crate::catalog::artwork::artist_binding(
                        string(&row, "name").as_deref().unwrap_or_default(),
                        string(&row, "musicbrainz_artist_id").as_deref(),
                        raw.as_deref(),
                    )?)
                } else {
                    raw
                };
                let payload = match kind {
                    "artist" => serde_json::to_string(&ArtistMetadata {
                        name: string(&row, "name").unwrap_or_default(),
                        sort_text: string(&row, "sort_text"),
                        musicbrainz_artist_id: string(&row, "musicbrainz_artist_id"),
                    })?,
                    "track" => {
                        let metadata = TrackMetadata {
                            title: string(&row, "title").unwrap_or_else(|| uri.clone()),
                            artist: string(&row, "display_artist").unwrap_or_default(),
                            album: string(&row, "display_album").unwrap_or_default(),
                            album_display_artist: string(&row, "album_display_artist"),
                            sort_text: string(&row, "sort_text").unwrap_or_default(),
                            duration_millis: integer(&row, "duration_millis").unwrap_or_default(),
                            disc_number: integer(&row, "disc_number").unwrap_or_default(),
                            track_number: integer(&row, "track_number").unwrap_or_default(),
                            year: integer(&row, "year"),
                            release_date: string(&row, "release_date"),
                            date_added: string(&row, "date_added"),
                            source_format: string(&row, "source_format"),
                            audio_properties: string(&row, "audio_properties")
                                .and_then(|text| {
                                    serde_json::from_str::<AudioProperties>(&text).ok()
                                })
                                .unwrap_or_default(),
                            comment: string(&row, "comment"),
                            bpm: integer(&row, "bpm"),
                            musicbrainz_recording_id: string(&row, "musicbrainz_recording_id"),
                            musicbrainz_release_track_id: string(
                                &row,
                                "musicbrainz_release_track_id",
                            ),
                            musicbrainz_album_id: string(&row, "musicbrainz_album_id"),
                            musicbrainz_release_group_id: string(
                                &row,
                                "musicbrainz_release_group_id",
                            ),
                            primary_artist_musicbrainz_id: string(
                                &row,
                                "primary_artist_musicbrainz_id",
                            ),
                            artists: artists.remove(&after).unwrap_or_default(),
                            album_artists: album_artists.remove(&after).unwrap_or_default(),
                            genres: genres.remove(&after).unwrap_or_default(),
                            moods: moods.remove(&after).unwrap_or_default(),
                            album_tags: None,
                            source_loudness: gain.map(|(_, gain)| gain),
                            album_loudness: None,
                        };
                        searches.push((after, crate::catalog::cache::track_search(&metadata)));
                        serde_json::to_string(&metadata)?
                    }
                    "album" => {
                        let releases: Vec<String> = if exists(
                            connection,
                            prefix,
                            "album_release_types",
                        )
                        .await?
                        {
                            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT release_type FROM {prefix}album_release_types WHERE album_key=?1 ORDER BY position"))).bind(after).fetch_all(&mut *connection).await?
                        } else {
                            Vec::new()
                        };
                        let metadata = AlbumMetadata {
                            title: string(&row, "title").unwrap_or_default(),
                            artist: string(&row, "display_artist").unwrap_or_default(),
                            sort_text: string(&row, "sort_text").unwrap_or_default(),
                            year: integer(&row, "year"),
                            release_date: string(&row, "release_date"),
                            date_added: string(&row, "date_added"),
                            musicbrainz_release_id: string(&row, "musicbrainz_release_id"),
                            musicbrainz_release_group_id: string(
                                &row,
                                "musicbrainz_release_group_id",
                            ),
                            is_compilation: integer(&row, "is_compilation").map(|value| value != 0),
                            album_artists: artists.remove(&after).unwrap_or_default(),
                            genres: genres.remove(&after).unwrap_or_default(),
                            release_types: releases.clone(),
                        };
                        if let Some(identity) = string(&row, "release_lookup_identity") {
                            sqlx::query("INSERT OR REPLACE INTO album_release_observations VALUES(?1,?2,jsonb(?3))").bind(after).bind(identity).bind(serde_json::to_string(&releases)?).execute(&mut *connection).await?;
                        }
                        let audio = gain
                        .map(|(token, gain)| serde_json::to_vec(&serde_json::json!({"token":token.iter().map(|byte|format!("{byte:02X}")).collect::<String>(),"gain":gain})))
                        .transpose()?;
                        album_audio.push((after, audio));
                        album_orders.push((
                            after,
                            metadata.title.to_lowercase(),
                            metadata.artist.to_lowercase(),
                        ));
                        serde_json::to_string(&metadata)?
                    }
                    _ => serde_json::to_string(&TagMetadata {
                        name: string(&row, "name").unwrap_or_default(),
                        sort_text: string(&row, "sort_text"),
                    })?,
                };
                inputs.push((
                    after,
                    payload,
                    raw,
                    if kind == "track" {
                        integer(&row, "migration_indexed") == Some(1)
                    } else {
                        true
                    },
                    native_state(&row),
                ));
            }
            write_metadata(connection, kind, &inputs).await?;
            if kind == "track" {
                let mut query = sqlx::QueryBuilder::new("WITH input(track_key,normalized) AS(");
                query.push_values(&searches, |mut value, (key, search)| {
                    value.push_bind(key).push_bind(search);
                });
                query.push(") UPDATE tracks SET normalized_search=input.normalized FROM input WHERE tracks.track_key=input.track_key");
                query.build().execute(&mut *connection).await?;
            } else if kind == "album" {
                let mut query = sqlx::QueryBuilder::new("WITH input(album_key,accepted_audio) AS(");
                query.push_values(&album_audio, |mut value, (key, audio)| {
                    value.push_bind(key).push_bind(audio);
                });
                query.push(") UPDATE albums SET cached_local=coalesce((SELECT kind IN('local','file','files','documents','webdav','smb','remote_file','remote-file') FROM sources WHERE sources.source_key=albums.source_key),0),accepted_audio=input.accepted_audio FROM input WHERE albums.album_key=input.album_key");
                query.build().execute(&mut *connection).await?;
                let mut query =
                    sqlx::QueryBuilder::new("WITH input(album_key,title_search,artist_search) AS(");
                query.push_values(&album_orders, |mut value, (key, title, artist)| {
                    value.push_bind(key).push_bind(title).push_bind(artist);
                });
                query.push(") INSERT INTO album_order(album_key,source_key,title_sort,album_artist_sort,year_sort,release_sort,added_sort,title_search,artist_search) SELECT album.album_key,album.source_key,coalesce(album.sort_text,''),(SELECT artist.sort_text FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key ORDER BY credit.position LIMIT 1),album.year,album.release_date,coalesce(unixepoch(album.date_added),album.first_seen_at,(SELECT coalesce(unixepoch(member.date_added),member.first_seen_at) FROM tracks member INDEXED BY tracks_album_order_idx WHERE member.album_key=album.album_key AND member.indexed=1 ORDER BY coalesce(member.cue_path,member.source_path),member.disc_number,member.track_number,member.object_id LIMIT 1)),input.title_search,input.artist_search FROM input JOIN albums album USING(album_key)");
                query.build().execute(&mut *connection).await?;
            }
        }
    }

    sqlx::query("UPDATE tracks SET local_file_key=(SELECT file.local_file_key FROM local_files file WHERE file.source_key=tracks.source_key AND file.path=coalesce(tracks.cue_path,tracks.source_path) AND file.state='accepted') WHERE indexed=1 OR cue_path IS NULL").execute(&mut *connection).await?;
    sqlx::query("UPDATE local_file_dependencies AS edge SET accepted_inputs=json_array(dependency.size_bytes,dependency.mtime_ns,dependency.revision) FROM local_files owner JOIN local_files dependency ON dependency.source_key=owner.source_key WHERE owner.local_file_key=edge.local_file_key AND owner.kind='cue' AND owner.state='accepted' AND dependency.path=edge.dependency_path AND EXISTS(SELECT 1 FROM tracks WHERE tracks.local_file_key=owner.local_file_key)").execute(&mut *connection).await?;
    sqlx::raw_sql("PRAGMA main.optimize=0x10002")
        .execute(&mut *connection)
        .await?;
    crate::catalog::artwork::initialize_choices(connection).await?;
    Ok(())
}
