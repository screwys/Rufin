use super::*;
use crate::{ArtistMetadata, NativeUserState, TagMetadata, TrackObservation};
use sqlx::{QueryBuilder, Sqlite};

pub(super) fn table(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "track" => ("tracks", "track_key"),
        "album" => ("albums", "album_key"),
        "artist" => ("artists", "artist_key"),
        "genre" => ("genres", "genre_key"),
        "mood" => ("moods", "mood_key"),
        "folder" => ("folders", "folder_key"),
        _ => unreachable!("catalog kind"),
    }
}

pub(super) async fn source_key(
    connection: &mut SqliteConnection,
    source: &str,
) -> LibraryResult<i64> {
    sqlx::query_scalar("SELECT source_key FROM sources WHERE object_id=?1")
        .bind(source)
        .fetch_optional(connection)
        .await?
        .ok_or_else(|| LibraryError::InvalidRequest("source is no longer configured".into()))
}

pub(super) async fn write_headers<T: serde::Serialize + Default + Sync>(
    connection: &mut SqliteConnection,
    source: i64,
    source_id: &str,
    kind: &str,
    rows: &[(&str, &T, String, Option<Vec<u8>>)],
    admit: bool,
    native: bool,
    observed: Option<i64>,
) -> LibraryResult<(Vec<String>, bool)> {
    let (table, _) = table(kind);
    let normalized = if kind == "album" {
        "normalized_title"
    } else {
        "normalized_name"
    };
    let mut changed = (Vec::new(), false);
    for page in rows.chunks(128) {
        let patches = if native {
            None
        } else {
            Some(
                page.iter()
                    .map(|(_, metadata, _, _)| reference_patch(*metadata))
                    .collect::<LibraryResult<Vec<_>>>()?,
            )
        };
        let mut query =
            QueryBuilder::<Sqlite>::new("WITH input(object_id,media_uri,metadata,normalized) AS(");
        query.push_values(
            page.iter().enumerate(),
            |mut row, (index, (id, metadata, normalized, _))| {
                row.push_bind(*id).push_bind(crate::source_entity_uri(
                    &SourceId::new(source_id),
                    kind,
                    id,
                ));
                if let Some(patches) = &patches {
                    row.push_bind(sqlx::types::Json(&patches[index]));
                } else {
                    row.push_bind(sqlx::types::Json(*metadata));
                }
                row.push_bind(normalized);
            },
        );
        let metadata = if !native {
            format!(
                "jsonb_patch(coalesce({table}.metadata,jsonb('{{}}')),(SELECT jsonb(metadata) FROM input WHERE object_id=excluded.object_id LIMIT 1))"
            )
        } else if kind == "folder" {
            format!(
                "CASE WHEN json_extract(excluded.metadata,'$.name') IS NULL THEN {table}.metadata ELSE excluded.metadata END"
            )
        } else {
            "excluded.metadata".to_owned()
        };
        let caption = if !native {
            format!(
                "CASE WHEN excluded.{normalized}='' OR('{kind}'='folder' AND json_extract(excluded.metadata,'$.name') IS NULL) THEN {table}.{normalized} ELSE excluded.{normalized} END"
            )
        } else if kind == "folder" {
            format!(
                "CASE WHEN json_extract(excluded.metadata,'$.name') IS NULL THEN {table}.{normalized} ELSE excluded.{normalized} END"
            )
        } else {
            format!("excluded.{normalized}")
        };
        query.push(format!(") INSERT INTO {table}(source_key,object_id,media_uri,metadata,{normalized},indexed,native_metadata) SELECT "))
            .push_bind(source).push(",object_id,media_uri,");
        if native {
            query.push("jsonb(metadata)");
        } else {
            query
                .push("jsonb_patch(")
                .push_bind(sqlx::types::Json(T::default()))
                .push(",metadata)");
        }
        query.push(",normalized,").push_bind(admit)
            .push(",").push_bind(native)
            .push(format!(" FROM input WHERE true ON CONFLICT(source_key,object_id) DO UPDATE SET metadata={metadata},{normalized}={caption},indexed=max({table}.indexed,excluded.indexed),native_metadata=max({table}.native_metadata,excluded.native_metadata) WHERE "))
            .push(if native { "1" } else { "native_metadata=0" })
            .push(format!(" AND {table}.observed_stamp<=")).push_bind(observed.unwrap_or(i64::MAX))
            .push(format!(" AND ({table}.metadata,{table}.indexed,{table}.native_metadata) IS NOT({metadata},max({table}.indexed,excluded.indexed),max({table}.native_metadata,excluded.native_metadata)) RETURNING object_id"));
        changed.0.extend(
            query
                .build_query_scalar::<String>()
                .fetch_all(&mut *connection)
                .await?,
        );
        changed.1 |= super::artwork::write_images(
            connection,
            source,
            kind,
            &page
                .iter()
                .map(|(id, _, _, artwork)| (*id, artwork.as_deref()))
                .collect::<Vec<_>>(),
            observed,
            native,
        )
        .await?;
    }
    Ok(changed)
}

pub(super) async fn users(
    connection: &mut SqliteConnection,
    source: i64,
    kind: &str,
    rows: &[(&str, &NativeUserState)],
    observed: Option<i64>,
) -> LibraryResult<CatalogUpdate> {
    let (table, _) = table(kind);
    let mut changed = CatalogUpdate::default();
    for page in rows.chunks(128) {
        let mut states = std::collections::BTreeMap::<_, NativeUserState>::new();
        for (id, state) in page
            .iter()
            .filter(|(_, state)| **state != NativeUserState::default())
        {
            states.entry(*id).or_default().merge((*state).clone());
        }

        if states.is_empty() {
            continue;
        }
        let previous: Vec<(String, Option<bool>, Option<i64>, Option<i64>, Option<i64>, Option<i64>)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT item.object_id,state.favorite,state.rating,state.play_count,state.skip_count,state.last_played FROM {table} item LEFT JOIN native_media_state state USING(media_uri) WHERE item.source_key=?1 AND item.observed_stamp<=?2 AND item.object_id IN(SELECT value FROM json_each(?3))"
            )))
            .bind(source)
            .bind(observed.unwrap_or(i64::MAX))
            .bind(serde_json::to_string(&states.keys().collect::<Vec<_>>())?)
            .fetch_all(&mut *connection)
            .await?;
        for (id, favorite, rating, plays, skips, last_played) in previous {
            if let Some(state) = states.get(id.as_str()) {
                changed.metadata |= state.favorite.is_some_and(|value| Some(value) != favorite)
                    || state.rating.is_some_and(|value| value != rating);
                changed.activity |= state.play_count.is_some_and(|value| Some(value) != plays)
                    || state.skip_count.is_some_and(|value| Some(value) != skips)
                    || state
                        .last_played
                        .is_some_and(|value| Some(value) != last_played);
            }
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "WITH input(object_id,favorite,rating,rating_known,play_count,skip_count,last_played) AS(",
        );
        query.push_values(states, |mut row, (id, state)| {
            row.push_bind(id)
                .push_bind(state.favorite)
                .push_bind(state.rating.flatten())
                .push_bind(state.rating.is_some())
                .push_bind(state.play_count)
                .push_bind(state.skip_count)
                .push_bind(state.last_played);
        });
        query.push(format!(") INSERT INTO native_media_state(media_uri,favorite,rating,play_count,skip_count,last_played) SELECT item.media_uri,input.favorite,input.rating,input.play_count,input.skip_count,input.last_played FROM input JOIN {table} item USING(object_id) WHERE item.source_key="))
            .push_bind(source).push(" AND item.observed_stamp<=").push_bind(observed.unwrap_or(i64::MAX))
            .push(" ON CONFLICT(media_uri) DO UPDATE SET favorite=coalesce(excluded.favorite,native_media_state.favorite),rating=CASE WHEN(SELECT rating_known FROM input JOIN ")
            .push(table).push(" item USING(object_id) WHERE item.media_uri=excluded.media_uri LIMIT 1) THEN excluded.rating ELSE native_media_state.rating END,play_count=coalesce(excluded.play_count,native_media_state.play_count),skip_count=coalesce(excluded.skip_count,native_media_state.skip_count),last_played=coalesce(excluded.last_played,native_media_state.last_played) WHERE (favorite,rating,play_count,skip_count,last_played) IS NOT(coalesce(excluded.favorite,favorite),CASE WHEN(SELECT rating_known FROM input JOIN ")
            .push(table).push(" item USING(object_id) WHERE item.media_uri=excluded.media_uri LIMIT 1) THEN excluded.rating ELSE rating END,coalesce(excluded.play_count,play_count),coalesce(excluded.skip_count,skip_count),coalesce(excluded.last_played,last_played))");
        query.build().execute(&mut *connection).await?;
    }
    Ok(changed)
}

pub(super) async fn stamp(
    connection: &mut SqliteConnection,
    source: i64,
    kind: &str,
    ids: &[&str],
    stamp: Option<i64>,
) -> LibraryResult<()> {
    if let Some(stamp) = stamp {
        let (table, _) = table(kind);
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET observed_stamp=?3 WHERE source_key=?1 AND object_id IN(SELECT value FROM json_each(?2)) AND observed_stamp<?3")))
            .bind(source).bind(serde_json::to_string(ids)?).bind(stamp).execute(connection).await?;
    }
    Ok(())
}

pub(super) fn index_credits<'a>(
    connection: &'a mut SqliteConnection,
    source: i64,
    source_id: &'a str,
    credits: impl Iterator<Item = &'a crate::ArtistCredit>,
) -> impl std::future::Future<Output = LibraryResult<()>> + Send + 'a {
    let mut names = std::collections::BTreeMap::<&str, ArtistMetadata>::new();
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
    async move {
        for page in names.into_iter().collect::<Vec<_>>().chunks(128) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "WITH input(object_id,media_uri,metadata,normalized) AS(",
            );
            query.push_values(page, |mut row, (id, metadata)| {
                row.push_bind(*id)
                    .push_bind(crate::source_entity_uri(
                        &SourceId::new(source_id),
                        "artist",
                        id,
                    ))
                    .push_bind(sqlx::types::Json(metadata))
                    .push_bind(metadata.name.to_lowercase());
            });
            let merged = "jsonb_set(artists.metadata,'$.name',CASE WHEN json_extract(excluded.metadata,'$.name')='' THEN json_extract(artists.metadata,'$.name') ELSE json_extract(excluded.metadata,'$.name') END,'$.sort_text',coalesce(json_extract(excluded.metadata,'$.sort_text'),json_extract(artists.metadata,'$.sort_text')),'$.musicbrainz_artist_id',coalesce(json_extract(excluded.metadata,'$.musicbrainz_artist_id'),json_extract(artists.metadata,'$.musicbrainz_artist_id')))";
            query.push(" ) INSERT INTO artists(source_key,object_id,media_uri,metadata,normalized_name) SELECT ")
            .push_bind(source).push(",object_id,media_uri,jsonb(metadata),normalized FROM input WHERE true ON CONFLICT(source_key,object_id) DO UPDATE SET metadata=")
            .push(merged).push(",normalized_name=CASE WHEN excluded.normalized_name='' THEN artists.normalized_name ELSE excluded.normalized_name END WHERE native_metadata=0 AND artists.metadata IS NOT ")
            .push(merged).push(" RETURNING artist_key,json(metadata),native_artwork");
            let changed: Vec<(i64, sqlx::types::Json<ArtistMetadata>, Option<Vec<u8>>)> =
                query.build_query_as().fetch_all(&mut *connection).await?;
            if !changed.is_empty() {
                let bindings = changed
                    .into_iter()
                    .map(|(key, metadata, artwork)| {
                        Ok((
                            key,
                            super::artwork::artist_binding(
                                &metadata.name,
                                metadata.musicbrainz_artist_id.as_deref(),
                                artwork.as_deref(),
                            )?,
                        ))
                    })
                    .collect::<LibraryResult<Vec<_>>>()?;
                let mut update = QueryBuilder::<Sqlite>::new("WITH input(artist_key,binding) AS(");
                update.push_values(&bindings, |mut row, (key, binding)| {
                    row.push_bind(key).push_bind(binding);
                });
                update.push(") UPDATE artists SET native_artwork=input.binding FROM input WHERE artists.artist_key=input.artist_key AND native_artwork IS NOT input.binding");
                update.build().execute(&mut *connection).await?;
            }
        }
        Ok(())
    }
}

pub(super) fn index_tags<'a>(
    connection: &'a mut SqliteConnection,
    source: i64,
    source_id: &'a str,
    kind: &'a str,
    tags: impl Iterator<Item = &'a crate::TagReference>,
) -> impl std::future::Future<Output = LibraryResult<()>> + Send + 'a {
    let (table, _) = table(kind);
    let tags = tags.collect::<Vec<_>>();
    async move {
        for page in tags.chunks(128) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "WITH input(object_id,media_uri,metadata,normalized) AS(",
            );
            query.push_values(page, |mut row, tag| {
                row.push_bind(&tag.object_id)
                    .push_bind(crate::source_entity_uri(
                        &SourceId::new(source_id),
                        kind,
                        &tag.object_id,
                    ))
                    .push_bind(sqlx::types::Json(TagMetadata {
                        name: tag.name.clone(),
                        sort_text: tag.sort_text.clone(),
                    }))
                    .push_bind(tag.name.to_lowercase());
            });
            query.push(format!(") INSERT INTO {table}(source_key,object_id,media_uri,metadata,normalized_name) SELECT "))
            .push_bind(source).push(",object_id,media_uri,jsonb(metadata),normalized FROM input WHERE true ON CONFLICT(source_key,object_id) DO UPDATE SET metadata=excluded.metadata,normalized_name=excluded.normalized_name WHERE native_metadata=0 AND ")
            .push(format!("{table}.metadata IS NOT excluded.metadata"));
            query.build().execute(&mut *connection).await?;
        }
        Ok(())
    }
}

async fn track_indexes(
    connection: &mut SqliteConnection,
    source: i64,
    source_id: &str,
    rows: &[&TrackObservation],
) -> LibraryResult<()> {
    index_credits(
        connection,
        source,
        source_id,
        rows.iter().flat_map(|row| {
            row.metadata
                .artists
                .iter()
                .chain(&row.metadata.album_artists)
        }),
    )
    .await?;
    index_tags(
        connection,
        source,
        source_id,
        "genre",
        rows.iter().flat_map(|row| &row.metadata.genres),
    )
    .await?;
    index_tags(
        connection,
        source,
        source_id,
        "mood",
        rows.iter().flat_map(|row| &row.metadata.moods),
    )
    .await?;
    let input =
        serde_json::to_string(&rows.iter().map(|row| [&row.object_id]).collect::<Vec<_>>())?;
    for (relation, kind, array) in [
        ("track_artists", "artist", "artists"),
        ("track_album_artists", "artist", "album_artists"),
        ("track_genres", "genre", "genres"),
        ("track_moods", "mood", "moods"),
    ] {
        let (entity, key) = table(kind);
        let expected = format!(
            "SELECT track.track_key,item.{key},CAST(edge.key AS INTEGER) position FROM json_each(?2) input CROSS JOIN tracks track ON track.source_key=?1 AND track.object_id=json_extract(input.value,'$[0]') CROSS JOIN json_each(track.metadata,'$.{array}') edge CROSS JOIN {entity} item ON item.source_key=?1 AND item.object_id=json_extract(edge.value,'$.object_id')"
        );
        replace_edges(
            connection,
            source,
            relation,
            "track_key",
            key,
            &expected,
            &input,
            None,
        )
        .await?;
    }
    Ok(())
}

async fn track_folders(
    connection: &mut SqliteConnection,
    source: i64,
    rows: &[TrackObservation],
    observed: Option<i64>,
) -> LibraryResult<bool> {
    let folders = serde_json::to_string(
        &rows
            .iter()
            .filter_map(|row| {
                row.folders
                    .as_ref()
                    .map(|folders| (&row.object_id, folders))
            })
            .collect::<Vec<_>>(),
    )?;
    let expected = "SELECT track.track_key,folder.folder_key,CAST(edge.key AS INTEGER) position FROM json_each(?2) input CROSS JOIN tracks track ON track.source_key=?1 AND track.object_id=json_extract(input.value,'$[0]') CROSS JOIN json_each(input.value,'$[1]') edge CROSS JOIN folders folder ON folder.source_key=?1 AND folder.object_id=edge.value";
    replace_edges(
        connection,
        source,
        "track_folders",
        "track_key",
        "folder_key",
        expected,
        &folders,
        observed,
    )
    .await
}

pub(super) async fn replace_edges(
    connection: &mut SqliteConnection,
    source: i64,
    relation: &str,
    owner: &str,
    target: &str,
    expected: &str,
    input: &str,
    observed: Option<i64>,
) -> LibraryResult<bool> {
    let owners = if owner == "track_key" {
        "tracks"
    } else {
        "albums"
    };
    let removed = sqlx::query(sqlx::AssertSqlSafe(format!("WITH expected AS({expected}) DELETE FROM {relation} WHERE {owner} IN(SELECT {owner} FROM {owners} WHERE source_key=?1 AND observed_stamp<=?3 AND object_id IN(SELECT json_extract(value,'$[0]') FROM json_each(?2))) AND NOT EXISTS(SELECT 1 FROM expected WHERE expected.{owner}={relation}.{owner} AND expected.{target}={relation}.{target} AND expected.position={relation}.position)")))
        .bind(source).bind(input).bind(observed.unwrap_or(i64::MAX)).execute(&mut *connection).await?.rows_affected();
    let inserted=sqlx::query(sqlx::AssertSqlSafe(format!("WITH expected AS({expected}) INSERT OR IGNORE INTO {relation}({owner},{target},position) SELECT expected.{owner},expected.{target},expected.position FROM expected JOIN {owners} owner ON owner.{owner}=expected.{owner} WHERE owner.observed_stamp<=?3")))
        .bind(source).bind(input).bind(observed.unwrap_or(i64::MAX)).execute(connection).await?.rows_affected();
    Ok(removed > 0 || inserted > 0)
}

pub(crate) fn track_search(metadata: &crate::TrackMetadata) -> String {
    format!(
        "{} {} {} {} {}",
        metadata.title,
        metadata.artist,
        metadata.album,
        metadata.comment.as_deref().unwrap_or_default(),
        metadata
            .year
            .map(|year| year.to_string())
            .unwrap_or_default()
    )
    .to_lowercase()
}

pub(super) async fn tracks(
    connection: &mut SqliteConnection,
    source: i64,
    source_id: &str,
    rows: &[TrackObservation],
    observed: Option<i64>,
    admit: bool,
    authoritative: bool,
) -> LibraryResult<(CatalogUpdate, Vec<i64>)> {
    let mut changed = CatalogUpdate::default();
    let mut previous_albums = Vec::new();
    for page in rows.chunks(128) {
        if page.is_empty() {
            continue;
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "WITH input(object_id,media_uri,album_id,metadata,source_path,cue_path,cue_start,cue_end,first_seen,audio_key,normalized_search) AS(",
        );
        let uris = page
            .iter()
            .map(|row| {
                let direct = row.media_uri.as_deref().and_then(|uri| {
                    if crate::cue_media_parts(uri).is_some()
                        || crate::source_entity_parts(uri).is_some()
                    {
                        Some(uri.to_owned())
                    } else {
                        crate::normalize_direct_media_uri(uri)
                    }
                });
                match direct {
                    Some(uri) if crate::cue_media_parts(&uri).is_none() => {
                        match (row.cue_start_millis, row.cue_end_millis) {
                            (Some(start), Some(end)) => {
                                crate::cue_media_uri(&row.object_id, &uri, start, end)
                            }
                            _ => uri,
                        }
                    }
                    Some(uri) => uri,
                    None => {
                        crate::source_entity_uri(&SourceId::new(source_id), "track", &row.object_id)
                    }
                }
            })
            .collect::<Vec<_>>();
        let with_albums = page
            .iter()
            .filter_map(|row| row.album_object_id.as_deref())
            .collect::<std::collections::HashSet<_>>();
        if !with_albums.is_empty() {
            let mut albums =
                QueryBuilder::<Sqlite>::new("INSERT INTO albums(source_key,object_id,media_uri) ");
            albums.push_values(with_albums, |mut row, id| {
                row.push_bind(source)
                    .push_bind(id)
                    .push_bind(crate::source_entity_uri(
                        &SourceId::new(source_id),
                        "album",
                        id,
                    ));
            });
            albums.push(" ON CONFLICT(source_key,object_id) DO NOTHING");
            albums.build().execute(&mut *connection).await?;
        }
        let identities = serde_json::to_string(
            &page
                .iter()
                .zip(&uris)
                .map(|(row, uri)| {
                    (
                        &row.object_id,
                        uri,
                        &row.album_object_id,
                        row.loudness_analysis_key
                            .iter()
                            .map(|byte| format!("{byte:02X}"))
                            .collect::<String>(),
                    )
                })
                .collect::<Vec<_>>(),
        )?;
        let audio_changed = "WITH input AS(SELECT json_extract(value,'$[0]') id,json_extract(value,'$[1]') uri,json_extract(value,'$[2]') album_id,unhex(json_extract(value,'$[3]')) audio FROM json_each(?2)),changed AS(SELECT old.album_key old_album,album.album_key new_album FROM input LEFT JOIN tracks old ON old.media_uri=input.uri LEFT JOIN albums album ON album.source_key=?1 AND album.object_id=input.album_id WHERE NOT EXISTS(SELECT 1 FROM tracks known WHERE known.source_key=?1 AND known.object_id=input.id AND(known.observed_stamp>?4 OR(NOT ?5 AND known.native_metadata=1))) AND(old.track_key IS NULL OR(old.observed_stamp<=?4 AND(?5 OR old.native_metadata=0) AND(old.album_key IS NOT album.album_key OR old.source_loudness_analysis_key IS NOT input.audio OR(?3 AND old.indexed=0))))) SELECT old_album FROM changed WHERE old_album IS NOT NULL UNION SELECT new_album FROM changed WHERE new_album IS NOT NULL";
        let audio_albums: Vec<i64> = sqlx::query_scalar(audio_changed)
            .bind(source)
            .bind(&identities)
            .bind(admit)
            .bind(observed.unwrap_or(i64::MAX))
            .bind(authoritative)
            .fetch_all(&mut *connection)
            .await?;
        if admit {
            sqlx::query("UPDATE tracks SET source_key=NULL,object_id=NULL,indexed=0 WHERE track_key IN(SELECT known.track_key FROM json_each(?2) input CROSS JOIN tracks known ON known.source_key=?1 AND known.object_id=json_extract(input.value,'$[0]') WHERE known.media_uri<>json_extract(input.value,'$[1]') AND known.observed_stamp<=?3 AND(?4 OR known.native_metadata=0))").bind(source).bind(&identities).bind(observed.unwrap_or(i64::MAX)).bind(authoritative).execute(&mut *connection).await?;
        }
        let patches = if authoritative {
            None
        } else {
            Some(
                page.iter()
                    .map(|row| reference_patch(&row.metadata))
                    .collect::<LibraryResult<Vec<_>>>()?,
            )
        };
        query.push_values(
            page.iter().zip(&uris).enumerate(),
            |mut value, (index, (row, uri))| {
                value
                    .push_bind(&row.object_id)
                    .push_bind(uri)
                    .push_bind(&row.album_object_id);
                if let Some(patches) = &patches {
                    value.push_bind(sqlx::types::Json(&patches[index]));
                } else {
                    value.push_bind(sqlx::types::Json(&row.metadata));
                }
                value
                    .push_bind(&row.source_path)
                    .push_bind(&row.cue_path)
                    .push_bind(row.cue_start_millis)
                    .push_bind(row.cue_end_millis)
                    .push_bind(row.first_seen_at)
                    .push_bind(row.loudness_analysis_key.as_slice())
                    .push_bind(track_search(&row.metadata));
            },
        );
        query.push(") INSERT INTO tracks(source_key,object_id,media_uri,album_key,metadata,source_path,cue_path,cue_start_millis,cue_end_millis,first_seen_at,source_loudness_analysis_key,loudness_analysis_key,normalized_search,indexed,native_metadata) SELECT ")
            .push_bind(source).push(",input.object_id,input.media_uri,album.album_key,");
        if authoritative {
            query.push("jsonb(input.metadata)");
        } else {
            query
                .push("jsonb_patch(")
                .push_bind(sqlx::types::Json(crate::TrackMetadata::default()))
                .push(",input.metadata)");
        }
        query.push(",input.source_path,input.cue_path,input.cue_start,input.cue_end,input.first_seen,input.audio_key,coalesce((SELECT loudness_analysis_key FROM local_access_files WHERE media_uri=input.media_uri ORDER BY CASE origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,local_access_file_key LIMIT 1),input.audio_key),input.normalized_search,")
            .push_bind(admit).push(",").push_bind(authoritative).push(" FROM input LEFT JOIN albums album ON album.source_key=").push_bind(source)
            .push(" AND album.object_id=input.album_id WHERE NOT EXISTS(SELECT 1 FROM tracks known WHERE known.source_key=").push_bind(source).push(" AND known.object_id=input.object_id AND(known.observed_stamp>").push_bind(observed.unwrap_or(i64::MAX)).push(if authoritative{")"}else{" OR known.native_metadata=1)"}).push(") ON CONFLICT(media_uri) DO UPDATE SET source_key=excluded.source_key,object_id=excluded.object_id,album_key=excluded.album_key,metadata=")
            .push(if authoritative {"excluded.metadata"} else {"jsonb_patch(tracks.metadata,(SELECT jsonb(metadata) FROM input WHERE object_id=excluded.object_id LIMIT 1))"}).push(",normalized_search=excluded.normalized_search,native_metadata=max(tracks.native_metadata,excluded.native_metadata),source_path=excluded.source_path,cue_path=excluded.cue_path,cue_start_millis=excluded.cue_start_millis,cue_end_millis=excluded.cue_end_millis,first_seen_at=coalesce(tracks.first_seen_at,excluded.first_seen_at),source_loudness_analysis_key=excluded.source_loudness_analysis_key,loudness_analysis_key=excluded.loudness_analysis_key,indexed=max(tracks.indexed,excluded.indexed) WHERE ")
            .push(if authoritative{"1"}else{"tracks.native_metadata=0"}).push(" AND tracks.observed_stamp<=").push_bind(observed.unwrap_or(i64::MAX)).push(" AND (tracks.source_key,tracks.object_id,tracks.album_key,tracks.metadata,tracks.source_path,tracks.cue_path,tracks.cue_start_millis,tracks.cue_end_millis,tracks.source_loudness_analysis_key,tracks.loudness_analysis_key,tracks.indexed,tracks.native_metadata) IS NOT(excluded.source_key,excluded.object_id,excluded.album_key,").push(if authoritative {"excluded.metadata"} else {"jsonb_patch(tracks.metadata,(SELECT jsonb(metadata) FROM input WHERE object_id=excluded.object_id LIMIT 1))"}).push(",excluded.source_path,excluded.cue_path,excluded.cue_start_millis,excluded.cue_end_millis,excluded.source_loudness_analysis_key,excluded.loudness_analysis_key,max(tracks.indexed,excluded.indexed),max(tracks.native_metadata,excluded.native_metadata)) RETURNING object_id");
        let accepted = query
            .build_query_scalar::<String>()
            .fetch_all(&mut *connection)
            .await?
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        changed.metadata |= !accepted.is_empty();
        if !authoritative && !accepted.is_empty() {
            let rows: Vec<(i64,sqlx::types::Json<crate::TrackMetadata>)> = sqlx::query_as("SELECT track_key,json(metadata) FROM tracks WHERE source_key=?1 AND object_id IN(SELECT value FROM json_each(?2))").bind(source).bind(serde_json::to_string(&accepted)?).fetch_all(&mut *connection).await?;
            let mut normalized =
                QueryBuilder::<Sqlite>::new("WITH input(track_key,normalized) AS(");
            normalized.push_values(&rows, |mut value, (key, metadata)| {
                value.push_bind(*key).push_bind(track_search(metadata));
            });
            normalized.push(") UPDATE tracks SET normalized_search=input.normalized FROM input WHERE tracks.track_key=input.track_key AND tracks.normalized_search IS NOT input.normalized");
            normalized.build().execute(&mut *connection).await?;
        }
        changed.artwork |= super::artwork::write_images(
            connection,
            source,
            "track",
            &page
                .iter()
                .map(|row| (row.object_id.as_str(), row.artwork_binding.as_deref()))
                .collect::<Vec<_>>(),
            observed,
            authoritative,
        )
        .await?;
        previous_albums.extend(&audio_albums);
        crate::loudness::invalidate_album_audio(
            connection,
            &audio_albums
                .iter()
                .copied()
                .map(crate::AlbumKey::from_raw)
                .collect::<Vec<_>>(),
            true,
        )
        .await?;
        stamp(
            connection,
            source,
            "track",
            &page
                .iter()
                .map(|row| row.object_id.as_str())
                .collect::<Vec<_>>(),
            observed,
        )
        .await?;
        track_indexes(
            connection,
            source,
            source_id,
            &page
                .iter()
                .filter(|row| accepted.contains(&row.object_id))
                .collect::<Vec<_>>(),
        )
        .await?;
        changed.metadata |= track_folders(connection, source, page, observed).await?;
        changed.merge(
            users(
                connection,
                source,
                "track",
                &page
                    .iter()
                    .map(|row| (row.object_id.as_str(), &row.user_data))
                    .collect::<Vec<_>>(),
                observed,
            )
            .await?,
        );
    }
    Ok((changed, previous_albums))
}

fn reference_patch<T: serde::Serialize + Default>(
    metadata: &T,
) -> LibraryResult<serde_json::Value> {
    fn retain(value: &mut serde_json::Value, default: &serde_json::Value) -> bool {
        if let serde_json::Value::Object(fields) = value {
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

pub(crate) async fn remember_track(
    connection: &mut SqliteConnection,
    reference: &crate::TrackReference,
) -> LibraryResult<crate::TrackKey> {
    let mut uri = reference.media_uri.clone();
    let (source, object) = if let Some((source, _, object)) =
        crate::source_entity_parts(&uri).filter(|(_, kind, _)| kind == "track")
    {
        let source = crate::db::write_source_identity(connection, source.as_str()).await?;
        if let Some(known) = sqlx::query_scalar::<_, String>(
            "SELECT media_uri FROM tracks WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(&object)
        .fetch_optional(&mut *connection)
        .await?
        {
            uri = known;
        }
        (Some(source), Some(object))
    } else {
        (None, None)
    };
    if reference.metadata == crate::TrackMetadata::default() && reference.artwork_binding.is_none()
    {
        if let Some(key) = sqlx::query_scalar("SELECT track_key FROM tracks WHERE media_uri=?1")
            .bind(&uri)
            .fetch_optional(&mut *connection)
            .await?
        {
            return Ok(key);
        }
    }
    let changed: Option<(crate::TrackKey,Option<i64>,Option<String>,sqlx::types::Json<crate::TrackMetadata>)> = sqlx::query_as("INSERT INTO tracks(media_uri,source_key,object_id,metadata,indexed,native_artwork,normalized_search) VALUES(?1,?2,?3,jsonb(?6),0,?4,?5) ON CONFLICT(media_uri) DO UPDATE SET metadata=jsonb_patch(tracks.metadata,?6),native_artwork=coalesce(tracks.native_artwork,excluded.native_artwork) WHERE native_metadata=0 AND(tracks.metadata IS NOT jsonb_patch(tracks.metadata,?6) OR(tracks.native_artwork IS NULL AND excluded.native_artwork IS NOT NULL)) RETURNING track_key,source_key,object_id,json(metadata)")
        .bind(&uri).bind(source).bind(object.as_deref()).bind(&reference.artwork_binding).bind(track_search(&reference.metadata)).bind(sqlx::types::Json(reference_patch(&reference.metadata)?)).fetch_optional(&mut *connection).await?;
    let Some((key, source, object, metadata)) = changed else {
        return Ok(
            sqlx::query_scalar("SELECT track_key FROM tracks WHERE media_uri=?1")
                .bind(&uri)
                .fetch_one(connection)
                .await?,
        );
    };
    sqlx::query(
        "UPDATE tracks SET normalized_search=?2 WHERE track_key=?1 AND normalized_search IS NOT ?2",
    )
    .bind(key)
    .bind(track_search(&metadata))
    .execute(&mut *connection)
    .await?;
    if let Some((source, object)) = source.zip(object) {
        let source_id: String =
            sqlx::query_scalar("SELECT object_id FROM sources WHERE source_key=?1")
                .bind(source)
                .fetch_one(&mut *connection)
                .await?;
        track_indexes(
            connection,
            source,
            &source_id,
            &[&TrackObservation {
                object_id: object,
                metadata: metadata.0,
                ..Default::default()
            }],
        )
        .await?;
    }
    super::artwork::refresh_choices(connection, &[key.raw()], &[], &[], true).await?;
    Ok(key)
}

pub(super) async fn observation_stamp(
    connection: &mut SqliteConnection,
    source: i64,
    observed: Option<i64>,
) -> LibraryResult<i64> {
    if let Some(observed) = observed {
        return Ok(observed);
    }
    Ok(sqlx::query_scalar("UPDATE sources SET observation_stamp=observation_stamp+1 WHERE source_key=?1 RETURNING observation_stamp").bind(source).fetch_one(connection).await?)
}

impl Database {
    pub async fn write_direct_track_metadata(
        &self,
        uri: &str,
        metadata: &crate::TrackMetadata,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        Ok(sqlx::query("INSERT INTO tracks(media_uri,metadata,normalized_search,native_metadata) VALUES(?1,jsonb(?2),?3,1) ON CONFLICT(media_uri) DO UPDATE SET metadata=excluded.metadata,normalized_search=excluded.normalized_search,native_metadata=1 WHERE tracks.source_key IS NULL AND(tracks.metadata IS NOT excluded.metadata OR tracks.native_metadata=0)")
            .bind(uri).bind(serde_json::to_string(metadata)?).bind(track_search(metadata)).execute(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?).await?.rows_affected()>0)
    }
}

pub(crate) async fn remember_collection(
    connection: &mut SqliteConnection,
    kind: &str,
    source: SourceKey,
    object_id: &str,
    uri: Option<&str>,
    metadata: &serde_json::Value,
    artwork: Option<Vec<u8>>,
) -> LibraryResult<i64> {
    let source_id: String = sqlx::query_scalar("SELECT object_id FROM sources WHERE source_key=?1")
        .bind(source)
        .fetch_one(&mut *connection)
        .await?;
    let (table, key_column) = table(kind);
    let previous: Option<(i64, bool)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {key_column},native_metadata FROM {table} WHERE source_key=?1 AND object_id=?2"
    )))
    .bind(source)
    .bind(object_id)
    .fetch_optional(&mut *connection)
    .await?;
    if let Some((key, true)) = previous {
        return Ok(key);
    }
    let normalized = metadata
        .get("title")
        .or_else(|| metadata.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(if kind == "folder" { object_id } else { "" })
        .to_lowercase();
    macro_rules! remember {
        ($metadata:ty) => {{
            let metadata: $metadata = serde_json::from_value(metadata.clone())?;
            write_headers(
                connection,
                source.raw(),
                &source_id,
                kind,
                &[(object_id, &metadata, normalized, artwork.clone())],
                false,
                false,
                None,
            )
            .await?;
        }};
    }
    match kind {
        "album" => remember!(crate::AlbumMetadata),
        "artist" => remember!(crate::ArtistMetadata),
        "genre" | "mood" => remember!(crate::TagMetadata),
        "folder" => remember!(crate::FolderMetadata),
        _ => unreachable!("collection reference kind"),
    }
    let (table, key) = self::table(kind);
    if let Some(uri) = uri {
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET media_uri=?3 WHERE source_key=?1 AND object_id=?2 AND indexed=0 AND media_uri<>?3"))).bind(source).bind(object_id).bind(uri).execute(&mut *connection).await?;
    }
    let key: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT {key} FROM {table} WHERE source_key=?1 AND object_id=?2"
    )))
    .bind(source)
    .bind(object_id)
    .fetch_one(&mut *connection)
    .await?;
    if kind == "album" {
        let album = AlbumObservation {
            object_id: object_id.to_owned(),
            metadata: serde_json::from_value(metadata.clone())?,
            artwork_binding: artwork,
            ..Default::default()
        };
        super::collections::headers(connection, source.raw(), &source_id, &[&album], &[], None)
            .await?;
        super::files::album_order(connection, &[key]).await?;
    }
    super::artwork::refresh_choices(
        connection,
        &[],
        if kind == "album" {
            std::slice::from_ref(&key)
        } else {
            &[]
        },
        if kind == "artist" {
            std::slice::from_ref(&key)
        } else {
            &[]
        },
        true,
    )
    .await?;
    Ok(key)
}

impl super::Catalog<'_> {
    pub async fn apply_catalog(&mut self, batch: &CatalogBatch, admit: bool) -> LibraryResult<()> {
        let source_id = self.source_id;
        let observed = self.observed;
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let source = self.source.raw();
        let observed = Some(observation_stamp(&mut tx, source, observed).await?);
        let mut update = CatalogUpdate::default();
        let mut changed_albums = Vec::new();
        macro_rules! write {
            ($kind:literal,$rows:expr,$caption:expr,$art:expr) => {{
                let rows = &$rows;
                let inputs = rows
                    .iter()
                    .map(|row| {
                        Ok((
                            row.object_id.as_str(),
                            &row.metadata,
                            ($caption)(row),
                            ($art)(row)?,
                        ))
                    })
                    .collect::<LibraryResult<Vec<_>>>()?;
                let change = write_headers(
                    &mut tx,
                    source,
                    source_id,
                    $kind,
                    &inputs,
                    admit,
                    !batch.reference,
                    observed,
                )
                .await?;
                update.metadata |= !change.0.is_empty();
                if $kind == "album" {
                    changed_albums = change.0;
                }
                update.artwork |= change.1;
                stamp(
                    &mut tx,
                    source,
                    $kind,
                    &rows
                        .iter()
                        .map(|row| row.object_id.as_str())
                        .collect::<Vec<_>>(),
                    observed,
                )
                .await?;
            }};
        }
        write!(
            "album",
            batch.albums,
            |row: &AlbumObservation| row.metadata.title.to_lowercase(),
            |row: &AlbumObservation| Ok::<_, LibraryError>(row.artwork_binding.clone())
        );
        write!(
            "artist",
            batch.artists,
            |row: &ArtistObservation| row.metadata.name.to_lowercase(),
            |row: &ArtistObservation| Ok::<_, LibraryError>(row.artwork_binding.clone())
        );
        write!(
            "genre",
            batch.genres,
            |row: &GenreObservation| row.metadata.name.to_lowercase(),
            |row: &GenreObservation| Ok::<_, LibraryError>(row.artwork_binding.clone())
        );
        write!(
            "mood",
            batch.moods,
            |row: &MoodObservation| row.metadata.name.to_lowercase(),
            |_: &MoodObservation| Ok::<_, LibraryError>(None)
        );
        write!(
            "folder",
            batch.folders,
            |row: &FolderObservation| row
                .metadata
                .name
                .as_deref()
                .unwrap_or(&row.object_id)
                .to_lowercase(),
            |row: &FolderObservation| Ok::<_, LibraryError>(row.artwork_binding.clone())
        );
        let (track_update, previous_albums) = tracks(
            &mut tx,
            source,
            source_id,
            &batch.tracks,
            observed,
            admit,
            !batch.reference,
        )
        .await?;
        update.merge(track_update);
        for (kind, states) in [
            (
                "album",
                batch
                    .albums
                    .iter()
                    .map(|row| (row.object_id.as_str(), &row.user_data))
                    .collect::<Vec<_>>(),
            ),
            (
                "artist",
                batch
                    .artists
                    .iter()
                    .map(|row| (row.object_id.as_str(), &row.user_data))
                    .collect::<Vec<_>>(),
            ),
        ] {
            update.merge(users(&mut tx, source, kind, &states, observed).await?);
        }
        update.metadata |= super::collections::headers(
            &mut tx,
            source,
            source_id,
            &batch
                .albums
                .iter()
                .filter(|row| changed_albums.contains(&row.object_id))
                .collect::<Vec<_>>(),
            &batch.folders,
            observed,
        )
        .await?;
        update.playlists |= super::collections::playlists(
            &mut tx,
            source,
            &batch.playlists,
            observed,
            admit,
            batch.reference,
        )
        .await?;
        if update.metadata || update.artwork {
            update.artwork |= super::artwork::refresh_batch(
                &mut tx,
                source,
                &batch.tracks,
                &batch.albums,
                &batch.artists,
                &previous_albums,
                &update,
            )
            .await?;
        }
        if update.changed() {
            sqlx::query(
                "UPDATE sources SET catalog_revision=catalog_revision+1 WHERE source_key=?1",
            )
            .bind(source)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.changes.merge(update);
        Ok(())
    }
    pub async fn complete_source_observation(
        &mut self,
        observed: i64,
        kinds: &[&str],
        freshness: Option<&Freshness>,
    ) -> LibraryResult<()> {
        let mut update = CatalogUpdate::default();
        for kind in kinds {
            let mut after = 0_i64;
            loop {
                let mut writer = self.database.writer().await?;
                let mut tx = writer
                    .as_mut()
                    .ok_or(LibraryError::WriterUnavailable)?
                    .begin()
                    .await?;
                let source = self.source.raw();
                let (table, key) = if *kind == "playlist" {
                    ("playlists", "playlist_key")
                } else {
                    table(kind)
                };
                let absent = if *kind == "artist" {
                    " AND NOT EXISTS(SELECT 1 FROM track_artists credit JOIN tracks member USING(track_key) WHERE credit.artist_key=artists.artist_key AND member.indexed=1) AND NOT EXISTS(SELECT 1 FROM track_album_artists credit JOIN tracks member USING(track_key) WHERE credit.artist_key=artists.artist_key AND member.indexed=1) AND NOT EXISTS(SELECT 1 FROM album_artists credit JOIN albums album USING(album_key) WHERE credit.artist_key=artists.artist_key AND(album.indexed=1 OR EXISTS(SELECT 1 FROM tracks member WHERE member.album_key=album.album_key AND member.indexed=1)))"
                } else {
                    ""
                };
                let keys:Vec<i64>=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {key} FROM {table} WHERE source_key=?1 AND indexed=1 AND observed_stamp<?2 AND {key}>?3 {absent} ORDER BY {key} LIMIT 128"))).bind(source).bind(observed).bind(after).fetch_all(&mut *tx).await?;
                let Some(last) = keys.last() else {
                    tx.commit().await?;
                    self.changes.merge(update.clone());
                    break;
                };
                after = *last;
                let albums: Vec<i64> = if *kind == "track" {
                    sqlx::query_scalar("SELECT DISTINCT album_key FROM tracks WHERE track_key IN(SELECT value FROM json_each(?1)) AND album_key IS NOT NULL").bind(serde_json::to_string(&keys)?).fetch_all(&mut *tx).await?
                } else if *kind == "album" {
                    keys.clone()
                } else {
                    Vec::new()
                };
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE {table} SET indexed=0 WHERE {key} IN(SELECT value FROM json_each(?1))"
                )))
                .bind(serde_json::to_string(&keys)?)
                .execute(&mut *tx)
                .await?;
                if *kind == "playlist" {
                    update.playlists = true;
                } else {
                    update.metadata = true;
                }
                if *kind == "track" {
                    crate::loudness::invalidate_album_audio(
                        &mut tx,
                        &albums
                            .iter()
                            .map(|key| crate::AlbumKey::from_raw(*key))
                            .collect::<Vec<_>>(),
                        true,
                    )
                    .await?;
                }
                super::files::album_order(&mut tx, &albums).await?;
                super::artwork::refresh_choices(
                    &mut tx,
                    if *kind == "track" { &keys } else { &[] },
                    &albums,
                    if *kind == "artist" { &keys } else { &[] },
                    false,
                )
                .await?;
                tx.commit().await?;
                self.changes.merge(update.clone());
            }
        }
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let source = self.source.raw();
        if kinds.is_empty() {
            sqlx::query("UPDATE sources SET catalog_complete=1,freshness=?2,catalog_revision=catalog_revision+?3 WHERE source_key=?1").bind(source).bind(freshness.map(Freshness::as_bytes)).bind(update.changed()).execute(&mut *tx).await?;
        } else if update.changed() {
            sqlx::query(
                "UPDATE sources SET catalog_revision=catalog_revision+1 WHERE source_key=?1",
            )
            .bind(source)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.changes.merge(update.clone());
        self.changes.merge(update);
        Ok(())
    }
}

impl super::Catalog<'_> {
    pub async fn start_observation(&mut self) -> LibraryResult<i64> {
        let mut writer = self.database.writer().await?;
        let stamp = sqlx::query_scalar("UPDATE sources SET observation_stamp=observation_stamp+1 WHERE source_key=?1 RETURNING observation_stamp")
            .bind(self.source).fetch_one(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?).await?;
        self.observed = Some(stamp);
        Ok(stamp)
    }
}
