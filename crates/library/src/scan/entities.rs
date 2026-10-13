use std::collections::HashMap;

use super::{
    AlbumInput, ArtistInput, FolderInput, GenreInput, MoodInput, RecordInput, Scan, ScanLink,
    TrackInput,
};
use crate::{
    LibraryError, LibraryResult, SourceId, cue_media_uri, normalize_direct_media_uri,
    source_entity_uri,
};

impl Scan {
    pub async fn write_tracks(
        &mut self,
        records: &[(&TrackInput, Option<&AlbumInput>)],
    ) -> LibraryResult<usize> {
        let mut written = 0;
        for page in records.chunks(128) {
            let owns_batch = self.batch_writer.is_none();
            if owns_batch {
                self.begin_batch().await?;
            }
            let result = self.write_track_page(page).await;
            if result.is_err() {
                self.failed = true;
            }
            if owns_batch {
                self.finish_batch().await?;
            }
            written += result?;
        }
        Ok(written)
    }

    async fn write_track_page(
        &mut self,
        page: &[(&TrackInput, Option<&AlbumInput>)],
    ) -> LibraryResult<usize> {
        let mut first = self
            .observe_tracks(
                &page
                    .iter()
                    .map(|(track, _)| track.object_id.as_str())
                    .collect::<Vec<_>>(),
            )
            .await?;
        let mut tracks = Vec::new();
        let mut albums = Vec::new();
        let mut artists = Vec::new();
        let mut genres = Vec::new();
        let mut moods = Vec::new();
        let mut folders = HashMap::new();
        let mut album_artist_links = Vec::new();
        let mut album_genre_links = Vec::new();
        let mut release_type_links = Vec::new();
        let mut track_artist_links = Vec::new();
        let mut track_genre_links = Vec::new();
        let mut track_mood_links = Vec::new();
        for &(track, album) in page {
            let track_id = track.object_id.as_str();
            if let Some(album) = album {
                albums.push(album);
            }
            let (media_uri, normalized_search, audio_properties) = self.track_context(track)?;
            if let Some(existed) = first.remove(track_id) {
                tracks.push((
                    track,
                    media_uri,
                    normalized_search,
                    audio_properties,
                    existed,
                ));
            }
            let album_relations = album.and_then(|album| album.relations.as_ref());
            let track_relations = track.relations.as_ref();
            let album_credits = album_relations
                .map(|relations| {
                    if relations.album_artists.is_empty() {
                        relations.artists.as_slice()
                    } else {
                        relations.album_artists.as_slice()
                    }
                })
                .unwrap_or_default();
            let track_credits = track_relations
                .map(|relations| relations.artists.as_slice())
                .unwrap_or_default();
            let track_album_credits = track_relations
                .map(|relations| relations.album_artists.as_slice())
                .unwrap_or_default();
            for credits in [album_credits, track_credits, track_album_credits] {
                artists.extend(credits);
            }
            let album_genres = album_relations
                .map(|relations| relations.genres.as_slice())
                .unwrap_or_default();
            let track_genres = track_relations
                .map(|relations| relations.genres.as_slice())
                .unwrap_or_default();
            for entries in [album_genres, track_genres] {
                genres.extend(entries);
            }
            if let Some(album) = album {
                let album_id = album.object_id.as_str();
                album_artist_links.extend(
                    album_credits
                        .iter()
                        .map(|artist| (album_id, artist.object_id.as_str())),
                );
                if let Some(relations) = album_relations {
                    album_genre_links.extend(
                        relations
                            .genres
                            .iter()
                            .map(|genre| (album_id, genre.object_id.as_str())),
                    );
                    release_type_links.extend(
                        relations
                            .release_types
                            .iter()
                            .map(|kind| (album_id, kind.as_str())),
                    );
                }
            }
            if let Some(relations) = track_relations {
                moods.extend(&relations.moods);
                track_artist_links.extend(
                    relations
                        .artists
                        .iter()
                        .map(|artist| (track_id, artist.object_id.as_str())),
                );
                track_genre_links.extend(
                    relations
                        .genres
                        .iter()
                        .map(|genre| (track_id, genre.object_id.as_str())),
                );
                track_mood_links.extend(
                    relations
                        .moods
                        .iter()
                        .map(|mood| (track_id, mood.object_id.as_str())),
                );
            }
            if let Some(entries) = &track.folders {
                folders.insert(track_id, entries);
            }
        }
        self.write_album_rows(&albums).await?;
        self.write_artists(&artists).await?;
        self.write_genres(&genres).await?;
        self.write_moods(&moods).await?;
        if !tracks.is_empty() {
            let source = self.source;
            let facts = || {
                let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                    "WITH input(source,object_id,media_uri,album_id,favorite,rating,play_count,skip_count,last_played,activity) AS (",
                );
                query.push_values(&tracks, |mut row, (track, uri, _, _, _)| {
                    row.push_bind(source)
                        .push_bind(&track.object_id)
                        .push_bind(uri)
                        .push_bind(&track.album_object_id)
                        .push_bind(track.favorite)
                        .push_bind(
                            track
                                .rating
                                .filter(|rating| (0..=10).contains(rating))
                                .map(|rating| rating * 10),
                        )
                        .push_bind(track.source_play_count)
                        .push_bind(track.source_skip_count)
                        .push_bind(track.source_last_played)
                        .push_bind(
                            track.source_play_count.is_some()
                                || track.source_skip_count.is_some()
                                || track.source_last_played.is_some(),
                        );
                });
                query.push(") ");
                query
            };
            let mut detach = facts();
            detach.push("UPDATE tracks SET source_key=NULL,object_id=NULL,indexed=0 FROM input
                WHERE tracks.source_key=input.source AND tracks.object_id=input.object_id AND tracks.media_uri<>input.media_uri");
            self.changes.metadata |= self.execute_result(detach.build()).await?.rows_affected() > 0;
            let mut metadata =
                TrackInput::write_rows(tracks.iter().map(|(track, uri, search, audio, _)| {
                    (
                        *track,
                        self.source,
                        uri.as_str(),
                        search.as_str(),
                        audio.as_str(),
                    )
                }));
            metadata.push(" RETURNING object_id");
            let changed: Vec<String> = metadata
                .build_query_scalar()
                .fetch_all(
                    self.batch_writer
                        .as_mut()
                        .unwrap()
                        .as_mut()
                        .ok_or(LibraryError::WriterUnavailable)?,
                )
                .await?;
            self.changes.metadata |= !changed.is_empty();
            self.changes.tracks_added |= tracks
                .iter()
                .any(|(track, _, _, _, existed)| !existed && changed.contains(&track.object_id));
            let mut state = facts();
            state.push("UPDATE tracks SET source_favorite=COALESCE(input.favorite,source_favorite),source_rating=input.rating,
                source_play_count=CASE WHEN input.activity THEN input.play_count ELSE source_play_count END,
                source_skip_count=CASE WHEN input.activity THEN input.skip_count ELSE source_skip_count END,
                source_last_played=CASE WHEN input.activity THEN input.last_played ELSE source_last_played END
                FROM input WHERE tracks.source_key=input.source AND tracks.object_id=input.object_id
                  AND ((input.favorite IS NOT NULL AND source_favorite IS NOT input.favorite)
                    OR source_rating IS NOT input.rating
                    OR (input.activity AND (source_play_count,source_skip_count,source_last_played)
                        IS NOT (input.play_count,input.skip_count,input.last_played)))");
            self.changes.user_data |= self.execute_result(state.build()).await?.rows_affected() > 0;
            let mut pending = facts();
            pending.push("INSERT INTO temp.scan_track_albums(track_id,album_id)
                SELECT object_id,album_id FROM input WHERE album_id IS NOT NULL
                  AND NOT EXISTS(SELECT 1 FROM albums WHERE source_key=input.source AND object_id=input.album_id)
                ON CONFLICT(track_id) DO UPDATE SET album_id=excluded.album_id");
            self.execute(pending.build()).await?;
            self.remember_artworks(
                "track",
                &tracks
                    .iter()
                    .map(|(track, _, _, _, _)| {
                        (
                            track.object_id.as_str(),
                            track.artwork_binding.as_deref(),
                            true,
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .await?;
        }
        self.replace_track_folders(
            &folders.keys().copied().collect::<Vec<_>>(),
            &folders
                .iter()
                .flat_map(|(id, entries)| entries.iter().map(|folder| ScanLink::new(id, folder, 0)))
                .collect::<Vec<_>>(),
        )
        .await?;
        self.write_source_loudness(&page.iter().map(|(track, _)| *track).collect::<Vec<_>>())
            .await?;
        self.write_album_relations(&album_artist_links, &album_genre_links, &release_type_links)
            .await?;
        self.write_track_relations(&track_artist_links, &track_genre_links, &track_mood_links)
            .await?;
        Ok(tracks.len())
    }

    pub async fn write_records(&mut self, inputs: &[RecordInput]) -> LibraryResult<()> {
        for page in inputs.chunks(128) {
            let owns_batch = self.batch_writer.is_none();
            if owns_batch {
                self.begin_batch().await?;
            }
            let result = async {
                let mut folders = Vec::new();
                let mut albums = Vec::new();
                let mut tracks = Vec::new();
                let mut artists = Vec::new();
                let mut genres = Vec::new();
                let mut playlists = Vec::new();
                for input in page {
                    match input {
                        RecordInput::Folder(input) => folders.push(input),
                        RecordInput::Album(input) => albums.push(input),
                        RecordInput::Track(input) => tracks.push((input.as_ref(), None)),
                        RecordInput::Artist(input) => artists.push(input),
                        RecordInput::Genre(input) => genres.push(input),
                        RecordInput::Playlist(input) => playlists.push(input),
                    }
                }
                self.write_folders(&folders).await?;
                self.write_albums(&albums).await?;
                self.write_tracks(&tracks).await?;
                self.write_artists(&artists).await?;
                self.write_genres(&genres).await?;
                for playlist in playlists {
                    self.write_playlist(playlist).await?;
                }
                Ok::<_, LibraryError>(())
            }
            .await;
            if result.is_err() {
                self.failed = true;
            }
            if owns_batch {
                self.finish_batch().await?;
            }
            result?;
        }
        Ok(())
    }

    pub async fn write_albums(&mut self, inputs: &[&AlbumInput]) -> LibraryResult<()> {
        for page in inputs.chunks(128) {
            self.write_album_rows(page).await?;
            let mut artists = Vec::new();
            let mut genres = Vec::new();
            let mut artist_links = Vec::new();
            let mut genre_links = Vec::new();
            let mut release_types = Vec::new();
            for input in page {
                let id = input.object_id.as_str();
                if let Some(relations) = &input.relations {
                    let credits = if relations.album_artists.is_empty() {
                        &relations.artists
                    } else {
                        &relations.album_artists
                    };
                    artists.extend(credits);
                    genres.extend(&relations.genres);
                    artist_links
                        .extend(credits.iter().map(|artist| (id, artist.object_id.as_str())));
                    genre_links.extend(
                        relations
                            .genres
                            .iter()
                            .map(|genre| (id, genre.object_id.as_str())),
                    );
                    release_types.extend(
                        relations
                            .release_types
                            .iter()
                            .map(|kind| (id, kind.as_str())),
                    );
                }
            }
            self.write_artists(&artists).await?;
            self.write_genres(&genres).await?;
            self.write_album_relations(&artist_links, &genre_links, &release_types)
                .await?;
        }
        Ok(())
    }

    async fn write_album_rows(&mut self, inputs: &[&AlbumInput]) -> LibraryResult<()> {
        for page in inputs.chunks(128) {
            let mut rows: Vec<AlbumInput> = Vec::new();
            let mut positions = HashMap::new();
            for input in page {
                self.require_id("album", &input.object_id)?;
                self.require_row_bytes(&[
                    input.object_id.as_bytes(),
                    input.title.as_bytes(),
                    input.title.to_lowercase().as_bytes(),
                    input.display_artist.as_bytes(),
                    input.sort_text.as_bytes(),
                    input.release_date.as_deref().unwrap_or_default().as_bytes(),
                    input.date_added.as_deref().unwrap_or_default().as_bytes(),
                    input
                        .musicbrainz_release_id
                        .as_deref()
                        .unwrap_or_default()
                        .as_bytes(),
                    input
                        .musicbrainz_release_group_id
                        .as_deref()
                        .unwrap_or_default()
                        .as_bytes(),
                    input.artwork_binding.as_deref().unwrap_or_default(),
                ])?;
                let (position, supplied) = positions
                    .entry(input.object_id.as_str())
                    .or_insert((rows.len(), 0));
                *supplied |=
                    input.supplied(input.rating.filter(|rating| (0..=10).contains(rating)));
                let position = *position;
                if position == rows.len() {
                    rows.push((*input).clone());
                } else {
                    rows[position].merge(input);
                }
            }
            let first = self
                .observe_entities(
                    "album",
                    &rows
                        .iter()
                        .map(|row| row.object_id.as_str())
                        .collect::<Vec<_>>(),
                )
                .await?;
            let mut metadata = AlbumInput::write_rows(rows.iter().map(|input| {
                (
                    input,
                    self.source,
                    source_entity_uri(&SourceId::new(&self.source_id), "album", &input.object_id),
                    input.title.to_lowercase(),
                    first.contains(&input.object_id),
                )
            }));
            self.changes.metadata |=
                self.execute_result(metadata.build()).await?.rows_affected() > 0;
            let source = self.source;
            let facts = || {
                let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                    "WITH input(source,object_id,favorite,rating,supplied) AS (",
                );
                query.push_values(&rows, |mut row, input| {
                    let rating = input
                        .rating
                        .filter(|rating| (0..=10).contains(rating))
                        .map(|rating| rating * 10);
                    row.push_bind(source)
                        .push_bind(&input.object_id)
                        .push_bind(input.favorite)
                        .push_bind(rating)
                        .push_bind(positions[input.object_id.as_str()].1);
                });
                query.push(") ");
                query
            };
            let mut state = facts();
            state.push("UPDATE albums AS album SET source_favorite=COALESCE(input.favorite,album.source_favorite),
                source_rating=CASE WHEN seen.supplied & ").push_bind(AlbumInput::RATING)
                .push("=0 THEN COALESCE(input.rating,album.source_rating) ELSE album.source_rating END
                FROM input JOIN temp.scan_seen seen ON seen.kind='album' AND seen.object_id=input.object_id
                WHERE album.source_key=input.source AND album.object_id=input.object_id
                  AND ((input.favorite IS NOT NULL AND album.source_favorite IS NOT input.favorite)
                    OR (seen.supplied & ").push_bind(AlbumInput::RATING)
                .push("=0 AND input.rating IS NOT NULL AND album.source_rating IS NOT input.rating))");
            self.changes.user_data |= self.execute_result(state.build()).await?.rows_affected() > 0;
            let mut supplied = facts();
            supplied.push(
                "UPDATE temp.scan_seen SET supplied=scan_seen.supplied | input.supplied
                FROM input WHERE scan_seen.kind='album' AND scan_seen.object_id=input.object_id",
            );
            self.execute(supplied.build()).await?;
            self.remember_artworks(
                "album",
                &rows
                    .iter()
                    .map(|input| {
                        (
                            input.object_id.as_str(),
                            input.artwork_binding.as_deref(),
                            first.contains(&input.object_id),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .await?;
        }
        Ok(())
    }

    fn track_context(&mut self, input: &TrackInput) -> LibraryResult<(String, String, String)> {
        let object_id = input.object_id.as_str();
        let album_object_id = input.album_object_id.as_deref();
        let title = input.title.as_str();
        let display_album = input.display_album.as_str();
        let display_artist = input.display_artist.as_str();
        let comment = input.comment.as_deref();
        let normalized_search = format!(
            "{title} {display_artist} {display_album} {} {}",
            comment.unwrap_or_default(),
            input
                .year
                .map(|value| value.to_string())
                .unwrap_or_default()
        )
        .to_lowercase();
        let audio_properties = serde_json::to_string(&input.audio_properties)?;
        self.require_id("track", object_id)?;
        let direct_uri = input
            .media_uri
            .as_deref()
            .and_then(normalize_direct_media_uri);
        let media_uri = match (direct_uri, input.cue_start_millis, input.cue_end_millis) {
            (Some(file_uri), Some(start), Some(end)) => {
                cue_media_uri(object_id, &file_uri, start, end)
            }
            (Some(uri), None, None) => uri,
            _ => source_entity_uri(&SourceId::new(&self.source_id), "track", object_id),
        };
        if let Some(album_id) = album_object_id {
            self.require_id("album", album_id)?;
        }
        if input.source_play_count.is_some_and(|value| value < 0)
            || input.source_skip_count.is_some_and(|value| value < 0)
            || input.source_last_played.is_some_and(|value| value < 0)
        {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "provider activity counts cannot be negative".to_string(),
            ));
        }
        self.require_row_bytes(&[
            object_id.as_bytes(),
            album_object_id.unwrap_or_default().as_bytes(),
            title.as_bytes(),
            normalized_search.as_bytes(),
            display_album.as_bytes(),
            display_artist.as_bytes(),
            input.sort_text.as_bytes(),
            input.release_date.as_deref().unwrap_or_default().as_bytes(),
            input.date_added.as_deref().unwrap_or_default().as_bytes(),
            media_uri.as_bytes(),
            input.source_path.as_deref().unwrap_or_default().as_bytes(),
            input
                .source_format
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
            audio_properties.as_bytes(),
            comment.unwrap_or_default().as_bytes(),
            input
                .musicbrainz_recording_id
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
            input
                .musicbrainz_release_track_id
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
            input.cue_path.as_deref().unwrap_or_default().as_bytes(),
            input.artwork_binding.as_deref().unwrap_or_default(),
            input.loudness_analysis_key.as_slice(),
        ])?;
        Ok((media_uri, normalized_search, audio_properties))
    }

    async fn write_source_loudness(&mut self, inputs: &[&TrackInput]) -> LibraryResult<()> {
        for kind in ["track", "album"] {
            let mut facts = HashMap::new();
            for input in inputs {
                let (object_id, measurement) = if kind == "track" {
                    (Some(input.object_id.as_str()), input.source_loudness)
                } else {
                    (input.album_object_id.as_deref(), input.album_loudness)
                };
                let (Some(object_id), Some(measurement)) = (object_id, measurement) else {
                    continue;
                };
                if !crate::loudness::valid_measurement(measurement) {
                    self.failed = true;
                    return Err(LibraryError::InvalidScan(
                        "invalid source loudness fact".into(),
                    ));
                }
                facts.insert(object_id, measurement);
            }
            if facts.is_empty() {
                continue;
            }
            let rows = facts
                .into_iter()
                .map(|(id, fact)| {
                    (
                        id,
                        fact.integrated_lufs,
                        fact.true_peak,
                        fact.replay_gain_db,
                        fact.replay_gain_peak,
                        i64::from(fact.integrated_lufs.is_some()) * 256
                            | i64::from(fact.replay_gain_db.is_some()) * 512,
                    )
                })
                .collect::<Vec<_>>();
            let rows = serde_json::to_string(&rows)
                .map_err(|error| LibraryError::InvalidScan(error.to_string()))?;
            self.execute(sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO temp.scan_seen(kind,object_id,supplied)
                 SELECT ?2,owner.object_id,json_extract(fact.value,'$[5]')
                 FROM json_each(?3) fact CROSS JOIN {kind}s owner
                 WHERE owner.source_key=?1 AND owner.object_id=json_extract(fact.value,'$[0]')
                 ON CONFLICT(kind,object_id) DO UPDATE SET supplied=(supplied & ~768) | excluded.supplied")))
                .bind(self.source).bind(kind).bind(&rows)).await?;
            self.changes.metadata |= self.execute_result(sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO loudness_measurements(source_key,entity_kind,entity_key,analysis_key,integrated_lufs,true_peak,origin)
                 SELECT ?1,?2,owner.{kind}_key,owner.source_loudness_analysis_key,
                        json_extract(fact.value,'$[1]'),json_extract(fact.value,'$[2]'),'source'
                 FROM json_each(?3) fact CROSS JOIN {kind}s owner
                 WHERE owner.source_key=?1 AND owner.object_id=json_extract(fact.value,'$[0]')
                   AND json_extract(fact.value,'$[1]') IS NOT NULL
                 ON CONFLICT(source_key,entity_kind,entity_key,origin) DO UPDATE SET
                    analysis_key=excluded.analysis_key,integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak
                 WHERE (analysis_key,integrated_lufs,true_peak) IS NOT (excluded.analysis_key,excluded.integrated_lufs,excluded.true_peak)")))
                .bind(self.source).bind(kind).bind(&rows)).await?.rows_affected()>0;
            self.changes.metadata |= self.execute_result(sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO replay_gain_measurements(source_key,entity_kind,entity_key,analysis_key,gain_db,peak)
                 SELECT ?1,?2,owner.{kind}_key,owner.source_loudness_analysis_key,
                        json_extract(fact.value,'$[3]'),json_extract(fact.value,'$[4]')
                 FROM json_each(?3) fact CROSS JOIN {kind}s owner
                 WHERE owner.source_key=?1 AND owner.object_id=json_extract(fact.value,'$[0]')
                   AND json_extract(fact.value,'$[3]') IS NOT NULL
                 ON CONFLICT(source_key,entity_kind,entity_key) DO UPDATE SET
                    analysis_key=excluded.analysis_key,gain_db=excluded.gain_db,peak=excluded.peak
                 WHERE (analysis_key,gain_db,peak) IS NOT (excluded.analysis_key,excluded.gain_db,excluded.peak)")))
                .bind(self.source).bind(kind).bind(&rows)).await?.rows_affected()>0;
        }
        Ok(())
    }

    /// Full artist records supply their effective sort name, including the display-name fallback.
    /// Sparse credits leave it absent so they retain the existing artist sort name.
    pub async fn write_artists(&mut self, inputs: &[&ArtistInput]) -> LibraryResult<()> {
        for page in inputs.chunks(128) {
            let mut rows: Vec<ArtistInput> = Vec::new();
            let mut positions = HashMap::new();
            for input in page {
                self.require_id("artist", &input.object_id)?;
                self.require_row_bytes(&[
                    input.object_id.as_bytes(),
                    input.name.as_bytes(),
                    input.name.to_lowercase().as_bytes(),
                    input.sort_text.as_deref().unwrap_or_default().as_bytes(),
                    input
                        .musicbrainz_artist_id
                        .as_deref()
                        .unwrap_or_default()
                        .as_bytes(),
                    input.artwork_binding.as_deref().unwrap_or_default(),
                ])?;
                let position = *positions
                    .entry(input.object_id.as_str())
                    .or_insert(rows.len());
                if position == rows.len() {
                    rows.push((*input).clone());
                } else {
                    let row = &mut rows[position];
                    if input.sort_text.is_some()
                        || row.name.to_lowercase() != input.name.to_lowercase()
                    {
                        row.name.clone_from(&input.name);
                    }
                    if input.sort_text.is_some() {
                        row.sort_text.clone_from(&input.sort_text);
                        row.musicbrainz_artist_id
                            .clone_from(&input.musicbrainz_artist_id);
                        row.rating = input.rating;
                    } else {
                        if input.musicbrainz_artist_id.is_some() {
                            row.musicbrainz_artist_id
                                .clone_from(&input.musicbrainz_artist_id);
                        }
                        row.rating = input
                            .rating
                            .filter(|rating| (0..=10).contains(rating))
                            .or(row.rating);
                    }
                    row.favorite = input.favorite.or(row.favorite);
                    if input.artwork_binding.is_some() {
                        row.artwork_binding.clone_from(&input.artwork_binding);
                    }
                }
            }
            let first = self
                .observe_entities(
                    "artist",
                    &rows
                        .iter()
                        .map(|row| row.object_id.as_str())
                        .collect::<Vec<_>>(),
                )
                .await?;
            let mut metadata = ArtistInput::write_rows(rows.iter().map(|input| {
                (
                    input,
                    self.source,
                    source_entity_uri(&SourceId::new(&self.source_id), "artist", &input.object_id),
                    input.name.to_lowercase(),
                )
            }));
            self.changes.metadata |=
                self.execute_result(metadata.build()).await?.rows_affected() > 0;
            let source = self.source;
            let facts = || {
                let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                    "WITH input(source,object_id,favorite,rating,full,first) AS (",
                );
                query.push_values(&rows, |mut row, input| {
                    row.push_bind(source)
                        .push_bind(&input.object_id)
                        .push_bind(input.favorite)
                        .push_bind(
                            input
                                .rating
                                .filter(|rating| (0..=10).contains(rating))
                                .map(|rating| rating * 10),
                        )
                        .push_bind(input.sort_text.is_some())
                        .push_bind(first.contains(&input.object_id));
                });
                query.push(") ");
                query
            };
            let mut state = facts();
            state.push("UPDATE artists SET source_favorite=COALESCE(input.favorite,source_favorite),
                source_rating=CASE WHEN input.full THEN input.rating ELSE COALESCE(input.rating,source_rating) END
                FROM input WHERE artists.source_key=input.source AND artists.object_id=input.object_id
                  AND ((input.favorite IS NOT NULL AND source_favorite IS NOT input.favorite)
                    OR ((input.full OR input.rating IS NOT NULL) AND source_rating IS NOT input.rating))");
            self.changes.user_data |= self.execute_result(state.build()).await?.rows_affected() > 0;
            let mut unresolved = facts();
            unresolved.push(
                "INSERT OR IGNORE INTO temp.scan_unresolved_artists
                SELECT object_id FROM input WHERE first AND favorite IS NULL",
            );
            self.execute(unresolved.build()).await?;
            let mut resolved = facts();
            resolved.push(
                "DELETE FROM temp.scan_unresolved_artists WHERE object_id IN
                (SELECT object_id FROM input WHERE favorite IS NOT NULL)",
            );
            self.execute(resolved.build()).await?;
            self.remember_artworks(
                "artist",
                &rows
                    .iter()
                    .map(|input| {
                        (
                            input.object_id.as_str(),
                            input.artwork_binding.as_deref(),
                            first.contains(&input.object_id),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .await?;
        }
        Ok(())
    }

    /// Artist credits whose favorite state has not been supplied by an artist record.
    pub async fn unresolved_artist_ids(&mut self) -> LibraryResult<Vec<String>> {
        if self.failed {
            return Err(LibraryError::ScanFailed);
        }
        let query = sqlx::query_scalar(
            "SELECT object_id FROM temp.scan_unresolved_artists ORDER BY object_id LIMIT 100",
        );
        let rows = if let Some(writer) = self.batch_writer.as_mut() {
            query
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?
        } else {
            let mut writer = self.database.writer().await?;
            query
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?
        };
        Ok(rows)
    }

    pub async fn write_genres(&mut self, inputs: &[&GenreInput]) -> LibraryResult<()> {
        for page in inputs.chunks(128) {
            let mut rows: Vec<GenreInput> = Vec::new();
            let mut positions = HashMap::new();
            for input in page {
                self.require_id("genre", &input.object_id)?;
                self.require_row_bytes(&[
                    input.object_id.as_bytes(),
                    input.name.as_bytes(),
                    input.name.to_lowercase().as_bytes(),
                    input.sort_text.as_deref().unwrap_or_default().as_bytes(),
                    input.artwork_binding.as_deref().unwrap_or_default(),
                ])?;
                let position = *positions
                    .entry(input.object_id.as_str())
                    .or_insert(rows.len());
                if position == rows.len() {
                    rows.push((*input).clone());
                } else {
                    let row = &mut rows[position];
                    if input.sort_text.is_some()
                        || row.name.to_lowercase() != input.name.to_lowercase()
                    {
                        row.name.clone_from(&input.name);
                    }
                    if input.sort_text.is_some() {
                        row.sort_text.clone_from(&input.sort_text);
                    }
                    if input.artwork_binding.is_some() {
                        row.artwork_binding.clone_from(&input.artwork_binding);
                    }
                }
            }
            let first = self
                .observe_entities(
                    "genre",
                    &rows
                        .iter()
                        .map(|row| row.object_id.as_str())
                        .collect::<Vec<_>>(),
                )
                .await?;
            let mut metadata = GenreInput::write_rows(
                rows.iter()
                    .map(|input| (input, self.source, input.name.to_lowercase())),
            );
            self.changes.metadata |=
                self.execute_result(metadata.build()).await?.rows_affected() > 0;
            self.remember_artworks(
                "genre",
                &rows
                    .iter()
                    .map(|input| {
                        (
                            input.object_id.as_str(),
                            input.artwork_binding.as_deref(),
                            first.contains(&input.object_id),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn write_moods(&mut self, inputs: &[&MoodInput]) -> LibraryResult<()> {
        for page in inputs.chunks(128) {
            let mut rows = HashMap::new();
            for input in page {
                self.require_id("mood", &input.object_id)?;
                self.require_row_bytes(&[
                    input.object_id.as_bytes(),
                    input.name.as_bytes(),
                    input.name.to_lowercase().as_bytes(),
                    input.sort_text.as_bytes(),
                ])?;
                rows.insert(input.object_id.as_str(), *input);
            }
            self.observe_entities("mood", &rows.keys().copied().collect::<Vec<_>>())
                .await?;
            let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                "INSERT INTO moods(source_key,object_id,name,normalized_name,sort_text) ",
            );
            query.push_values(rows.values(), |mut row, input| {
                row.push_bind(self.source)
                    .push_bind(&input.object_id)
                    .push_bind(&input.name)
                    .push_bind(input.name.to_lowercase())
                    .push_bind(&input.sort_text);
            });
            query.push(" ON CONFLICT(source_key,object_id) DO UPDATE SET
                name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text
                WHERE (moods.name,moods.normalized_name,moods.sort_text)
                    IS NOT (excluded.name,excluded.normalized_name,excluded.sort_text)");
            self.changes.metadata |= self.execute_result(query.build()).await?.rows_affected() > 0;
        }
        Ok(())
    }

    pub async fn write_folders(&mut self, inputs: &[&FolderInput]) -> LibraryResult<()> {
        for page in inputs.chunks(128) {
            let mut rows: Vec<FolderInput> = Vec::new();
            let mut positions = HashMap::new();
            for input in page {
                self.require_id("folder", &input.object_id)?;
                if let Some(name) = &input.name {
                    let normalized_name = name.to_lowercase();
                    self.require_row_bytes(&[
                        input.object_id.as_bytes(),
                        name.as_bytes(),
                        normalized_name.as_bytes(),
                        input
                            .sort_text
                            .as_deref()
                            .unwrap_or(&normalized_name)
                            .as_bytes(),
                        input.artwork_binding.as_deref().unwrap_or_default(),
                    ])?;
                }
                let position = *positions
                    .entry(input.object_id.as_str())
                    .or_insert(rows.len());
                if position == rows.len() {
                    rows.push((*input).clone());
                } else if input.name.is_some() {
                    let row = &mut rows[position];
                    if row.name.is_none() {
                        row.artwork_binding = None;
                    }
                    row.name.clone_from(&input.name);
                    row.sort_text.clone_from(&input.sort_text);
                    if input.artwork_binding.is_some() {
                        row.artwork_binding.clone_from(&input.artwork_binding);
                    }
                }
            }
            let first = self
                .observe_entities(
                    "folder",
                    &rows
                        .iter()
                        .filter(|row| row.name.is_some())
                        .map(|row| row.object_id.as_str())
                        .collect::<Vec<_>>(),
                )
                .await?;
            let mut references = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                "INSERT OR IGNORE INTO temp.scan_seen(kind,object_id,written) ",
            );
            references.push_values(&rows, |mut row, input| {
                row.push_bind("folder")
                    .push_bind(&input.object_id)
                    .push_bind(false);
            });
            self.execute(references.build()).await?;
            let mut metadata = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                "WITH input(source,object_id,name,normalized_name,sort_text) AS (",
            );
            metadata.push_values(&rows, |mut row, input| {
                let normalized = input
                    .name
                    .as_deref()
                    .unwrap_or(&input.object_id)
                    .to_lowercase();
                row.push_bind(self.source)
                    .push_bind(&input.object_id)
                    .push_bind(&input.name)
                    .push_bind(normalized.clone())
                    .push_bind(input.sort_text.clone().unwrap_or(normalized));
            });
            metadata.push(") INSERT INTO folders(source_key,object_id,name,normalized_name,sort_text)
                SELECT source,object_id,COALESCE(name,object_id),CASE WHEN name IS NULL THEN object_id ELSE normalized_name END,
                    CASE WHEN name IS NULL THEN object_id ELSE sort_text END FROM input WHERE true
                ON CONFLICT(source_key,object_id) DO UPDATE SET indexed=1,
                    name=CASE WHEN (SELECT name FROM input WHERE object_id=excluded.object_id) IS NULL THEN folders.name ELSE excluded.name END,
                    normalized_name=CASE WHEN (SELECT name FROM input WHERE object_id=excluded.object_id) IS NULL THEN folders.normalized_name ELSE excluded.normalized_name END,
                    sort_text=CASE WHEN (SELECT name FROM input WHERE object_id=excluded.object_id) IS NULL THEN folders.sort_text ELSE excluded.sort_text END
                WHERE folders.indexed=0 OR ((SELECT name FROM input WHERE object_id=excluded.object_id) IS NOT NULL
                    AND (folders.name,folders.normalized_name,folders.sort_text) IS NOT (excluded.name,excluded.normalized_name,excluded.sort_text))");
            self.changes.metadata |=
                self.execute_result(metadata.build()).await?.rows_affected() > 0;
            self.remember_artworks(
                "folder",
                &rows
                    .iter()
                    .filter(|input| input.name.is_some())
                    .map(|input| {
                        (
                            input.object_id.as_str(),
                            input.artwork_binding.as_deref(),
                            first.contains(&input.object_id),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .await?;
        }
        Ok(())
    }
}

// Clear absent fields only after every album record has supplied its facts.
pub(super) async fn finish_albums(
    connection: &mut sqlx::SqliteConnection,
    source: i64,
) -> LibraryResult<(bool, bool)> {
    let metadata = sqlx::query(sqlx::AssertSqlSafe(AlbumInput::finish_sql()))
        .bind(source)
        .execute(&mut *connection)
        .await?
        .rows_affected()
        > 0;
    let user_data = sqlx::query(
        "UPDATE albums AS album SET source_rating=NULL FROM temp.scan_seen seen
         WHERE album.source_key=?1 AND seen.kind='album' AND seen.object_id=album.object_id
           AND seen.written AND seen.supplied & ?2=0 AND album.source_rating IS NOT NULL",
    )
    .bind(source)
    .bind(AlbumInput::RATING)
    .execute(connection)
    .await?
    .rows_affected()
        > 0;
    Ok((metadata, user_data))
}
