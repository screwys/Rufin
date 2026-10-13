use std::collections::HashMap;

use super::{
    AlbumInput, ArtistInput, FolderInput, GenreInput, MoodInput, RecordInput, Scan, TrackInput,
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
        let mut folders = HashMap::new();
        for &(track, _) in page {
            let track_id = track.object_id.as_str();
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
            if let Some(entries) = &track.folders {
                folders.insert(track_id, entries);
            }
        }
        self.write_album_rows(
            &page
                .iter()
                .filter_map(|(_, album)| *album)
                .collect::<Vec<_>>(),
        )
        .await?;
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
                .flat_map(|(id, entries)| entries.iter().map(|folder| (*id, folder.as_str())))
                .collect::<Vec<_>>(),
        )
        .await?;
        self.write_source_loudness(&page.iter().map(|(track, _)| *track).collect::<Vec<_>>())
            .await?;
        self.write_credits(page.iter().map(|(track, album)| (Some(*track), *album)))
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
            self.write_credits(page.iter().map(|album| (None, Some(*album))))
                .await?;
        }
        Ok(())
    }

    async fn write_credits<'a>(
        &mut self,
        inputs: impl IntoIterator<Item = (Option<&'a TrackInput>, Option<&'a AlbumInput>)>,
    ) -> LibraryResult<()> {
        let mut artists = Vec::new();
        let mut genres = Vec::new();
        let mut moods = Vec::new();
        let mut album_artists = Vec::new();
        let mut album_genres = Vec::new();
        let mut release_types = Vec::new();
        let mut track_artists = Vec::new();
        let mut track_genres = Vec::new();
        let mut track_moods = Vec::new();
        for (track, album) in inputs {
            if let Some(album) = album {
                let id = album.object_id.as_str();
                let credits = if album.album_artists.is_empty() {
                    &album.artists
                } else {
                    &album.album_artists
                };
                artists.extend(credits);
                genres.extend(&album.genres);
                album_artists.extend(credits.iter().map(|artist| (id, artist.object_id.as_str())));
                album_genres.extend(
                    album
                        .genres
                        .iter()
                        .map(|genre| (id, genre.object_id.as_str())),
                );
                release_types.extend(album.release_types.iter().map(|kind| (id, kind.as_str())));
            }
            if let Some(track) = track {
                let id = track.object_id.as_str();
                artists.extend(&track.artists);
                artists.extend(&track.album_artists);
                genres.extend(&track.genres);
                moods.extend(&track.moods);
                track_artists.extend(
                    track
                        .artists
                        .iter()
                        .map(|artist| (id, artist.object_id.as_str())),
                );
                track_genres.extend(
                    track
                        .genres
                        .iter()
                        .map(|genre| (id, genre.object_id.as_str())),
                );
                track_moods.extend(track.moods.iter().map(|mood| (id, mood.object_id.as_str())));
            }
        }
        artists.sort_by(|left, right| left.object_id.cmp(&right.object_id));
        artists.dedup();
        genres.sort_by(|left, right| left.object_id.cmp(&right.object_id));
        genres.dedup();
        moods.sort_by(|left, right| left.object_id.cmp(&right.object_id));
        moods.dedup();
        self.write_artists(&artists).await?;
        self.write_genres(&genres).await?;
        self.write_moods(&moods).await?;
        for (owner, target, links) in [
            ("album", "artist", &album_artists),
            ("album", "genre", &album_genres),
            ("album", "release_type", &release_types),
            ("track", "artist", &track_artists),
            ("track", "genre", &track_genres),
            ("track", "mood", &track_moods),
        ] {
            self.write_relations(owner, target, links).await?;
        }
        Ok(())
    }

    async fn write_album_rows(&mut self, inputs: &[&AlbumInput]) -> LibraryResult<()> {
        for page in inputs.chunks(128) {
            let mut unique = page.to_vec();
            unique.sort_by(|left, right| left.object_id.cmp(&right.object_id));
            unique.dedup();
            let page = unique.as_slice();
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
            }
            let first = self
                .observe_entities(
                    "album",
                    &page
                        .iter()
                        .map(|row| row.object_id.as_str())
                        .collect::<Vec<_>>(),
                )
                .await?;
            let mut metadata = AlbumInput::write_rows(page.iter().map(|input| {
                (
                    *input,
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
                    "WITH raw(ordinal,source,object_id,favorite,rating,supplied) AS (",
                );
                query.push_values(page.iter().enumerate(), |mut row, (ordinal, input)| {
                    let rating = input
                        .rating
                        .filter(|rating| (0..=10).contains(rating))
                        .map(|rating| rating * 10);
                    row.push_bind(ordinal as i64)
                        .push_bind(source)
                        .push_bind(&input.object_id)
                        .push_bind(input.favorite)
                        .push_bind(rating)
                        .push_bind(input.supplied(rating));
                });
                query.push(") ");
                query
            };
            let mut state = facts();
            state.push(", input AS (SELECT source,object_id,
                (SELECT candidate.favorite FROM raw candidate WHERE candidate.object_id=raw.object_id AND candidate.favorite IS NOT NULL ORDER BY candidate.ordinal DESC LIMIT 1) favorite,
                (SELECT candidate.rating FROM raw candidate WHERE candidate.object_id=raw.object_id AND candidate.rating IS NOT NULL ORDER BY candidate.ordinal LIMIT 1) rating
                FROM raw WHERE ordinal IN (SELECT MIN(ordinal) FROM raw GROUP BY object_id))
                UPDATE albums AS album SET source_favorite=COALESCE(input.favorite,album.source_favorite),
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
                "INSERT INTO temp.scan_seen(kind,object_id,supplied)
                SELECT 'album',object_id,supplied FROM raw WHERE true
                ON CONFLICT(kind,object_id) DO UPDATE SET supplied=scan_seen.supplied | excluded.supplied
                WHERE scan_seen.supplied | excluded.supplied <> scan_seen.supplied",
            );
            self.execute(supplied.build()).await?;
            self.remember_artworks(
                "album",
                &page
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
            }
            let first = self
                .observe_entities(
                    "artist",
                    &page
                        .iter()
                        .map(|row| row.object_id.as_str())
                        .collect::<Vec<_>>(),
                )
                .await?;
            let mut metadata = ArtistInput::write_rows(page.iter().map(|input| {
                (
                    *input,
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
                    "WITH raw(ordinal,source,object_id,favorite,rating,complete,first) AS (",
                );
                query.push_values(page.iter().enumerate(), |mut row, (ordinal, input)| {
                    row.push_bind(ordinal as i64)
                        .push_bind(source)
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
            state.push(", input AS (SELECT source,object_id,
                (SELECT candidate.favorite FROM raw candidate WHERE candidate.object_id=raw.object_id AND candidate.favorite IS NOT NULL ORDER BY candidate.ordinal DESC LIMIT 1) favorite,
                (SELECT candidate.rating FROM raw candidate WHERE candidate.object_id=raw.object_id AND (candidate.complete OR candidate.rating IS NOT NULL) ORDER BY candidate.ordinal DESC LIMIT 1) rating,
                EXISTS(SELECT 1 FROM raw candidate WHERE candidate.object_id=raw.object_id AND candidate.complete) AS complete
                FROM raw WHERE ordinal IN (SELECT MAX(ordinal) FROM raw GROUP BY object_id))
                UPDATE artists SET source_favorite=COALESCE(input.favorite,source_favorite),
                source_rating=CASE WHEN input.complete THEN input.rating ELSE COALESCE(input.rating,source_rating) END
                FROM input WHERE artists.source_key=input.source AND artists.object_id=input.object_id
                  AND ((input.favorite IS NOT NULL AND source_favorite IS NOT input.favorite)
                    OR ((input.complete OR input.rating IS NOT NULL) AND source_rating IS NOT input.rating))");
            self.changes.user_data |= self.execute_result(state.build()).await?.rows_affected() > 0;
            let mut unresolved = facts();
            unresolved.push(
                "INSERT OR IGNORE INTO temp.scan_unresolved_artists
                SELECT object_id FROM raw WHERE first AND favorite IS NULL",
            );
            self.execute(unresolved.build()).await?;
            let mut resolved = facts();
            resolved.push(
                "DELETE FROM temp.scan_unresolved_artists WHERE object_id IN
                (SELECT object_id FROM raw WHERE favorite IS NOT NULL)",
            );
            self.execute(resolved.build()).await?;
            self.remember_artworks(
                "artist",
                &page
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
            for input in page {
                self.require_id("genre", &input.object_id)?;
                self.require_row_bytes(&[
                    input.object_id.as_bytes(),
                    input.name.as_bytes(),
                    input.name.to_lowercase().as_bytes(),
                    input.sort_text.as_deref().unwrap_or_default().as_bytes(),
                    input.artwork_binding.as_deref().unwrap_or_default(),
                ])?;
            }
            let first = self
                .observe_entities(
                    "genre",
                    &page
                        .iter()
                        .map(|row| row.object_id.as_str())
                        .collect::<Vec<_>>(),
                )
                .await?;
            let mut metadata = GenreInput::write_rows(
                page.iter()
                    .map(|input| (*input, self.source, input.name.to_lowercase())),
            );
            self.changes.metadata |=
                self.execute_result(metadata.build()).await?.rows_affected() > 0;
            self.remember_artworks(
                "genre",
                &page
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
            for input in page {
                self.require_id("mood", &input.object_id)?;
                self.require_row_bytes(&[
                    input.object_id.as_bytes(),
                    input.name.as_bytes(),
                    input.name.to_lowercase().as_bytes(),
                    input.sort_text.as_bytes(),
                ])?;
            }
            self.observe_entities(
                "mood",
                &page
                    .iter()
                    .map(|input| input.object_id.as_str())
                    .collect::<Vec<_>>(),
            )
            .await?;
            let mut query = MoodInput::write_rows(
                page.iter()
                    .map(|input| (*input, self.source, input.name.to_lowercase())),
            );
            self.changes.metadata |= self.execute_result(query.build()).await?.rows_affected() > 0;
        }
        Ok(())
    }

    pub async fn write_folders(&mut self, inputs: &[&FolderInput]) -> LibraryResult<()> {
        for page in inputs.chunks(128) {
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
            }
            let first = self
                .observe_entities(
                    "folder",
                    &page
                        .iter()
                        .filter(|row| row.name.is_some())
                        .map(|row| row.object_id.as_str())
                        .collect::<Vec<_>>(),
                )
                .await?;
            let mut references = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                "INSERT OR IGNORE INTO temp.scan_seen(kind,object_id,written) ",
            );
            references.push_values(page, |mut row, input| {
                row.push_bind("folder")
                    .push_bind(&input.object_id)
                    .push_bind(false);
            });
            self.execute(references.build()).await?;
            let mut metadata = FolderInput::write_rows(page.iter().map(|input| {
                (
                    *input,
                    self.source,
                    input.name.as_ref().map(|name| name.to_lowercase()),
                )
            }));
            self.changes.metadata |=
                self.execute_result(metadata.build()).await?.rows_affected() > 0;
            self.write_relations(
                "track",
                "folder",
                &page
                    .iter()
                    .flat_map(|folder| {
                        folder
                            .tracks
                            .iter()
                            .map(|track| (track.as_str(), folder.object_id.as_str()))
                    })
                    .collect::<Vec<_>>(),
            )
            .await?;
            self.remember_artworks(
                "folder",
                &page
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
