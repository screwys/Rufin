use super::*;
use crate::{ArtworkCandidate, ArtworkImage};
use sqlx::{QueryBuilder, Sqlite};

pub(crate) fn artist_binding(
    name: &str,
    musicbrainz_id: Option<&str>,
    binding: Option<&[u8]>,
) -> LibraryResult<Vec<u8>> {
    let mut candidates = binding
        .map(serde_json::from_slice::<Vec<ArtworkCandidate>>)
        .transpose()?
        .unwrap_or_default();
    candidates.retain(|candidate| !matches!(candidate.image, ArtworkImage::Artist { .. }));
    candidates.push(ArtworkCandidate::new(
        ArtworkImage::Artist {
            name: name.to_owned(),
            musicbrainz_id: musicbrainz_id.map(str::to_owned),
        },
        None,
    ));
    Ok(serde_json::to_vec(&candidates)?)
}

pub(super) async fn write_images(
    connection: &mut SqliteConnection,
    source: i64,
    kind: &str,
    rows: &[(&str, Option<&[u8]>)],
    observed: Option<i64>,
    native: bool,
) -> LibraryResult<bool> {
    if rows.is_empty() {
        return Ok(false);
    }
    let bindings = if kind == "artist" {
        let names = sqlx::query_as::<_,(String,String,Option<String>,Option<Vec<u8>>)>("SELECT object_id,name,musicbrainz_artist_id,native_artwork FROM artists WHERE source_key=?1 AND object_id IN(SELECT value FROM json_each(?2))")
            .bind(source).bind(serde_json::to_string(&rows.iter().map(|(id,_)|id).collect::<Vec<_>>())?).fetch_all(&mut *connection).await?
            .into_iter().map(|(id,name,mbid,artwork)|(id,(name,mbid,artwork))).collect::<std::collections::HashMap<_,_>>();
        rows.iter()
            .map(|(id, artwork)| {
                if let Some((name, mbid, previous)) = names.get(*id) {
                    let artwork = if native {
                        *artwork
                    } else {
                        previous.as_deref().or(*artwork)
                    };
                    Ok(Some(artist_binding(name, mbid.as_deref(), artwork)?))
                } else {
                    Ok(artwork.map(<[u8]>::to_vec))
                }
            })
            .collect::<LibraryResult<Vec<_>>>()?
    } else {
        rows.iter()
            .map(|(_, artwork)| artwork.map(<[u8]>::to_vec))
            .collect()
    };
    let (table, _) = super::cache::table(kind);
    let next = if kind == "track" && native {
        "CASE WHEN EXISTS(SELECT 1 FROM local_files physical WHERE physical.source_key=tracks.source_key AND physical.path=tracks.source_path AND(physical.artist_pictures IS NULL OR physical.picture_index>=0)) THEN coalesce(input.binding,tracks.native_artwork) ELSE input.binding END".to_owned()
    } else if native || kind == "artist" {
        "input.binding".to_owned()
    } else {
        format!("coalesce({table}.native_artwork,input.binding)")
    };
    let mut query = QueryBuilder::<Sqlite>::new("WITH input(object_id,binding) AS(");
    query.push_values(rows.iter().zip(&bindings), |mut row, ((id, _), binding)| {
        row.push_bind(*id).push_bind(binding);
    });
    query
        .push(format!(
            ") UPDATE {table} SET native_artwork={next},observed_stamp=coalesce("
        ))
        .push_bind(observed)
        .push(format!(
            ",{table}.observed_stamp) FROM input WHERE {table}.source_key="
        ))
        .push_bind(source)
        .push(format!(
            " AND {table}.object_id=input.object_id AND observed_stamp<="
        ))
        .push_bind(observed.unwrap_or(i64::MAX))
        .push(" AND ")
        .push(if native { "1" } else { "native_metadata=0" })
        .push(format!(" AND native_artwork IS NOT {next}"));
    Ok(query.build().execute(connection).await?.rows_affected() > 0)
}

pub(crate) async fn refresh_choices(
    connection: &mut SqliteConnection,
    tracks: &[i64],
    albums: &[i64],
    artists: &[i64],
    native_changed: bool,
) -> LibraryResult<bool> {
    if tracks.is_empty() && albums.is_empty() && artists.is_empty() {
        return Ok(false);
    }
    let tracks_json = serde_json::to_string(tracks)?;
    let artists_json = serde_json::to_string(artists)?;
    let albums: Vec<i64> = sqlx::query_scalar(
        "SELECT value FROM json_each(?2)
         UNION SELECT album_key FROM tracks WHERE track_key IN(SELECT value FROM json_each(?1)) AND album_key IS NOT NULL
         UNION SELECT album_key FROM albums WHERE artwork_track_key IN(SELECT value FROM json_each(?1))
         UNION SELECT album_key FROM albums WHERE artwork_artist_key IN(SELECT value FROM json_each(?3))",
    ).bind(&tracks_json).bind(serde_json::to_string(albums)?).bind(&artists_json)
        .fetch_all(&mut *connection).await?;
    let albums_json = serde_json::to_string(&albums)?;
    sqlx::query(
        "WITH chosen AS(SELECT album_key,CASE WHEN native_artwork IS NULL OR json_array_length(native_artwork)=0 THEN
           (SELECT track_key FROM tracks WHERE tracks.album_key=albums.album_key AND indexed=1 AND native_artwork IS NOT NULL AND json_array_length(native_artwork)>0
            ORDER BY disc_number,track_number,sort_text,track_key LIMIT 1) END track_key
           FROM albums WHERE album_key IN(SELECT value FROM json_each(?1)))
         UPDATE albums SET artwork_track_key=chosen.track_key FROM chosen WHERE albums.album_key=chosen.album_key AND artwork_track_key IS NOT chosen.track_key",
    ).bind(&albums_json).execute(&mut *connection).await?;
    let artists: Vec<i64> = sqlx::query_scalar(
        "SELECT value FROM json_each(?3)
         UNION SELECT artist_key FROM artists WHERE artwork_album_key IN(SELECT value FROM json_each(?2)) OR artwork_track_key IN(SELECT value FROM json_each(?1))
         UNION SELECT artist_key FROM track_artists WHERE track_key IN(SELECT value FROM json_each(?1))
         UNION SELECT artist_key FROM effective_album_artists WHERE album_key IN(SELECT value FROM json_each(?2))
         UNION SELECT artist_key FROM track_artists JOIN tracks USING(track_key) WHERE album_key IN(SELECT value FROM json_each(?2)) AND indexed=1",
    ).bind(&tracks_json).bind(&albums_json).bind(&artists_json).fetch_all(&mut *connection).await?;
    let artists_json = serde_json::to_string(&artists)?;
    let related_albums = "SELECT credit.album_key FROM album_artists credit JOIN albums album USING(album_key) WHERE credit.artist_key=artist.artist_key AND album.metadata IS NOT NULL
        UNION SELECT track.album_key FROM track_album_artists credit JOIN tracks track USING(track_key) JOIN albums album USING(album_key) WHERE credit.artist_key=artist.artist_key AND track.indexed=1 AND album.metadata IS NULL
        UNION SELECT track.album_key FROM track_artists credit JOIN tracks track USING(track_key) WHERE credit.artist_key=artist.artist_key AND track.indexed=1 AND track.album_key IS NOT NULL";
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "WITH chosen AS(SELECT artist.artist_key,
           (SELECT album.album_key FROM ({related_albums}) related JOIN albums album USING(album_key) JOIN album_order ordering USING(album_key)
            WHERE (album.native_artwork IS NOT NULL AND json_array_length(album.native_artwork)>0 OR album.artwork_track_key IS NOT NULL)
              AND(album.indexed=1 OR EXISTS(SELECT 1 FROM tracks WHERE tracks.album_key=album.album_key AND indexed=1))
            ORDER BY ordering.title_sort,album.album_key LIMIT 1) album_key,
           (SELECT track.track_key FROM track_artists credit JOIN tracks track USING(track_key)
            WHERE credit.artist_key=artist.artist_key AND track.indexed=1 AND track.album_key IS NULL AND track.native_artwork IS NOT NULL AND json_array_length(track.native_artwork)>0
            ORDER BY track.sort_text,track.track_key LIMIT 1) track_key
         FROM artists artist WHERE artist.artist_key IN(SELECT value FROM json_each(?1)))
         UPDATE artists SET artwork_album_key=chosen.album_key,artwork_track_key=CASE WHEN chosen.album_key IS NULL THEN chosen.track_key END FROM chosen
         WHERE artists.artist_key=chosen.artist_key AND(artwork_album_key,artwork_track_key) IS NOT(chosen.album_key,CASE WHEN chosen.album_key IS NULL THEN chosen.track_key END)"
    ))).bind(&artists_json).execute(&mut *connection).await?;
    let changed_artists: Vec<i64> = sqlx::query_scalar(
        "WITH chosen AS(SELECT artist.artist_key,(SELECT CAST(json_group_array(json(value) ORDER BY ordinal) AS BLOB) FROM(
           SELECT value,CASE WHEN json_extract(value,'$.image.kind') IN('artist','album') THEN 200000 ELSE 0 END+CAST(key AS INTEGER) ordinal FROM json_each(artist.native_artwork)
           UNION ALL SELECT value,100000+CAST(key AS INTEGER) FROM json_each(coalesce(
             (SELECT coalesce(nullif(album.native_artwork,CAST('[]' AS BLOB)),track.native_artwork) FROM albums album LEFT JOIN tracks track ON track.track_key=album.artwork_track_key WHERE album.album_key=artist.artwork_album_key),
             (SELECT native_artwork FROM tracks WHERE track_key=artist.artwork_track_key)))) HAVING count(*)>0) binding
         FROM artists artist WHERE artist.artist_key IN(SELECT value FROM json_each(?1)))
         UPDATE artists SET artwork_binding=chosen.binding FROM chosen WHERE artists.artist_key=chosen.artist_key AND artwork_binding IS NOT chosen.binding RETURNING artist_key",
    ).bind(&artists_json).fetch_all(&mut *connection).await?;
    let changed_artists_json = serde_json::to_string(&changed_artists)?;
    let albums: Vec<i64> = sqlx::query_scalar("SELECT value FROM json_each(?1) UNION SELECT album_key FROM albums WHERE artwork_artist_key IN(SELECT value FROM json_each(?2))")
        .bind(&albums_json).bind(&changed_artists_json).fetch_all(&mut *connection).await?;
    let albums_json = serde_json::to_string(&albums)?;
    let changed_albums: Vec<i64> = sqlx::query_scalar(
        "WITH chosen AS(SELECT album.album_key,CASE WHEN (album.native_artwork IS NULL OR json_array_length(album.native_artwork)=0) AND album.artwork_track_key IS NULL
           THEN(SELECT artist_key FROM effective_album_artists WHERE album_key=album.album_key ORDER BY position LIMIT 1) END artist_key FROM albums album WHERE album.album_key IN(SELECT value FROM json_each(?1))),
         bindings AS(SELECT album.album_key,chosen.artist_key,coalesce(nullif(album.native_artwork,CAST('[]' AS BLOB)),track.native_artwork,artist.artwork_binding) binding
           FROM chosen JOIN albums album USING(album_key) LEFT JOIN tracks track ON track.track_key=album.artwork_track_key LEFT JOIN artists artist ON artist.artist_key=chosen.artist_key)
         UPDATE albums SET artwork_binding=bindings.binding,artwork_artist_key=bindings.artist_key FROM bindings
         WHERE albums.album_key=bindings.album_key AND(artwork_binding,artwork_artist_key) IS NOT(bindings.binding,bindings.artist_key) RETURNING album_key",
    ).bind(&albums_json).fetch_all(&mut *connection).await?;
    let changed_tracks =
        refresh_track_choices(connection, tracks, &changed_albums, &changed_artists).await?;
    if native_changed || changed_tracks || !changed_albums.is_empty() || !changed_artists.is_empty()
    {
        crate::artwork::refresh_mosaics(
            connection,
            if native_changed || changed_tracks {
                tracks
            } else {
                &[]
            },
            &changed_albums,
            &changed_artists,
        )
        .await?;
    }
    Ok(changed_tracks || !changed_albums.is_empty() || !changed_artists.is_empty())
}

pub(crate) async fn refresh_track_choices(
    connection: &mut SqliteConnection,
    tracks: &[i64],
    albums: &[i64],
    artists: &[i64],
) -> LibraryResult<bool> {
    let changed: Vec<i64> = sqlx::query_scalar(
        "WITH requested AS(SELECT value track_key FROM json_each(?1)
           UNION SELECT track_key FROM tracks WHERE album_key IN(SELECT value FROM json_each(?2))
           UNION SELECT track_key FROM tracks WHERE artwork_artist_key IN(SELECT value FROM json_each(?3))),
         chosen AS(SELECT track.track_key,
           CASE WHEN track.native_artwork IS NOT NULL AND json_array_length(track.native_artwork)>0 AND(coalesce(source.distinct_track_covers,0)=1 OR album.artwork_binding IS NULL) THEN 1
                WHEN album.artwork_binding IS NOT NULL THEN 2 ELSE 3 END kind,
           (SELECT artist_key FROM track_artists WHERE track_key=track.track_key ORDER BY position LIMIT 1) artist_key
           FROM requested JOIN tracks track USING(track_key) LEFT JOIN sources source USING(source_key) LEFT JOIN albums album USING(album_key))
         UPDATE tracks SET artwork_kind=chosen.kind,artwork_artist_key=CASE WHEN chosen.kind=3 THEN chosen.artist_key END FROM chosen
         WHERE tracks.track_key=chosen.track_key AND(artwork_kind,artwork_artist_key) IS NOT(chosen.kind,CASE WHEN chosen.kind=3 THEN chosen.artist_key END) RETURNING track_key",
    ).bind(serde_json::to_string(tracks)?).bind(serde_json::to_string(albums)?).bind(serde_json::to_string(artists)?).fetch_all(&mut *connection).await?;
    Ok(!changed.is_empty())
}

pub(crate) async fn refresh_batch(
    connection: &mut SqliteConnection,
    source: i64,
    tracks: &[TrackObservation],
    albums: &[AlbumObservation],
    artists: &[ArtistObservation],
    previous_albums: &[i64],
    update: &CatalogUpdate,
) -> LibraryResult<bool> {
    if tracks.is_empty() && albums.is_empty() && artists.is_empty() {
        return Ok(false);
    }
    let mut keys = Vec::new();
    for (kind, ids) in [
        (
            "track",
            tracks.iter().map(|row| &row.object_id).collect::<Vec<_>>(),
        ),
        ("album", albums.iter().map(|row| &row.object_id).collect()),
        ("artist", artists.iter().map(|row| &row.object_id).collect()),
    ] {
        let (table, key) = super::cache::table(kind);
        keys.push(sqlx::query_scalar::<_,i64>(sqlx::AssertSqlSafe(format!("SELECT {key} FROM {table} WHERE source_key=?1 AND object_id IN(SELECT value FROM json_each(?2))")))
            .bind(source).bind(serde_json::to_string(&ids)?).fetch_all(&mut *connection).await?);
    }
    keys[1].extend(previous_albums);
    if update.metadata {
        keys[1] = sqlx::query_scalar("SELECT value FROM json_each(?2) UNION SELECT album_key FROM tracks WHERE track_key IN(SELECT value FROM json_each(?1)) AND album_key IS NOT NULL UNION SELECT album_key FROM album_artists WHERE artist_key IN(SELECT value FROM json_each(?3)) UNION SELECT album_key FROM track_album_artists JOIN tracks USING(track_key) WHERE artist_key IN(SELECT value FROM json_each(?3)) AND album_key IS NOT NULL")
            .bind(serde_json::to_string(&keys[0])?).bind(serde_json::to_string(&keys[1])?).bind(serde_json::to_string(&keys[2])?).fetch_all(&mut *connection).await?;
        super::files::album_order(connection, &keys[1]).await?;
    }
    refresh_choices(connection, &keys[0], &keys[1], &keys[2], update.artwork).await
}

pub(crate) async fn initialize_choices(connection: &mut SqliteConnection) -> LibraryResult<()> {
    for kind in ["album", "artist", "track"] {
        let (table, key) = super::cache::table(kind);
        let mut after = 0;
        loop {
            let keys: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT {key} FROM {table} WHERE {key}>?1 ORDER BY {key} LIMIT 128"
            )))
            .bind(after)
            .fetch_all(&mut *connection)
            .await?;
            let Some(last) = keys.last() else { break };
            after = *last;
            refresh_choices(
                connection,
                if kind == "track" { &keys } else { &[] },
                if kind == "album" { &keys } else { &[] },
                if kind == "artist" { &keys } else { &[] },
                true,
            )
            .await?;
        }
    }
    Ok(())
}

impl super::Catalog<'_> {
    pub async fn set_source_artwork(
        &mut self,
        kind: &str,
        rows: &[(String, Option<Vec<u8>>)],
    ) -> LibraryResult<()> {
        let observed = self.observed;
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let source = self.source.raw();
        let observed = super::cache::observation_stamp(&mut tx, source, observed).await?;
        let mut update = CatalogUpdate::default();
        for page in rows.chunks(128) {
            if page.is_empty() {
                continue;
            }
            let changed = write_images(
                &mut tx,
                source,
                kind,
                &page
                    .iter()
                    .map(|(id, artwork)| (id.as_str(), artwork.as_deref()))
                    .collect::<Vec<_>>(),
                Some(observed),
                true,
            )
            .await?;
            update.artwork |= changed;
            if changed {
                let (table, key) = super::cache::table(kind);
                let keys: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {key} FROM {table} WHERE source_key=?1 AND object_id IN(SELECT value FROM json_each(?2))")))
                    .bind(source).bind(serde_json::to_string(&page.iter().map(|(id,_)|id).collect::<Vec<_>>())?).fetch_all(&mut *tx).await?;
                refresh_choices(
                    &mut tx,
                    if kind == "track" { &keys } else { &[] },
                    if kind == "album" { &keys } else { &[] },
                    if kind == "artist" { &keys } else { &[] },
                    true,
                )
                .await?;
            }
        }
        tx.commit().await?;
        self.changes.merge(update);
        Ok(())
    }
}
