//! Resolved catalog metadata, membership contributions and totals share this write owner.
use super::*;
use sqlx::{QueryBuilder, Sqlite};

const METRICS: [&str; 6] = [
    "album_count",
    "track_count",
    "duration_millis",
    "downloaded_count",
    "play_count",
    "last_played",
];
const OWNERS: [(&str, i32, &str); 5] = [
    ("folder", 0, "track_folders"),
    ("artist", 0, "track_artists"),
    ("artist", 1, "track_album_artists"),
    ("genre", 0, "track_genres"),
    ("mood", 0, "track_moods"),
];

pub(crate) async fn albums(
    connection: &mut SqliteConnection,
    keys: &[i64],
) -> LibraryResult<Vec<i64>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let input = serde_json::to_string(keys)?;
    sqlx::query("UPDATE albums SET input_metadata=NULL,indexed=0,cached_local=0,accepted_audio=NULL WHERE cached_local=1 AND album_key IN(SELECT value FROM json_each(?1)) AND NOT EXISTS(SELECT 1 FROM tracks WHERE tracks.album_key=albums.album_key AND indexed=1 AND json_type(metadata,'$.album_tags') IS NOT 'object')")
        .bind(&input).execute(&mut *connection).await?;
    // Observed file tags own the album even when its header is derived.
    sqlx::query("UPDATE albums SET native_metadata=1,input_metadata=NULL WHERE native_metadata=0 AND cached_local=0 AND album_key IN(SELECT value FROM json_each(?1)) AND EXISTS(SELECT 1 FROM tracks WHERE tracks.album_key=albums.album_key AND indexed=1 AND json_type(metadata,'$.album_tags')='object')")
        .bind(&input).execute(&mut *connection).await?;
    for (relation, target, edge, array) in [
        (
            "album_artists",
            "artist",
            "track_album_artists",
            "album_artists",
        ),
        ("album_genres", "genre", "track_genres", "genres"),
    ] {
        let expected = format!("SELECT album_key,{target}_key,row_number() OVER(PARTITION BY album_key ORDER BY path,disc,number,object_id,ordinal)-1 position FROM (
            SELECT *,row_number() OVER(PARTITION BY album_key,{target}_key ORDER BY path,disc,number,object_id,ordinal) first_credit FROM (
                SELECT album.album_key,credit.{target}_key,NULL path,0 disc,0 number,'' object_id,CAST(item.key AS INTEGER) ordinal
                FROM albums album JOIN json_each(album.input_metadata,'$.{array}') item
                JOIN {target}s credit ON credit.source_key=album.source_key AND credit.object_id=json_extract(item.value,'$.object_id')
                WHERE album.album_key IN(SELECT value FROM json_each(?1)) AND album.input_metadata IS NOT NULL
                UNION ALL SELECT track.album_key,credit.{target}_key,coalesce(track.cue_path,track.source_path),track.disc_number,track.track_number,track.object_id,credit.position
                FROM tracks track INDEXED BY tracks_album_order_idx JOIN {edge} credit USING(track_key) JOIN albums album USING(album_key)
                WHERE track.album_key IN(SELECT value FROM json_each(?1)) AND track.indexed=1 AND album.input_metadata IS NULL
            )) WHERE first_credit=1");
        replace(
            connection,
            relation,
            &format!("{target}_key"),
            &expected,
            &input,
        )
        .await?;
    }
    let first = |field: &str| {
        format!(
            "(SELECT json_extract(member.metadata,'$.{field}') FROM tracks member INDEXED BY tracks_album_metadata_order_idx WHERE member.album_key=album.album_key AND member.indexed=1 AND json_extract(member.metadata,'$.{field}') IS NOT NULL AND json_extract(member.metadata,'$.{field}')<>'' ORDER BY coalesce(member.cue_path,member.source_path),member.disc_number,member.track_number,member.object_id LIMIT 1)"
        )
    };
    let field = |native: &str, local: &str| {
        format!(
            "CASE WHEN album.input_metadata IS NOT NULL THEN json_extract(album.input_metadata,'$.{native}') ELSE {} END",
            first(local)
        )
    };
    let raw_id = |native: &str, local: &str| {
        format!(
            "CASE WHEN album.input_metadata IS NOT NULL THEN json_extract(album.input_metadata,'$.{native}') ELSE (SELECT json_extract(member.metadata,'$.{local}') FROM tracks member INDEXED BY tracks_album_metadata_order_idx WHERE member.album_key=album.album_key AND member.indexed=1 AND json_extract(member.metadata,'$.{local}') IS NOT NULL ORDER BY coalesce(member.cue_path,member.source_path),member.disc_number,member.track_number,member.object_id LIMIT 1) END"
        )
    };
    let group = raw_id(
        "musicbrainz_release_group_id",
        "musicbrainz_release_group_id",
    );
    let release = raw_id("musicbrainz_release_id", "musicbrainz_album_id");
    let identity = format!(
        "CASE WHEN ({group}) IS NOT NULL THEN 'release-group:'||({group}) WHEN ({release}) IS NOT NULL THEN 'release:'||({release}) END"
    );
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE albums AS album SET release_identity={identity},release_lookup_identity=CASE WHEN release_lookup_identity IS ({identity}) THEN release_lookup_identity END WHERE album_key IN(SELECT value FROM json_each(?1)) AND (release_identity IS NOT ({identity}) OR release_lookup_identity IS NOT NULL AND release_lookup_identity IS NOT ({identity}))")))
        .bind(&input).execute(&mut *connection).await?;
    let expected="SELECT album_key,release_type,row_number() OVER(PARTITION BY album_key ORDER BY path,disc,number,object_id,ordinal)-1 position FROM (
        SELECT *,row_number() OVER(PARTITION BY album_key,release_type ORDER BY path,disc,number,object_id,ordinal) first_credit FROM (
            SELECT album.album_key,item.value release_type,NULL path,0 disc,0 number,'' object_id,CAST(item.key AS INTEGER) ordinal,1 native
            FROM albums album JOIN json_each(coalesce((SELECT release_types FROM album_release_observations accepted WHERE accepted.album_key=album.album_key AND accepted.lookup_identity=album.release_lookup_identity),json_extract(album.input_metadata,'$.release_types'))) item
            WHERE album.album_key IN(SELECT value FROM json_each(?1)) AND (album.input_metadata IS NOT NULL OR EXISTS(SELECT 1 FROM album_release_observations accepted WHERE accepted.album_key=album.album_key AND accepted.lookup_identity=album.release_lookup_identity))
            UNION ALL SELECT track.album_key,item.value,coalesce(track.cue_path,track.source_path),track.disc_number,track.track_number,track.object_id,CAST(item.key AS INTEGER),0
            FROM tracks track INDEXED BY tracks_album_order_idx JOIN albums album USING(album_key) JOIN json_each(track.metadata,'$.album_tags.release_types') item
            WHERE track.album_key IN(SELECT value FROM json_each(?1)) AND track.indexed=1 AND album.input_metadata IS NULL
              AND NOT EXISTS(SELECT 1 FROM album_release_observations accepted WHERE accepted.album_key=album.album_key AND accepted.lookup_identity=album.release_lookup_identity)
        )) WHERE native=1 OR first_credit=1";
    replace(
        connection,
        "album_release_types",
        "release_type",
        expected,
        &input,
    )
    .await?;
    let artist = "(SELECT group_concat(artist.name,', ' ORDER BY credit.position) FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key ORDER BY credit.position)";
    let added = "coalesce(unixepoch(json_extract(album.input_metadata,'$.date_added')),album.first_seen_at,(SELECT coalesce(unixepoch(member.date_added),member.first_seen_at) FROM tracks member INDEXED BY tracks_album_metadata_order_idx WHERE member.album_key=album.album_key AND member.indexed=1 ORDER BY coalesce(member.cue_path,member.source_path),member.disc_number,member.track_number,member.object_id LIMIT 1))";
    let title = "coalesce(json_extract(album.input_metadata,'$.sort_text'),(SELECT coalesce(json_extract(member.metadata,'$.album_tags.sort_text'),member.display_album,'') FROM tracks member INDEXED BY tracks_album_metadata_order_idx WHERE member.album_key=album.album_key AND member.indexed=1 ORDER BY coalesce(member.cue_path,member.source_path),member.disc_number,member.track_number,member.object_id LIMIT 1),'')";
    let mut changed:Vec<i64>=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("WITH ordering AS MATERIALIZED(
        SELECT album.album_key,{title} title_sort,(SELECT artist.sort_text FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key ORDER BY credit.position LIMIT 1) album_artist_sort,{} year_sort,{} release_sort,{added} added_sort FROM albums album WHERE album_key IN(SELECT value FROM json_each(?1)))
        UPDATE albums SET title_sort=ordering.title_sort,album_artist_sort=ordering.album_artist_sort,year_sort=ordering.year_sort,release_sort=ordering.release_sort,added_sort=ordering.added_sort FROM ordering WHERE albums.album_key=ordering.album_key AND (albums.title_sort,albums.album_artist_sort,albums.year_sort,albums.release_sort,albums.added_sort) IS NOT (ordering.title_sort,ordering.album_artist_sort,ordering.year_sort,ordering.release_sort,ordering.added_sort) RETURNING album_key",field("year","year"),field("release_date","release_date"))))
        .bind(&input).fetch_all(&mut *connection).await?;
    let artists = "(SELECT json_group_array(json_object('object_id',artist.object_id,'name',artist.name,'sort_text',artist.sort_text,'musicbrainz_artist_id',artist.musicbrainz_artist_id) ORDER BY credit.position) FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key)";
    let genres = "(SELECT json_group_array(json_object('object_id',genre.object_id,'name',genre.name,'sort_text',genre.sort_text) ORDER BY credit.position) FROM album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key)";
    let releases = "(SELECT json_group_array(release_type ORDER BY position) FROM album_release_types WHERE album_key=album.album_key)";
    let metadata = format!(
        "jsonb_patch(coalesce(album.input_metadata,jsonb_object('title',coalesce({},''),'artist',coalesce({artist},''),'sort_text',coalesce({},''),'year',{},'release_date',{},'date_added',{},'musicbrainz_release_id',{},'musicbrainz_release_group_id',{},'is_compilation',json(CASE {} WHEN 1 THEN 'true' WHEN 0 THEN 'false' ELSE 'null' END),'album_artists',json({artists}),'genres',json({genres}))),jsonb_object('date_added',date(album.added_sort,'unixepoch'),'release_types',json({releases})))",
        first("album"),
        first("album_tags.sort_text"),
        first("year"),
        first("release_date"),
        first("date_added"),
        first("musicbrainz_album_id"),
        first("musicbrainz_release_group_id"),
        first("album_tags.is_compilation")
    );
    let metadata_changed:Vec<i64>=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("UPDATE albums AS album SET metadata={metadata} WHERE album_key IN(SELECT value FROM json_each(?1)) AND metadata IS NOT ({metadata}) RETURNING album_key")))
        .bind(&input).fetch_all(&mut *connection).await?;
    changed.extend(metadata_changed);
    let captions:Vec<(i64,String,String)>=sqlx::query_as("SELECT album.album_key,coalesce(album.title,''),CASE WHEN album.input_metadata IS NOT NULL THEN coalesce(json_extract(album.input_metadata,'$.artist'),'') ELSE coalesce((SELECT coalesce(nullif(json_extract(member.metadata,'$.album_display_artist'),''),member.display_artist) FROM tracks member INDEXED BY tracks_album_metadata_order_idx WHERE member.album_key=album.album_key AND member.indexed=1 ORDER BY coalesce(member.cue_path,member.source_path),member.disc_number,member.track_number,member.object_id LIMIT 1),'') END FROM albums album WHERE album_key IN(SELECT value FROM json_each(?1))")
        .bind(&input).fetch_all(&mut *connection).await?;
    if captions.is_empty() {
        return Ok(changed);
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH captions(album_key,title,artist) AS(");
    query.push_values(&captions, |mut row, (key, title, artist)| {
        row.push_bind(key)
            .push_bind(title.to_lowercase())
            .push_bind(artist.to_lowercase());
    });
    query.push(") UPDATE albums SET title_search=captions.title,artist_search=captions.artist FROM captions WHERE albums.album_key=captions.album_key AND (title_search,artist_search) IS NOT (captions.title,captions.artist)");
    query.build().execute(&mut *connection).await?;
    let token = "trim(item.value,char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288))";
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE albums SET release_class=classified.class FROM (
        SELECT album.album_key,CASE WHEN coalesce(album.is_compilation,0)<>0 THEN 3 WHEN count(nullif({token},''))=0 OR max({token}='album') THEN 0 WHEN max({token} IN ('compilation','compilations','collection','collections')) THEN 3 WHEN max({token} IN ('ep','e.p.')) THEN 1 WHEN max({token}='single') THEN 2 ELSE 4 END class
        FROM albums album LEFT JOIN album_release_types types USING(album_key) LEFT JOIN json_each('['||replace(json_quote(lower(types.release_type)),'|','\",\"')||']') item WHERE album.album_key IN(SELECT value FROM json_each(?1)) GROUP BY album.album_key
    ) classified WHERE albums.album_key=classified.album_key AND release_class IS NOT classified.class")))
        .bind(input).execute(&mut *connection).await?;
    sqlx::query("UPDATE tracks SET album_title_sort=album.title_sort,album_artist_sort=album.album_artist_sort,album_year_sort=album.year_sort FROM albums album WHERE tracks.album_key=album.album_key AND album.album_key IN(SELECT value FROM json_each(?1)) AND (tracks.album_title_sort,tracks.album_artist_sort,tracks.album_year_sort) IS NOT (album.title_sort,album.album_artist_sort,album.year_sort)")
        .bind(serde_json::to_string(keys)?).execute(connection).await?;
    changed.sort_unstable();
    changed.dedup();
    Ok(changed)
}

async fn replace(
    connection: &mut SqliteConnection,
    relation: &str,
    key: &str,
    expected: &str,
    input: &str,
) -> LibraryResult<()> {
    sqlx::query(sqlx::AssertSqlSafe(format!("WITH expected AS({expected}) DELETE FROM {relation} WHERE album_key IN(SELECT value FROM json_each(?1)) AND NOT EXISTS(SELECT 1 FROM expected WHERE expected.album_key={relation}.album_key AND expected.{key}={relation}.{key} AND expected.position={relation}.position)")))
        .bind(input).execute(&mut *connection).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("WITH expected AS({expected}) INSERT OR IGNORE INTO {relation}(album_key,{key},position) SELECT album_key,{key},position FROM expected")))
        .bind(input).execute(connection).await?;
    Ok(())
}

fn facts(row: &str, direction: i32) -> String {
    format!(
        "SELECT {row}.track_key,{row}.source_key,{row}.album_key,{row}.indexed,{row}.duration_millis,{row}.is_downloaded downloaded_count,{row}.displayed_play_count play_count,{row}.displayed_last_played last_played,{direction} direction"
    )
}

fn folder_children_delta(edges: &str) -> String {
    let changes = format!(
        "SELECT source_key,parent_folder_key,child_folder_key,sum(direction) count FROM ({edges}) edge WHERE EXISTS(SELECT 1 FROM sources WHERE source_key=edge.source_key) AND EXISTS(SELECT 1 FROM folders WHERE folder_key=edge.child_folder_key) AND (parent_folder_key IS NULL OR EXISTS(SELECT 1 FROM folders WHERE folder_key=edge.parent_folder_key)) GROUP BY source_key,parent_folder_key,child_folder_key HAVING sum(direction)<>0"
    );
    format!("INSERT INTO folder_children(source_key,parent_folder_key,child_folder_key,admitted_track_count) SELECT source_key,parent_folder_key,child_folder_key,count FROM ({changes}) changed WHERE true ON CONFLICT DO UPDATE SET admitted_track_count=folder_children.admitted_track_count+excluded.admitted_track_count;
        DELETE FROM folder_children WHERE folder_child_key IN(SELECT stored.folder_child_key FROM ({changes}) changed CROSS JOIN folder_children stored ON stored.source_key=changed.source_key AND coalesce(stored.parent_folder_key,'')=coalesce(changed.parent_folder_key,'') AND stored.child_folder_key=changed.child_folder_key WHERE stored.admitted_track_count=0);")
}

fn folder_edges(facts: &str) -> String {
    format!(
        "SELECT facts.source_key,parent.folder_key parent_folder_key,child.folder_key child_folder_key,facts.direction FROM ({facts}) facts JOIN track_folders child USING(track_key) LEFT JOIN track_folders parent ON parent.track_key=child.track_key AND parent.position=child.position-1 WHERE facts.indexed=1 AND facts.source_key IS NOT NULL AND (child.position=0 OR parent.folder_key IS NOT NULL)"
    )
}

fn playlist_genre_delta(input: &str) -> String {
    format!("INSERT INTO playlist_genres(playlist_key,genre_key,track_count)
        SELECT playlist_key,genre_key,sum(direction) FROM ({input}) changed
        WHERE EXISTS(SELECT 1 FROM playlists WHERE playlist_key=changed.playlist_key) AND EXISTS(SELECT 1 FROM genres WHERE genre_key=changed.genre_key)
        GROUP BY playlist_key,genre_key HAVING sum(direction)<>0
        ON CONFLICT DO UPDATE SET track_count=playlist_genres.track_count+excluded.track_count;
        DELETE FROM playlist_genres WHERE rowid IN(SELECT stored.rowid FROM ({input}) changed CROSS JOIN playlist_genres stored ON stored.playlist_key=changed.playlist_key AND stored.genre_key=changed.genre_key WHERE stored.track_count=0);")
}

fn folder_path_change(event: &str) -> String {
    let affected = if event == "update" {
        "SELECT OLD.track_key track_key UNION SELECT NEW.track_key"
    } else if event == "delete" {
        "SELECT OLD.track_key track_key"
    } else {
        "SELECT NEW.track_key track_key"
    };
    let current = format!(
        "SELECT track_key,folder_key,position FROM track_folders WHERE track_key IN({affected})"
    );
    let before = match event {
        "insert" => format!("{current} EXCEPT SELECT NEW.track_key,NEW.folder_key,NEW.position"),
        "delete" => format!("{current} UNION SELECT OLD.track_key,OLD.folder_key,OLD.position"),
        _ => format!(
            "SELECT * FROM ({current} EXCEPT SELECT NEW.track_key,NEW.folder_key,NEW.position) UNION SELECT OLD.track_key,OLD.folder_key,OLD.position"
        ),
    };
    folder_children_delta(&format!(
        "WITH paths AS MATERIALIZED(SELECT before.*,-1 direction FROM ({before}) before UNION ALL SELECT current.*,1 FROM ({current}) current) SELECT track.source_key,parent.folder_key parent_folder_key,child.folder_key child_folder_key,child.direction FROM paths child JOIN tracks track USING(track_key) LEFT JOIN paths parent ON parent.track_key=child.track_key AND parent.position=child.position-1 AND parent.direction=child.direction WHERE track.indexed=1 AND track.source_key IS NOT NULL AND (child.position=0 OR parent.folder_key IS NOT NULL)"
    ))
}

fn predicate(kind: &str, role: i32, entity: &str, track: &str) -> String {
    match (kind, role) {
        ("artist", 1) => format!(
            "(({track}.album_key IS NOT NULL AND EXISTS(SELECT 1 FROM album_artists credit WHERE credit.album_key={track}.album_key AND credit.artist_key={entity})) OR ({track}.album_key IS NULL AND EXISTS(SELECT 1 FROM track_album_artists credit WHERE credit.track_key={track}.track_key AND credit.artist_key={entity})))"
        ),
        ("genre", _) => format!(
            "(EXISTS(SELECT 1 FROM track_genres credit WHERE credit.track_key={track}.track_key AND credit.genre_key={entity}) OR EXISTS(SELECT 1 FROM album_genres credit WHERE credit.album_key={track}.album_key AND credit.genre_key={entity}))"
        ),
        _ => format!(
            "EXISTS(SELECT 1 FROM {} credit WHERE credit.track_key={track}.track_key AND credit.{kind}_key={entity})",
            if kind == "playlist" {
                "playlist_entries".into()
            } else {
                format!("track_{kind}s")
            }
        ),
    }
}

fn gallery(keys: &str) -> String {
    ["genre", "mood"].map(|kind| {
        let member = predicate(kind, 0, "stored.entity_key", "candidate");
        let artwork = crate::artwork::raw_track_artwork_sql("candidate");
        format!("UPDATE collection_album_index SET artwork_group_uri=chosen.group_uri,artwork_member_uri=chosen.member_uri,artwork_track_key=chosen.track_key FROM (
            SELECT resolved.summary_key,coalesce(album.media_uri,track.media_uri) group_uri,track.media_uri member_uri,track.track_key FROM (
                SELECT stored.summary_key,CASE WHEN stored.album_key IS NOT NULL THEN (
                    SELECT candidate.track_key FROM tracks candidate INDEXED BY tracks_album_artwork WHERE candidate.album_key=stored.album_key AND candidate.indexed=1 AND {member} AND ({artwork}) IS NOT NULL ORDER BY candidate.media_uri LIMIT 1
                ) ELSE (SELECT candidate.track_key FROM tracks candidate WHERE candidate.track_key=stored.albumless_track_key AND candidate.indexed=1 AND {member} AND ({artwork}) IS NOT NULL) END track_key
                FROM ({keys}) requested CROSS JOIN collection_album_index stored ON stored.summary_key=requested.summary_key WHERE stored.kind='{kind}' AND stored.role=0 AND stored.folder_key IS NULL
            ) resolved LEFT JOIN tracks track USING(track_key) LEFT JOIN albums album USING(album_key)
        ) chosen WHERE collection_album_index.summary_key=chosen.summary_key AND (artwork_group_uri,artwork_member_uri,artwork_track_key) IS NOT (chosen.group_uri,chosen.member_uri,chosen.track_key);")
    }).join("")
}

pub(crate) async fn artwork(
    connection: &mut SqliteConnection,
    tracks: &[i64],
    albums: &[i64],
    artists: &[i64],
) -> LibraryResult<()> {
    let keys = "SELECT stored.summary_key FROM (SELECT value album_key FROM json_each(?2) UNION SELECT album_key FROM tracks WHERE track_key IN(SELECT value FROM json_each(?1)) UNION SELECT album_key FROM tracks WHERE artwork_artist_key IN(SELECT value FROM json_each(?3))) changed CROSS JOIN collection_album_index stored INDEXED BY collection_album_index_album ON stored.album_key=changed.album_key WHERE stored.kind IN('genre','mood') AND stored.folder_key IS NULL
        UNION SELECT stored.summary_key FROM (SELECT value track_key FROM json_each(?1) UNION SELECT track_key FROM tracks WHERE artwork_artist_key IN(SELECT value FROM json_each(?3))) changed CROSS JOIN collection_album_index stored INDEXED BY collection_album_index_albumless ON stored.albumless_track_key=changed.track_key WHERE stored.kind IN('genre','mood') AND stored.folder_key IS NULL";
    for statement in gallery(keys)
        .split(';')
        .filter(|sql| !sql.trim().is_empty())
    {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(serde_json::to_string(tracks)?)
            .bind(serde_json::to_string(albums)?)
            .bind(serde_json::to_string(artists)?)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

fn members(input: &str, kind: &str, role: i32) -> String {
    let edge = if kind == "playlist" {
        "playlist_entries".to_owned()
    } else if kind == "artist" && role == 1 {
        "track_album_artists".into()
    } else {
        format!("track_{kind}s")
    };
    let admitted = if kind == "playlist" {
        "1"
    } else if kind == "artist" && role == 1 {
        "facts.indexed=1 AND facts.album_key IS NULL"
    } else {
        "facts.indexed=1"
    };
    let occurrence = if kind == "playlist" {
        "credit.playlist_entry_key"
    } else {
        "NULL"
    };
    let direct = format!(
        "SELECT '{kind}' kind,credit.{kind}_key entity_key,{role} role,facts.*,{occurrence} occurrence FROM ({input}) facts JOIN {edge} credit USING(track_key) WHERE {admitted}"
    );
    if kind == "genre" || kind == "artist" && role == 1 {
        format!(
            "{direct} UNION SELECT '{kind}',credit.{kind}_key,{role},facts.*,NULL FROM ({input}) facts JOIN album_{kind}s credit USING(album_key) WHERE facts.indexed=1"
        )
    } else {
        direct
    }
}

fn scoped(input: &str) -> String {
    format!(
        "SELECT input.*,NULL folder_key FROM ({input}) input UNION ALL SELECT input.*,scope.folder_key FROM ({input}) input JOIN track_folders scope USING(track_key) WHERE input.kind<>'folder'"
    )
}

fn native_album(kind: &str, role: i32, entity: &str, album: &str, folder: &str) -> String {
    if kind == "genre" || kind == "artist" && role == 1 {
        format!(
            "{folder} IS NULL AND EXISTS(SELECT 1 FROM albums owner JOIN album_{kind}s credit USING(album_key) WHERE owner.album_key={album} AND owner.indexed=1 AND credit.{kind}_key={entity})"
        )
    } else {
        "0".into()
    }
}

fn contribution_delta(input: &str, kind: &str, role: i32) -> String {
    let admitted = if kind == "playlist" {
        "1"
    } else {
        "track.indexed=1"
    };
    let member = predicate(kind, role, "excluded.entity_key", "track");
    let maximum = format!(
        "CASE WHEN excluded.album_key IS NULL THEN (SELECT track.displayed_last_played FROM tracks track WHERE track.track_key=excluded.albumless_track_key AND {admitted} AND {member} AND (excluded.folder_key IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=excluded.folder_key))) ELSE (SELECT track.displayed_last_played FROM tracks track WHERE track.album_key=excluded.album_key AND {admitted} AND {member} AND (excluded.folder_key IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=excluded.folder_key)) ORDER BY track.displayed_last_played DESC LIMIT 1) END"
    );
    let removed = format!(
        "EXISTS(SELECT 1 FROM ({input}) removed WHERE removed.direction<0 AND removed.kind='{kind}' AND removed.role={role} AND removed.entity_key=excluded.entity_key AND removed.album_key IS excluded.album_key AND (excluded.album_key IS NOT NULL OR removed.track_key=excluded.albumless_track_key) AND removed.folder_key IS excluded.folder_key AND removed.last_played=collection_album_index.last_played)"
    );
    let largest = "CASE WHEN excluded.last_played IS NULL THEN collection_album_index.last_played WHEN collection_album_index.last_played IS NULL THEN excluded.last_played ELSE max(collection_album_index.last_played,excluded.last_played) END";
    let native = native_album(
        kind,
        role,
        "excluded.entity_key",
        "excluded.album_key",
        "excluded.folder_key",
    );
    format!("INSERT INTO collection_album_index(kind,entity_key,role,album_key,albumless_track_key,folder_key,track_count,duration_millis,downloaded_count,play_count,last_played,album_count)
        SELECT kind,entity_key,role,album_key,CASE WHEN album_key IS NULL THEN track_key END,folder_key,sum(direction),sum(direction*duration_millis),sum(direction*downloaded_count),sum(direction*play_count),max(CASE WHEN direction>0 THEN last_played END),album_key IS NOT NULL AND sum(direction)>0
        FROM ({input}) changed WHERE kind='{kind}' AND role={role}
          AND (album_key IS NULL OR EXISTS(SELECT 1 FROM albums WHERE albums.album_key=changed.album_key))
          AND (folder_key IS NULL OR EXISTS(SELECT 1 FROM folders WHERE folders.folder_key=changed.folder_key))
          AND EXISTS(SELECT 1 FROM {kind}s WHERE {kind}_key=changed.entity_key)
        GROUP BY entity_key,album_key,CASE WHEN album_key IS NULL THEN track_key END,folder_key
        HAVING sum(direction)<>0 OR sum(direction*duration_millis)<>0 OR sum(direction*downloaded_count)<>0 OR sum(direction*play_count)<>0 OR max(CASE WHEN direction>0 THEN last_played END) IS NOT max(CASE WHEN direction<0 THEN last_played END)
        ON CONFLICT DO UPDATE SET track_count=collection_album_index.track_count+excluded.track_count,duration_millis=collection_album_index.duration_millis+excluded.duration_millis,downloaded_count=collection_album_index.downloaded_count+excluded.downloaded_count,play_count=collection_album_index.play_count+excluded.play_count,
          last_played=CASE WHEN {removed} AND (excluded.last_played IS NULL OR excluded.last_played<collection_album_index.last_played) THEN ({maximum}) ELSE {largest} END,
          album_count=excluded.album_key IS NOT NULL AND (collection_album_index.track_count+excluded.track_count>0 OR ({native}));
        DELETE FROM collection_album_index WHERE summary_key IN(SELECT stored.summary_key FROM ({input}) changed CROSS JOIN collection_album_index stored ON stored.kind=changed.kind AND stored.entity_key=changed.entity_key AND stored.role=changed.role AND coalesce(stored.album_key,'')=coalesce(changed.album_key,'') AND coalesce(stored.albumless_track_key,'')=coalesce(CASE WHEN changed.album_key IS NULL THEN changed.track_key END,'') AND coalesce(stored.folder_key,'')=coalesce(changed.folder_key,'') WHERE stored.kind='{kind}' AND stored.role={role} AND stored.track_count=0 AND stored.album_count=0);")
}

fn canonical_delta(input: &str) -> String {
    format!("UPDATE albums SET track_count=albums.track_count+delta.track_count,duration_millis=albums.duration_millis+delta.duration_millis,downloaded_count=albums.downloaded_count+delta.downloaded_count,play_count=albums.play_count+delta.play_count,
        last_played=CASE WHEN delta.removed_last=albums.last_played AND (delta.last_played IS NULL OR delta.last_played<albums.last_played) THEN (SELECT max(track.displayed_last_played) FROM tracks track WHERE track.album_key=albums.album_key AND track.indexed=1) WHEN delta.last_played IS NULL THEN albums.last_played WHEN albums.last_played IS NULL THEN delta.last_played ELSE max(albums.last_played,delta.last_played) END
        FROM (SELECT album_key,sum(direction) track_count,sum(direction*duration_millis) duration_millis,sum(direction*downloaded_count) downloaded_count,sum(direction*play_count) play_count,max(CASE WHEN direction>0 THEN last_played END) last_played,max(CASE WHEN direction<0 THEN last_played END) removed_last FROM ({input}) facts WHERE indexed=1 AND album_key IS NOT NULL GROUP BY album_key
        HAVING sum(direction)<>0 OR sum(direction*duration_millis)<>0 OR sum(direction*downloaded_count)<>0 OR sum(direction*play_count)<>0 OR max(CASE WHEN direction>0 THEN last_played END) IS NOT max(CASE WHEN direction<0 THEN last_played END)) delta WHERE albums.album_key=delta.album_key;
        UPDATE sources SET track_count=track_count+delta.count FROM (SELECT source_key,sum(direction) count FROM ({input}) facts WHERE indexed=1 AND source_key IS NOT NULL GROUP BY source_key HAVING sum(direction)<>0) delta WHERE sources.source_key=delta.source_key; ")
}

fn totals_change(input: &str) -> String {
    format!("INSERT INTO collection_totals(kind,entity_key,role,folder_key,album_count,track_count,duration_millis,downloaded_count,play_count,last_played)
        SELECT kind,entity_key,role,folder_key,sum(direction*album_count),sum(direction*track_count),sum(direction*duration_millis),sum(direction*downloaded_count),sum(direction*play_count),max(CASE WHEN direction>0 THEN last_played END)
        FROM ({input}) changed WHERE folder_key IS NULL OR EXISTS(SELECT 1 FROM folders WHERE folders.folder_key=changed.folder_key) GROUP BY kind,entity_key,role,folder_key
        ON CONFLICT DO UPDATE SET
            album_count=collection_totals.album_count+excluded.album_count,
            track_count=collection_totals.track_count+excluded.track_count,
            duration_millis=collection_totals.duration_millis+excluded.duration_millis,
            downloaded_count=collection_totals.downloaded_count+excluded.downloaded_count,
            play_count=collection_totals.play_count+excluded.play_count,
            last_played=CASE WHEN EXISTS(SELECT 1 FROM ({input}) removed WHERE removed.direction<0 AND removed.kind=excluded.kind AND removed.entity_key=excluded.entity_key AND removed.role=excluded.role AND removed.folder_key IS excluded.folder_key AND removed.last_played=collection_totals.last_played)
                AND (excluded.last_played IS NULL OR excluded.last_played<collection_totals.last_played)
                THEN (SELECT last_played FROM collection_album_index WHERE kind=excluded.kind AND entity_key=excluded.entity_key AND role=excluded.role AND folder_key IS excluded.folder_key ORDER BY last_played DESC LIMIT 1)
                WHEN excluded.last_played IS NULL THEN collection_totals.last_played
                WHEN collection_totals.last_played IS NULL THEN excluded.last_played
                ELSE max(collection_totals.last_played,excluded.last_played) END;")
}

fn refresh_group(kind: &str, role: i32, entity: &str, album: &str) -> String {
    let selected = predicate(kind, role, entity, "track");
    let native = native_album(kind, role, entity, album, "NULL");
    let rows = format!(
        "SELECT '{kind}' kind,{entity} entity_key,{role} role,track.track_key,track.source_key,track.album_key,track.indexed,track.duration_millis,track.is_downloaded downloaded_count,track.displayed_play_count play_count,track.displayed_last_played last_played,1 direction,NULL occurrence FROM tracks track WHERE track.album_key={album} AND track.indexed=1 AND {selected}"
    );
    let scope = scoped(&rows);
    let grouped = format!(
        "SELECT kind,entity_key,role,album_key,folder_key,sum(direction) track_count,sum(direction*duration_millis) duration_millis,sum(direction*downloaded_count) downloaded_count,sum(direction*play_count) play_count,max(last_played) last_played,album_key IS NOT NULL AND (sum(direction)>0 OR ({native}) AND folder_key IS NULL) album_count FROM ({scope} UNION ALL SELECT '{kind}',{entity},{role},NULL,NULL,{album},1,0,0,0,NULL,0,NULL,NULL) current GROUP BY folder_key"
    );
    let keys = format!(
        "SELECT folder_key FROM actual UNION SELECT folder_key FROM collection_album_index WHERE kind='{kind}' AND entity_key={entity} AND role={role} AND coalesce(album_key,'')=coalesce({album},'')"
    );
    let updates = METRICS
        .iter()
        .map(|field| format!("{field}=excluded.{field}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("INSERT INTO collection_album_index(kind,entity_key,role,album_key,folder_key,track_count,duration_millis,downloaded_count,play_count,last_played,album_count) SELECT * FROM (WITH actual AS MATERIALIZED({grouped}),keys AS({keys})
        SELECT '{kind}',{entity},{role},{album},keys.folder_key,coalesce(actual.track_count,0),coalesce(actual.duration_millis,0),coalesce(actual.downloaded_count,0),coalesce(actual.play_count,0),actual.last_played,coalesce(actual.album_count,0)
        FROM keys LEFT JOIN actual ON coalesce(actual.folder_key,'')=coalesce(keys.folder_key,'') WHERE EXISTS(SELECT 1 FROM albums WHERE album_key={album}) AND (keys.folder_key IS NULL OR EXISTS(SELECT 1 FROM folders WHERE folder_key=keys.folder_key)) AND EXISTS(SELECT 1 FROM {kind}s WHERE {kind}_key={entity})
        ) resolved WHERE true ON CONFLICT DO UPDATE SET {updates} WHERE (collection_album_index.track_count,collection_album_index.duration_millis,collection_album_index.downloaded_count,collection_album_index.play_count,collection_album_index.last_played,collection_album_index.album_count) IS NOT (excluded.track_count,excluded.duration_millis,excluded.downloaded_count,excluded.play_count,excluded.last_played,excluded.album_count);
        DELETE FROM collection_album_index WHERE kind='{kind}' AND entity_key={entity} AND role={role} AND coalesce(album_key,'')=coalesce({album},'') AND track_count=0 AND album_count=0;")
}

fn display(scope: &str) -> String {
    format!("UPDATE tracks SET displayed_play_count=current.play_count,displayed_last_played=current.last_played,displayed_skip_count=current.skip_count,is_downloaded=current.downloaded_count FROM (
        SELECT track.track_key,EXISTS(SELECT 1 FROM local_locators WHERE media_uri=track.media_uri AND origin='download') downloaded_count,
        CASE WHEN source.kind IN('local','files','webdav','smb','documents') OR track.source_key IS NULL THEN track.local_play_count+coalesce(legacy.play_count,0) ELSE coalesce(native.play_count,0) END play_count,
        CASE WHEN source.kind IN('local','files','webdav','smb','documents') OR track.source_key IS NULL THEN (SELECT max(value) FROM(SELECT max(started_at) value FROM listens WHERE media_uri=track.media_uri UNION ALL SELECT legacy.last_played_at)) ELSE native.last_played END last_played,
        CASE WHEN source.kind IN('local','files','webdav','smb','documents') OR track.source_key IS NULL THEN coalesce((SELECT sum(skipped) FROM listens WHERE media_uri=track.media_uri),0)+coalesce(legacy.skip_count,0) ELSE coalesce(native.skip_count,0) END skip_count
        FROM tracks track LEFT JOIN sources source USING(source_key) LEFT JOIN native_media_state native USING(media_uri) LEFT JOIN legacy_activity legacy ON legacy.source_id=source.object_id AND legacy.track_object_id=track.object_id AND legacy.period='lifetime' AND legacy.item_kind='track' WHERE {scope}
        ) current WHERE tracks.track_key=current.track_key AND (displayed_play_count,displayed_last_played,displayed_skip_count,is_downloaded) IS NOT (current.play_count,current.last_played,current.skip_count,current.downloaded_count);")
}

fn triggers() -> String {
    let mut sql = String::new();
    for (event, name, rows) in [
        ("AFTER INSERT", "insert", vec![("NEW", 1)]),
        ("AFTER DELETE", "delete", vec![("OLD", -1)]),
        (
            "AFTER UPDATE OF source_key,album_key,indexed,metadata,media_uri,object_id,local_play_count",
            "update",
            vec![("OLD", -1), ("NEW", 1)],
        ),
        (
            "AFTER UPDATE OF displayed_play_count,displayed_last_played,is_downloaded",
            "display",
            vec![("OLD", -1), ("NEW", 1)],
        ),
    ] {
        let input = rows
            .iter()
            .map(|(row, sign)| facts(row, *sign))
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        let mut body = canonical_delta(&input);
        for (kind, role) in OWNERS
            .iter()
            .map(|(kind, role, _)| (*kind, *role))
            .chain([("playlist", 0)])
        {
            body.push_str(&contribution_delta(
                &scoped(&members(&input, kind, role)),
                kind,
                role,
            ));
        }
        if name != "display" {
            body.push_str(&folder_children_delta(&folder_edges(&input)));
        }
        let when = match name {
            "update" => {
                "WHEN (OLD.source_key,OLD.album_key,OLD.indexed,OLD.duration_millis,OLD.media_uri,OLD.object_id,OLD.local_play_count) IS NOT (NEW.source_key,NEW.album_key,NEW.indexed,NEW.duration_millis,NEW.media_uri,NEW.object_id,NEW.local_play_count)"
            }
            "display" => {
                "WHEN (OLD.displayed_play_count,OLD.displayed_last_played,OLD.is_downloaded) IS NOT (NEW.displayed_play_count,NEW.displayed_last_played,NEW.is_downloaded)"
            }
            _ => "",
        };
        if name == "insert" || name == "update" {
            let moved = if name == "update" {
                "AND OLD.media_uri IS NOT NEW.media_uri"
            } else {
                ""
            };
            body.push_str(&format!("UPDATE tracks SET local_play_count=(SELECT count(*) FROM listens WHERE media_uri=tracks.media_uri) WHERE track_key=NEW.track_key {moved} AND local_play_count IS NOT (SELECT count(*) FROM listens WHERE media_uri=tracks.media_uri);"));
            body.push_str(&display("track.track_key=NEW.track_key"));
        }
        sql.push_str(&format!("CREATE TRIGGER IF NOT EXISTS catalog_index_track_{name} {event} ON tracks {when} BEGIN {body} END;"));
    }
    for (kind, role, edge) in OWNERS
        .iter()
        .copied()
        .chain([("playlist", 0, "playlist_entries")])
    {
        for (name, event, rows) in [
            ("insert", "AFTER INSERT", vec![("NEW", 1)]),
            ("delete", "AFTER DELETE", vec![("OLD", -1)]),
            ("update", "AFTER UPDATE", vec![("OLD", -1), ("NEW", 1)]),
        ] {
            let input=rows.iter().map(|(row,sign)|{
                let admitted=if kind=="playlist" {"1".to_owned()}else if kind=="artist"&&role==1 {"track.indexed=1 AND track.album_key IS NULL".into()}else if kind=="genre" {format!("track.indexed=1 AND NOT EXISTS(SELECT 1 FROM album_genres inherited WHERE inherited.album_key=track.album_key AND inherited.genre_key={row}.genre_key)")}else{"track.indexed=1".into()};
                format!("SELECT '{kind}' kind,{row}.{kind}_key entity_key,{role} role,track.track_key,track.source_key,track.album_key,track.indexed,track.duration_millis,track.is_downloaded downloaded_count,track.displayed_play_count play_count,track.displayed_last_played last_played,{sign} direction,NULL occurrence FROM tracks track WHERE track.track_key={row}.track_key AND {admitted}")
            }).collect::<Vec<_>>().join(" UNION ALL ");
            let affected = if kind == "folder" {
                format!("SELECT changed.*,NULL folder_key FROM ({input}) changed")
            } else {
                scoped(&input)
            };
            let mut body = contribution_delta(&affected, kind, role);
            if kind == "playlist" || kind == "genre" {
                let pairs = rows.iter().map(|(row,sign)| {
                    if kind == "playlist" {
                        format!("SELECT {row}.playlist_key playlist_key,genre_key,{sign} direction FROM track_genres WHERE track_key={row}.track_key")
                    } else {
                        format!("SELECT playlist_key,{row}.genre_key genre_key,{sign} direction FROM playlist_entries WHERE track_key={row}.track_key")
                    }
                }).collect::<Vec<_>>().join(" UNION ALL ");
                body.push_str(&playlist_genre_delta(&pairs));
            }
            if kind == "folder" {
                for (kind, role) in OWNERS
                    .iter()
                    .map(|(kind, role, _)| (*kind, *role))
                    .chain([("playlist", 0)])
                    .filter(|(kind, _)| *kind != "folder")
                {
                    let scope=rows.iter().map(|(row,sign)|{let input=format!("SELECT track_key,source_key,album_key,indexed,duration_millis,is_downloaded downloaded_count,displayed_play_count play_count,displayed_last_played last_played,{sign} direction FROM tracks WHERE track_key={row}.track_key");format!("SELECT member.*,{row}.folder_key folder_key FROM ({}) member",members(&input,kind,role))}).collect::<Vec<_>>().join(" UNION ALL ");
                    body.push_str(&contribution_delta(&scope, kind, role));
                }
                body.push_str(&folder_path_change(name));
            }
            let when = if name == "update" {
                if kind == "folder" {
                    "WHEN (OLD.track_key,OLD.folder_key,OLD.position) IS NOT (NEW.track_key,NEW.folder_key,NEW.position)".into()
                } else {
                    format!(
                        "WHEN (OLD.track_key,OLD.{kind}_key) IS NOT (NEW.track_key,NEW.{kind}_key)"
                    )
                }
            } else {
                String::new()
            };
            sql.push_str(&format!("CREATE TRIGGER IF NOT EXISTS catalog_index_{kind}_{role}_{name} {event} ON {edge} {when} BEGIN {body} END;"));
        }
    }
    for (kind, role) in [("artist", 1), ("genre", 0)] {
        for (name, event, rows) in [
            ("insert", "AFTER INSERT", vec!["NEW"]),
            ("delete", "AFTER DELETE", vec!["OLD"]),
            ("update", "AFTER UPDATE", vec!["OLD", "NEW"]),
        ] {
            let body = rows
                .iter()
                .map(|row| {
                    refresh_group(
                        kind,
                        role,
                        &format!("{row}.{kind}_key"),
                        &format!("{row}.album_key"),
                    )
                })
                .collect::<String>();
            sql.push_str(&format!("CREATE TRIGGER IF NOT EXISTS catalog_index_album_{kind}_{name} {event} ON album_{kind}s BEGIN {body} END;"));
        }
    }
    for (name, event, rows) in [
        ("insert", "AFTER INSERT", vec![("NEW", 1)]),
        ("delete", "AFTER DELETE", vec![("OLD", -1)]),
        ("update", "AFTER UPDATE", vec![("OLD", -1), ("NEW", 1)]),
    ] {
        let input=rows.iter().map(|(row,sign)|{
            let metrics=METRICS.iter().map(|field|format!("{row}.{field}")).collect::<Vec<_>>().join(",");
            format!("SELECT {row}.kind,{row}.entity_key,{row}.role,{row}.folder_key,{metrics},{sign} direction")
        }).collect::<Vec<_>>().join(" UNION ALL ");
        let guard = if name == "update" {
            "WHEN (OLD.kind,OLD.entity_key,OLD.role,OLD.folder_key,OLD.album_count,OLD.track_count,OLD.duration_millis,OLD.downloaded_count,OLD.play_count,OLD.last_played) IS NOT (NEW.kind,NEW.entity_key,NEW.role,NEW.folder_key,NEW.album_count,NEW.track_count,NEW.duration_millis,NEW.downloaded_count,NEW.play_count,NEW.last_played)"
        } else {
            ""
        };
        sql.push_str(&format!("CREATE TRIGGER IF NOT EXISTS catalog_index_totals_{name} {event} ON collection_album_index {guard} BEGIN {} END;",totals_change(&input)));
    }
    for (kind, role, _) in OWNERS
        .iter()
        .copied()
        .chain([("playlist", 0, "playlist_entries")])
    {
        sql.push_str(&format!("CREATE TRIGGER IF NOT EXISTS catalog_index_{kind}_{role}_remove AFTER DELETE ON {kind}s BEGIN DELETE FROM collection_album_index WHERE kind='{kind}' AND entity_key=OLD.{kind}_key AND role={role}; DELETE FROM collection_totals WHERE kind='{kind}' AND entity_key=OLD.{kind}_key AND role={role}; END;"));
    }
    sql.push_str("CREATE TRIGGER IF NOT EXISTS catalog_index_track_edges_remove BEFORE DELETE ON tracks BEGIN DELETE FROM track_artists WHERE track_key=OLD.track_key; DELETE FROM track_album_artists WHERE track_key=OLD.track_key; DELETE FROM track_genres WHERE track_key=OLD.track_key; DELETE FROM track_moods WHERE track_key=OLD.track_key; DELETE FROM track_folders WHERE track_key=OLD.track_key; END;");
    for (name, event, guard) in [
        ("insert", "AFTER INSERT", ""),
        (
            "members",
            "AFTER UPDATE OF track_count",
            "AND OLD.track_count IS NOT NEW.track_count",
        ),
    ] {
        sql.push_str(&format!("CREATE TRIGGER IF NOT EXISTS catalog_index_gallery_{name} {event} ON collection_album_index WHEN NEW.kind IN('genre','mood') AND NEW.folder_key IS NULL {guard} BEGIN {} END;",gallery("SELECT NEW.summary_key summary_key")));
    }
    sql.push_str(&format!("CREATE TRIGGER IF NOT EXISTS catalog_index_gallery_track_uri AFTER UPDATE OF media_uri ON tracks WHEN OLD.media_uri IS NOT NEW.media_uri BEGIN {} END;",gallery("SELECT summary_key FROM collection_album_index INDEXED BY collection_album_index_album WHERE album_key=NEW.album_key UNION SELECT summary_key FROM collection_album_index INDEXED BY collection_album_index_albumless WHERE albumless_track_key=NEW.track_key")));
    sql.push_str(&format!("CREATE TRIGGER IF NOT EXISTS catalog_index_gallery_album_uri AFTER UPDATE OF media_uri ON albums WHEN OLD.media_uri IS NOT NEW.media_uri BEGIN {} END;",gallery("SELECT summary_key FROM collection_album_index INDEXED BY collection_album_index_album WHERE album_key=NEW.album_key")));
    sql.push_str("CREATE TRIGGER IF NOT EXISTS catalog_index_source_album_insert AFTER INSERT ON albums WHEN NEW.indexed=1 OR NEW.track_count>0 BEGIN UPDATE sources SET album_count=album_count+1 WHERE source_key=NEW.source_key; END; CREATE TRIGGER IF NOT EXISTS catalog_index_source_album_delete BEFORE DELETE ON albums WHEN OLD.indexed=1 OR OLD.track_count>0 BEGIN UPDATE sources SET album_count=album_count-1 WHERE source_key=OLD.source_key; END; CREATE TRIGGER IF NOT EXISTS catalog_index_source_album_update AFTER UPDATE OF source_key,indexed,track_count ON albums WHEN (OLD.source_key,OLD.indexed=1 OR OLD.track_count>0) IS NOT (NEW.source_key,NEW.indexed=1 OR NEW.track_count>0) BEGIN UPDATE sources SET album_count=album_count+changes.count FROM (SELECT source_key,sum(count) count FROM (SELECT OLD.source_key source_key,-1 count WHERE OLD.indexed=1 OR OLD.track_count>0 UNION ALL SELECT NEW.source_key,1 WHERE NEW.indexed=1 OR NEW.track_count>0) GROUP BY source_key HAVING sum(count)<>0) changes WHERE sources.source_key=changes.source_key; END;");
    sql.push_str(&activity_triggers());
    sql
}

fn activity_triggers() -> String {
    let mut sql = String::new();
    for (name, event, scope) in [
        (
            "native_insert",
            "AFTER INSERT ON native_media_state",
            "track.media_uri=NEW.media_uri",
        ),
        (
            "native_update",
            "AFTER UPDATE OF media_uri,play_count,skip_count,last_played ON native_media_state WHEN (OLD.media_uri,OLD.play_count,OLD.skip_count,OLD.last_played) IS NOT (NEW.media_uri,NEW.play_count,NEW.skip_count,NEW.last_played)",
            "track.media_uri IN(OLD.media_uri,NEW.media_uri)",
        ),
        (
            "native_delete",
            "AFTER DELETE ON native_media_state",
            "track.media_uri=OLD.media_uri",
        ),
        (
            "legacy_insert",
            "AFTER INSERT ON legacy_activity WHEN NEW.period='lifetime' AND NEW.item_kind='track'",
            "track.source_key=(SELECT source_key FROM sources WHERE object_id=NEW.source_id) AND track.object_id=NEW.track_object_id",
        ),
        (
            "legacy_update",
            "AFTER UPDATE ON legacy_activity WHEN (OLD.source_id,OLD.period,OLD.item_kind,OLD.track_object_id,OLD.play_count,OLD.skip_count,OLD.last_played_at) IS NOT (NEW.source_id,NEW.period,NEW.item_kind,NEW.track_object_id,NEW.play_count,NEW.skip_count,NEW.last_played_at)",
            "(OLD.period='lifetime' AND OLD.item_kind='track' AND track.source_key=(SELECT source_key FROM sources WHERE object_id=OLD.source_id) AND track.object_id=OLD.track_object_id) OR (NEW.period='lifetime' AND NEW.item_kind='track' AND track.source_key=(SELECT source_key FROM sources WHERE object_id=NEW.source_id) AND track.object_id=NEW.track_object_id)",
        ),
        (
            "legacy_delete",
            "AFTER DELETE ON legacy_activity WHEN OLD.period='lifetime' AND OLD.item_kind='track'",
            "track.source_key=(SELECT source_key FROM sources WHERE object_id=OLD.source_id) AND track.object_id=OLD.track_object_id",
        ),
        (
            "download_insert",
            "AFTER INSERT ON local_locators WHEN NEW.origin='download'",
            "track.media_uri=NEW.media_uri",
        ),
        (
            "download_update",
            "AFTER UPDATE OF media_uri,origin ON local_locators WHEN (OLD.media_uri,OLD.origin) IS NOT (NEW.media_uri,NEW.origin) AND (OLD.origin='download' OR NEW.origin='download')",
            "track.media_uri IN(OLD.media_uri,NEW.media_uri)",
        ),
        (
            "download_delete",
            "AFTER DELETE ON local_locators WHEN OLD.origin='download'",
            "track.media_uri=OLD.media_uri",
        ),
        (
            "source_update",
            "AFTER UPDATE OF kind,object_id ON sources WHEN (OLD.kind,OLD.object_id) IS NOT (NEW.kind,NEW.object_id)",
            "track.source_key=NEW.source_key",
        ),
    ] {
        sql.push_str(&format!(
            "CREATE TRIGGER IF NOT EXISTS catalog_index_{name} {event} BEGIN {} END;",
            display(scope)
        ));
    }
    sql.push_str(&format!("CREATE TRIGGER IF NOT EXISTS catalog_index_listen_update AFTER UPDATE OF media_uri,started_at,skipped ON listens WHEN (OLD.media_uri,OLD.started_at,OLD.skipped) IS NOT (NEW.media_uri,NEW.started_at,NEW.skipped) BEGIN UPDATE tracks SET local_play_count=local_play_count-1 WHERE media_uri=OLD.media_uri AND OLD.media_uri IS NOT NEW.media_uri; UPDATE tracks SET local_play_count=local_play_count+1 WHERE media_uri=NEW.media_uri AND OLD.media_uri IS NOT NEW.media_uri; {} END;",display("track.media_uri IN(OLD.media_uri,NEW.media_uri)")));
    let mut markers = String::new();
    for (kind, role) in [("artist", 1), ("genre", 0)] {
        markers.push_str(&format!("INSERT INTO collection_album_index(kind,entity_key,role,album_key,folder_key,album_count,track_count,duration_millis,downloaded_count,play_count) SELECT '{kind}',{kind}_key,{role},NEW.album_key,NULL,NEW.indexed=1,0,0,0,0 FROM album_{kind}s WHERE album_key=NEW.album_key ON CONFLICT DO UPDATE SET album_count=collection_album_index.track_count>0 OR NEW.indexed=1;"));
    }
    markers.push_str("DELETE FROM collection_album_index WHERE album_key=NEW.album_key AND track_count=0 AND album_count=0;");
    sql.push_str(&format!("CREATE TRIGGER IF NOT EXISTS catalog_index_album_admission AFTER UPDATE OF indexed ON albums WHEN OLD.indexed IS NOT NEW.indexed BEGIN {markers} END;"));
    sql
}

const CREATE: &str = r#"
CREATE TABLE IF NOT EXISTS playlist_genres (
    playlist_key INTEGER NOT NULL REFERENCES playlists ON DELETE CASCADE,
    genre_key INTEGER NOT NULL REFERENCES genres ON DELETE CASCADE,
    track_count INTEGER NOT NULL,
    PRIMARY KEY(playlist_key,genre_key)
) STRICT;
CREATE INDEX IF NOT EXISTS playlist_genres_rank ON playlist_genres(playlist_key,track_count DESC,genre_key);
CREATE INDEX IF NOT EXISTS playlist_genres_genre ON playlist_genres(genre_key,playlist_key);
CREATE TABLE IF NOT EXISTS folder_children (
    folder_child_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    parent_folder_key INTEGER REFERENCES folders ON DELETE CASCADE,
    child_folder_key INTEGER NOT NULL REFERENCES folders ON DELETE CASCADE,
    admitted_track_count INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS folder_children_parent ON folder_children(source_key,coalesce(parent_folder_key,''),child_folder_key);
CREATE TABLE IF NOT EXISTS collection_album_index (
    summary_key INTEGER PRIMARY KEY,kind TEXT NOT NULL,entity_key INTEGER NOT NULL,role INTEGER NOT NULL,
    album_key INTEGER REFERENCES albums ON DELETE CASCADE,albumless_track_key INTEGER REFERENCES tracks ON DELETE CASCADE,folder_key INTEGER REFERENCES folders ON DELETE CASCADE,
    artwork_group_uri TEXT,artwork_member_uri TEXT,artwork_track_key INTEGER REFERENCES tracks ON DELETE SET NULL,
    album_count INTEGER NOT NULL,track_count INTEGER NOT NULL,duration_millis INTEGER NOT NULL,
    downloaded_count INTEGER NOT NULL,play_count INTEGER NOT NULL,last_played INTEGER
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS collection_album_index_scope ON collection_album_index(kind,entity_key,role,coalesce(album_key,''),coalesce(folder_key,''),coalesce(albumless_track_key,''));
CREATE INDEX IF NOT EXISTS collection_album_index_album ON collection_album_index(album_key,kind,role,entity_key,folder_key);
CREATE INDEX IF NOT EXISTS collection_album_index_albumless ON collection_album_index(albumless_track_key) WHERE albumless_track_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS collection_album_index_activity ON collection_album_index(kind,entity_key,role,folder_key,last_played DESC);
CREATE INDEX IF NOT EXISTS collection_album_index_gallery ON collection_album_index(kind,entity_key,role,artwork_group_uri,artwork_member_uri) WHERE folder_key IS NULL AND artwork_track_key IS NOT NULL;
CREATE TABLE IF NOT EXISTS collection_totals (
    totals_key INTEGER PRIMARY KEY,kind TEXT NOT NULL,entity_key INTEGER NOT NULL,role INTEGER NOT NULL,
    folder_key INTEGER REFERENCES folders ON DELETE CASCADE,
    album_count INTEGER NOT NULL,track_count INTEGER NOT NULL,duration_millis INTEGER NOT NULL,
    downloaded_count INTEGER NOT NULL,play_count INTEGER NOT NULL,last_played INTEGER
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS collection_totals_scope ON collection_totals(kind,entity_key,role,coalesce(folder_key,''));
CREATE INDEX IF NOT EXISTS collection_totals_folder ON collection_totals(kind,role,coalesce(folder_key,''),entity_key);
CREATE INDEX IF NOT EXISTS tracks_album_activity ON tracks(album_key,indexed,displayed_last_played DESC);
CREATE INDEX IF NOT EXISTS tracks_album_artwork ON tracks(album_key,indexed,media_uri,track_key);
CREATE INDEX IF NOT EXISTS tracks_album_metadata_order_idx ON tracks(album_key,coalesce(cue_path,source_path),disc_number,track_number,object_id) WHERE indexed=1;
CREATE INDEX IF NOT EXISTS tracks_album_title_window ON tracks(source_key,indexed,album_title_sort,(album_key IS NULL),album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_album_title_desc_window ON tracks(source_key,indexed,album_title_sort DESC,(album_key IS NULL),album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_album_artist_window ON tracks(source_key,indexed,(album_artist_sort IS NULL),album_artist_sort,(album_year_sort IS NULL),album_year_sort,album_title_sort,(album_key IS NULL),album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_album_artist_desc_window ON tracks(source_key,indexed,(album_artist_sort IS NULL),album_artist_sort DESC,(album_year_sort IS NULL),album_year_sort,album_title_sort,(album_key IS NULL),album_key,disc_number,track_number,sort_text,track_key);
"#;

const PLAYLIST_GENRES: &str = "INSERT INTO playlist_genres(playlist_key,genre_key,track_count)
    SELECT entry.playlist_key,credit.genre_key,count(*) FROM playlist_entries entry JOIN track_genres credit USING(track_key) GROUP BY entry.playlist_key,credit.genre_key;";

pub(crate) async fn upgrade(connection: &mut SqliteConnection) -> LibraryResult<()> {
    for field in &METRICS[1..] {
        let declaration = if *field == "last_played" {
            "INTEGER"
        } else {
            "INTEGER NOT NULL DEFAULT 0"
        };
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE albums ADD COLUMN {field} {declaration}"
        )))
        .execute(&mut *connection)
        .await?;
    }
    sqlx::raw_sql("ALTER TABLE sources ADD COLUMN album_count INTEGER NOT NULL DEFAULT 0; ALTER TABLE sources ADD COLUMN track_count INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE tracks ADD COLUMN displayed_play_count INTEGER NOT NULL DEFAULT 0; ALTER TABLE tracks ADD COLUMN displayed_last_played INTEGER; ALTER TABLE tracks ADD COLUMN displayed_skip_count INTEGER NOT NULL DEFAULT 0; ALTER TABLE tracks ADD COLUMN is_downloaded INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE tracks ADD COLUMN album_title_sort TEXT; ALTER TABLE tracks ADD COLUMN album_artist_sort TEXT; ALTER TABLE tracks ADD COLUMN album_year_sort INTEGER;
        ALTER TABLE albums ADD COLUMN input_metadata BLOB; UPDATE albums SET input_metadata=metadata;
        ALTER TABLE albums ADD COLUMN title_sort TEXT NOT NULL DEFAULT ''; ALTER TABLE albums ADD COLUMN title_search TEXT NOT NULL DEFAULT ''; ALTER TABLE albums ADD COLUMN artist_search TEXT NOT NULL DEFAULT ''; ALTER TABLE albums ADD COLUMN album_artist_sort TEXT; ALTER TABLE albums ADD COLUMN year_sort INTEGER; ALTER TABLE albums ADD COLUMN release_sort TEXT; ALTER TABLE albums ADD COLUMN added_sort INTEGER; ALTER TABLE albums ADD COLUMN release_class INTEGER NOT NULL DEFAULT 0; ALTER TABLE albums ADD COLUMN release_identity TEXT;
        DROP VIEW IF EXISTS effective_album_artists; DROP VIEW IF EXISTS effective_album_genres; DROP VIEW IF EXISTS effective_album_release_types; DROP TABLE IF EXISTS album_order; DROP TABLE album_release_types;")
        .execute(connection).await?;
    Ok(())
}

pub(crate) async fn unite_totals(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let old_triggers: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type='trigger' AND name GLOB 'catalog_index_*'",
    )
    .fetch_all(&mut *connection)
    .await?;
    for name in old_triggers {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP TRIGGER \"{}\"",
            name.replace('"', "\"\"")
        )))
        .execute(&mut *connection)
        .await?;
    }
    sqlx::raw_sql(CREATE).execute(&mut *connection).await?;
    for (kind, role, _) in OWNERS {
        let prefix = if kind == "artist" && role == 1 {
            "album_artist_"
        } else {
            ""
        };
        let fields = METRICS.map(|field| format!("{prefix}{field}"));
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO collection_totals(kind,entity_key,role,folder_key,album_count,track_count,duration_millis,downloaded_count,play_count,last_played)
             SELECT '{kind}',{kind}_key,{role},NULL,{} FROM {kind}s WHERE ({}) IS NOT (0,0,0,0,0,NULL)", fields.join(","), fields.join(",")
        )))
        .execute(&mut *connection).await?;
        for field in fields {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE {kind}s DROP COLUMN {field}"
            )))
            .execute(&mut *connection)
            .await?;
        }
    }
    sqlx::raw_sql("INSERT INTO collection_totals(kind,entity_key,role,folder_key,album_count,track_count,duration_millis,downloaded_count,play_count,last_played)
        SELECT 'playlist',playlist_key,0,NULL,coalesce(extra.album_count,0),playlist.track_count,playlist.duration_millis,playlist.downloaded_count,coalesce(extra.play_count,0),extra.last_played FROM playlists playlist
        LEFT JOIN (SELECT entity_key,sum(album_count) album_count,sum(play_count) play_count,max(last_played) last_played FROM collection_album_index WHERE kind='playlist' AND role=0 AND folder_key IS NULL GROUP BY entity_key) extra ON extra.entity_key=playlist.playlist_key
        WHERE (coalesce(extra.album_count,0),playlist.track_count,playlist.duration_millis,playlist.downloaded_count,coalesce(extra.play_count,0),extra.last_played) IS NOT (0,0,0,0,0,NULL);
        ALTER TABLE playlists DROP COLUMN track_count;
        ALTER TABLE playlists DROP COLUMN duration_millis;
        ALTER TABLE playlists DROP COLUMN downloaded_count;
        INSERT INTO collection_totals(kind,entity_key,role,folder_key,album_count,track_count,duration_millis,downloaded_count,play_count,last_played)
            SELECT * FROM collection_scope_totals;
        DROP TABLE collection_scope_totals;")
        .execute(&mut *connection).await?;
    sqlx::raw_sql(PLAYLIST_GENRES)
        .execute(&mut *connection)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(triggers()))
        .execute(connection)
        .await?;
    Ok(())
}

pub(crate) async fn initialize(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(CREATE).execute(&mut *connection).await?;
    sqlx::raw_sql("DROP TRIGGER IF EXISTS local_play_count_track_insert; DROP TRIGGER IF EXISTS local_play_count_track_uri;").execute(&mut *connection).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(display("true")))
        .execute(&mut *connection)
        .await?;
    let mut after = None;
    loop {
        let keys:Vec<i64>=sqlx::query_scalar("SELECT album_key FROM albums WHERE ?1 IS NULL OR album_key>?1 ORDER BY album_key LIMIT 128").bind(after).fetch_all(&mut *connection).await?;
        let Some(last) = keys.last() else {
            break;
        };
        after = Some(*last);
        albums(connection, &keys).await?;
    }
    sqlx::raw_sql("UPDATE albums SET (track_count,duration_millis,downloaded_count,play_count,last_played)=(SELECT count(*),coalesce(sum(duration_millis),0),coalesce(sum(is_downloaded),0),coalesce(sum(displayed_play_count),0),max(displayed_last_played) FROM tracks WHERE tracks.album_key=albums.album_key AND indexed=1);
        UPDATE sources SET album_count=(SELECT count(*) FROM albums WHERE albums.source_key=sources.source_key AND (indexed=1 OR track_count>0)),track_count=(SELECT count(*) FROM tracks WHERE tracks.source_key=sources.source_key AND indexed=1);")
        .execute(&mut *connection).await?;
    let input = "SELECT track_key,source_key,album_key,indexed,duration_millis,is_downloaded downloaded_count,displayed_play_count play_count,displayed_last_played last_played,1 direction FROM tracks";
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("INSERT INTO folder_children(source_key,parent_folder_key,child_folder_key,admitted_track_count) SELECT source_key,parent_folder_key,child_folder_key,count(*) FROM ({}) edge GROUP BY source_key,parent_folder_key,child_folder_key;",folder_edges(input))))
        .execute(&mut *connection).await?;
    let member_rows = OWNERS
        .iter()
        .map(|(kind, role, _)| (*kind, *role))
        .chain([("playlist", 0)])
        .map(|(kind, role)| members(input, kind, role))
        .collect::<Vec<_>>()
        .join(" UNION ");
    let rows = scoped(&member_rows);
    let rows = format!(
        "{rows} UNION ALL SELECT 'artist',credit.artist_key,1,NULL,NULL,credit.album_key,1,0,0,0,NULL,0,NULL,NULL FROM album_artists credit JOIN albums album USING(album_key) WHERE album.indexed=1 UNION ALL SELECT 'genre',credit.genre_key,0,NULL,NULL,credit.album_key,1,0,0,0,NULL,0,NULL,NULL FROM album_genres credit JOIN albums album USING(album_key) WHERE album.indexed=1"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("INSERT INTO collection_album_index(kind,entity_key,role,album_key,albumless_track_key,folder_key,album_count,track_count,duration_millis,downloaded_count,play_count,last_played)
        SELECT kind,entity_key,role,album_key,CASE WHEN album_key IS NULL THEN track_key END,folder_key,album_key IS NOT NULL,sum(direction),sum(direction*duration_millis),sum(direction*downloaded_count),sum(direction*play_count),max(last_played) FROM ({rows}) current GROUP BY kind,entity_key,role,album_key,CASE WHEN album_key IS NULL THEN track_key END,folder_key;")))
        .execute(&mut *connection).await?;
    sqlx::raw_sql("INSERT INTO collection_totals(kind,entity_key,role,folder_key,album_count,track_count,duration_millis,downloaded_count,play_count,last_played)
        SELECT kind,entity_key,role,folder_key,sum(album_count),sum(track_count),sum(duration_millis),sum(downloaded_count),sum(play_count),max(last_played)
        FROM collection_album_index GROUP BY kind,entity_key,role,folder_key;")
        .execute(&mut *connection).await?;
    sqlx::raw_sql(PLAYLIST_GENRES)
        .execute(&mut *connection)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(gallery(
        "SELECT summary_key FROM collection_album_index",
    )))
    .execute(&mut *connection)
    .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(triggers()))
        .execute(connection)
        .await?;
    Ok(())
}
