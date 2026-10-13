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
        let mut written = 0;
        let mut albums: HashMap<&str, &AlbumInput> = HashMap::new();
        let mut artists: HashMap<&str, &ArtistInput> = HashMap::new();
        let mut genres: HashMap<&str, &GenreInput> = HashMap::new();
        let mut moods: HashMap<&str, &MoodInput> = HashMap::new();
        let mut album_artist_links = Vec::new();
        let mut album_genre_links = Vec::new();
        let mut release_type_links = Vec::new();
        let mut track_artist_links = Vec::new();
        let mut track_genre_links = Vec::new();
        let mut track_mood_links = Vec::new();
        for &(track, album) in page {
            let track_id = track.object_id.as_str();
            if let Some(album) = album
                && albums.insert(album.object_id.as_str(), album) != Some(album)
            {
                self.write_album_row(album).await?;
            }
            written += usize::from(self.write_track_row(track, first.remove(track_id)).await?);
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
                for artist in credits {
                    if artists.insert(artist.object_id.as_str(), artist) != Some(artist) {
                        self.write_artist(artist).await?;
                    }
                }
            }
            let album_genres = album_relations
                .map(|relations| relations.genres.as_slice())
                .unwrap_or_default();
            let track_genres = track_relations
                .map(|relations| relations.genres.as_slice())
                .unwrap_or_default();
            for entries in [album_genres, track_genres] {
                for genre in entries {
                    if genres.insert(genre.object_id.as_str(), genre) != Some(genre) {
                        self.write_genre(genre).await?;
                    }
                }
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
                for mood in &relations.moods {
                    if moods.insert(mood.object_id.as_str(), mood) != Some(mood) {
                        self.write_mood(mood).await?;
                    }
                }
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
            self.write_track_folder_membership(track).await?;
        }
        self.write_source_loudness(&page.iter().map(|(track, _)| *track).collect::<Vec<_>>())
            .await?;
        self.write_album_relations(&album_artist_links, &album_genre_links, &release_type_links)
            .await?;
        self.write_track_relations(&track_artist_links, &track_genre_links, &track_mood_links)
            .await?;
        Ok(written)
    }

    pub async fn write_record(&mut self, input: &RecordInput) -> LibraryResult<()> {
        match input {
            RecordInput::Track(input) => self
                .write_tracks(&[(input.as_ref(), None)])
                .await
                .map(|_| ()),
            RecordInput::Album(input) => self.write_album(input).await,
            RecordInput::Artist(input) => self.write_artist(input).await,
            RecordInput::Genre(input) => self.write_genre(input).await,
            RecordInput::Folder(input) => self.write_folder(input).await,
            RecordInput::Playlist(input) => self.write_playlist(input).await,
        }
    }

    pub async fn write_album(&mut self, input: &AlbumInput) -> LibraryResult<()> {
        self.write_album_row(input).await?;
        let object_id = input.object_id.as_str();
        if let Some(relations) = &input.relations {
            let artists = if relations.album_artists.is_empty() {
                &relations.artists
            } else {
                &relations.album_artists
            };
            for artist in artists {
                self.write_artist(artist).await?;
            }
            for genre in &relations.genres {
                self.write_genre(genre).await?;
            }
            self.write_album_relations(
                &artists
                    .iter()
                    .map(|artist| (object_id, artist.object_id.as_str()))
                    .collect::<Vec<_>>(),
                &relations
                    .genres
                    .iter()
                    .map(|genre| (object_id, genre.object_id.as_str()))
                    .collect::<Vec<_>>(),
                &relations
                    .release_types
                    .iter()
                    .map(|release_type| (object_id, release_type.as_str()))
                    .collect::<Vec<_>>(),
            )
            .await?;
        }
        Ok(())
    }

    async fn write_album_row(&mut self, input: &AlbumInput) -> LibraryResult<()> {
        let object_id = input.object_id.as_str();
        let normalized_title = input.title.to_lowercase();
        self.require_id("album", object_id)?;
        let media_uri = source_entity_uri(&SourceId::new(&self.source_id), "album", object_id);
        let rating = input
            .rating
            .filter(|rating| (0..=10).contains(rating))
            .map(|rating| rating * 10);
        self.require_row_bytes(&[
            object_id.as_bytes(),
            input.title.as_bytes(),
            normalized_title.as_bytes(),
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
        let first = self.observe("album", object_id).await?;
        let supplied = input.supplied(rating);
        let changed = self
            .execute_result(input.write_query(self.source, media_uri, normalized_title, first))
            .await?
            .rows_affected()
            > 0;
        self.changes.metadata |= changed;
        self.changes.user_data |= self.execute_result(sqlx::query(
            "UPDATE albums AS album SET source_favorite=COALESCE(?2,album.source_favorite),
                source_rating=CASE WHEN seen.supplied & ?5=0 THEN COALESCE(?3,album.source_rating) ELSE album.source_rating END
             FROM temp.scan_seen seen
             WHERE album.source_key=?4 AND album.object_id=?1 AND seen.kind='album' AND seen.object_id=?1
               AND ((?2 IS NOT NULL AND album.source_favorite IS NOT ?2)
                 OR (seen.supplied & ?5=0 AND ?3 IS NOT NULL AND album.source_rating IS NOT ?3))")
            .bind(object_id).bind(input.favorite).bind(rating).bind(self.source).bind(AlbumInput::RATING))
            .await?.rows_affected() > 0;
        self.execute(sqlx::query("UPDATE temp.scan_seen SET supplied=supplied | ?2 WHERE kind='album' AND object_id=?1")
            .bind(object_id).bind(supplied)).await?;
        self.remember_artwork("album", object_id, input.artwork_binding.as_deref(), first)
            .await?;
        Ok(())
    }

    async fn write_track_row(
        &mut self,
        input: &TrackInput,
        first: Option<bool>,
    ) -> LibraryResult<bool> {
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
        let Some(existed) = first else {
            return Ok(false);
        };
        let rating = input
            .rating
            .filter(|rating| (0..=10).contains(rating))
            .map(|rating| rating * 10);
        self.changes.metadata |= self
            .execute_result(
                sqlx::query(
                    "UPDATE tracks SET source_key=NULL,object_id=NULL,indexed=0
             WHERE source_key=?1 AND object_id=?2 AND media_uri<>?3",
                )
                .bind(self.source)
                .bind(object_id)
                .bind(&media_uri),
            )
            .await?
            .rows_affected()
            > 0;
        let changed = self
            .execute_result(input.write_query(
                self.source,
                &media_uri,
                normalized_search,
                audio_properties,
            ))
            .await?
            .rows_affected()
            > 0;
        self.changes.metadata |= changed;
        self.changes.tracks_added |= !existed && changed;
        self.changes.user_data |= self.execute_result(sqlx::query(
            "UPDATE tracks SET source_favorite=COALESCE(?2,source_favorite),source_rating=?3
             WHERE source_key=?4 AND object_id=?1
               AND ((?2 IS NOT NULL AND source_favorite IS NOT ?2) OR source_rating IS NOT ?3)")
            .bind(object_id).bind(input.favorite).bind(rating).bind(self.source))
            .await?.rows_affected() > 0;
        if let Some(album) = album_object_id {
            self.execute(sqlx::query(
                "INSERT INTO temp.scan_track_albums(track_id,album_id)
                 SELECT ?1,?2 WHERE NOT EXISTS(SELECT 1 FROM albums WHERE source_key=?3 AND object_id=?2)
                 ON CONFLICT(track_id) DO UPDATE SET album_id=excluded.album_id")
                .bind(object_id).bind(album).bind(self.source)).await?;
        }
        if input.source_play_count.is_some()
            || input.source_skip_count.is_some()
            || input.source_last_played.is_some()
        {
            self.changes.user_data |= self.execute_result(sqlx::query(
                "UPDATE tracks SET source_play_count=?3,source_skip_count=?4,source_last_played=?5
                 WHERE source_key=?1 AND object_id=?2
                   AND (source_play_count,source_skip_count,source_last_played) IS NOT (?3,?4,?5)")
                .bind(self.source).bind(object_id).bind(input.source_play_count)
                .bind(input.source_skip_count).bind(input.source_last_played)).await?.rows_affected() > 0;
        }
        self.remember_artwork("track", object_id, input.artwork_binding.as_deref(), true)
            .await?;
        Ok(true)
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

    async fn write_track_folder_membership(&mut self, input: &TrackInput) -> LibraryResult<()> {
        if let Some(folders) = &input.folders {
            self.replace_track_folders(
                &input.object_id,
                &folders
                    .iter()
                    .map(|folder| ScanLink::new(&input.object_id, folder, 0))
                    .collect::<Vec<_>>(),
            )
            .await?;
        }
        Ok(())
    }

    /// Full artist records supply their effective sort name, including the display-name fallback.
    /// Sparse credits leave it absent so they retain the existing artist sort name.
    pub async fn write_artist(&mut self, input: &ArtistInput) -> LibraryResult<()> {
        let object_id = input.object_id.as_str();
        let sort_text = input.sort_text.as_deref();
        let normalized_name = input.name.to_lowercase();
        self.require_id("artist", object_id)?;
        let media_uri = source_entity_uri(&SourceId::new(&self.source_id), "artist", object_id);
        let rating = input
            .rating
            .filter(|rating| (0..=10).contains(rating))
            .map(|rating| rating * 10);
        self.require_row_bytes(&[
            object_id.as_bytes(),
            input.name.as_bytes(),
            normalized_name.as_bytes(),
            sort_text.unwrap_or_default().as_bytes(),
            input
                .musicbrainz_artist_id
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
            input.artwork_binding.as_deref().unwrap_or_default(),
        ])?;
        let first = self.observe("artist", object_id).await?;
        let changed = self
            .execute_result(input.write_query(self.source, media_uri, normalized_name))
            .await?
            .rows_affected()
            > 0;
        self.changes.metadata |= changed;
        self.changes.user_data |= self
            .execute_result(
                sqlx::query(
                    "UPDATE artists SET source_favorite=COALESCE(?2,source_favorite),
                source_rating=CASE WHEN ?4 THEN ?3 ELSE COALESCE(?3,source_rating) END
             WHERE source_key=?5 AND object_id=?1
               AND ((?2 IS NOT NULL AND source_favorite IS NOT ?2)
                 OR ((?4 OR ?3 IS NOT NULL) AND source_rating IS NOT ?3))",
                )
                .bind(object_id)
                .bind(input.favorite)
                .bind(rating)
                .bind(sort_text.is_some())
                .bind(self.source),
            )
            .await?
            .rows_affected()
            > 0;
        if first && input.favorite.is_none() {
            self.execute(
                sqlx::query("INSERT OR IGNORE INTO temp.scan_unresolved_artists VALUES(?1)")
                    .bind(object_id),
            )
            .await?;
        } else if input.favorite.is_some() {
            self.execute(
                sqlx::query("DELETE FROM temp.scan_unresolved_artists WHERE object_id=?1")
                    .bind(object_id),
            )
            .await?;
        }
        self.remember_artwork("artist", object_id, input.artwork_binding.as_deref(), first)
            .await
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

    pub async fn write_genre(&mut self, input: &GenreInput) -> LibraryResult<()> {
        let object_id = input.object_id.as_str();
        let normalized_name = input.name.to_lowercase();
        self.require_id("genre", object_id)?;
        self.require_row_bytes(&[
            object_id.as_bytes(),
            input.name.as_bytes(),
            normalized_name.as_bytes(),
            input.sort_text.as_deref().unwrap_or_default().as_bytes(),
            input.artwork_binding.as_deref().unwrap_or_default(),
        ])?;
        let first = self.observe("genre", object_id).await?;
        let changed = self
            .execute_result(input.write_query(self.source, normalized_name))
            .await?
            .rows_affected()
            > 0;
        self.changes.metadata |= changed;
        self.remember_artwork("genre", object_id, input.artwork_binding.as_deref(), first)
            .await
    }

    pub async fn write_mood(&mut self, input: &MoodInput) -> LibraryResult<()> {
        let object_id = input.object_id.as_str();
        let name = input.name.as_str();
        let normalized_name = name.to_lowercase();
        let sort_text = input.sort_text.as_str();
        self.require_id("mood", object_id)?;
        self.require_row_bytes(&[
            object_id.as_bytes(),
            name.as_bytes(),
            normalized_name.as_bytes(),
            sort_text.as_bytes(),
        ])?;
        self.observe("mood", object_id).await?;
        self.changes.metadata |= self.execute_result(sqlx::query(
            "INSERT INTO moods(source_key,object_id,name,normalized_name,sort_text) VALUES(?5,?1,?2,?3,?4)
             ON CONFLICT(source_key,object_id) DO UPDATE SET
                name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text
             WHERE (moods.name,moods.normalized_name,moods.sort_text)
                IS NOT (excluded.name,excluded.normalized_name,excluded.sort_text)")
            .bind(object_id).bind(name).bind(&normalized_name).bind(sort_text).bind(self.source))
            .await?.rows_affected() > 0;
        Ok(())
    }

    pub async fn write_folder(&mut self, input: &FolderInput) -> LibraryResult<()> {
        let object_id = input.object_id.as_str();
        let Some(name) = input.name.as_deref() else {
            return self.write_folder_reference(object_id).await;
        };
        let normalized_name = name.to_lowercase();
        let sort_text = input.sort_text.as_deref().unwrap_or(&normalized_name);
        let artwork_binding = input.artwork_binding.as_deref();
        self.require_id("folder", object_id)?;
        self.require_row_bytes(&[
            object_id.as_bytes(),
            name.as_bytes(),
            normalized_name.as_bytes(),
            sort_text.as_bytes(),
            artwork_binding.unwrap_or_default(),
        ])?;
        let first = self.observe("folder", object_id).await?;
        self.changes.metadata |= self.execute_result(sqlx::query(
            "INSERT INTO folders(source_key,object_id,name,normalized_name,sort_text) VALUES(?5,?1,?2,?3,?4)
             ON CONFLICT(source_key,object_id) DO UPDATE SET
                name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text
             WHERE (folders.name,folders.normalized_name,folders.sort_text)
                IS NOT (excluded.name,excluded.normalized_name,excluded.sort_text)")
            .bind(object_id).bind(name).bind(&normalized_name).bind(sort_text).bind(self.source))
            .await?.rows_affected() > 0;
        self.remember_artwork("folder", object_id, artwork_binding, first)
            .await
    }

    /// A membership reference must not replace the section's advertised display name.
    async fn write_folder_reference(&mut self, object_id: &str) -> LibraryResult<()> {
        self.require_id("folder", object_id)?;
        self.execute(
            sqlx::query("INSERT OR IGNORE INTO temp.scan_seen(kind,object_id,written) VALUES('folder',?1,0)")
                .bind(object_id),
        )
        .await?;
        self.changes.metadata |= self
            .execute_result(
                sqlx::query(
                    "INSERT INTO folders(source_key,object_id,name,normalized_name,sort_text)
             VALUES(?1,?2,?2,?2,?2) ON CONFLICT(source_key,object_id) DO UPDATE SET indexed=1 WHERE indexed=0",
                )
                .bind(self.source)
                .bind(object_id),
            )
            .await?
            .rows_affected()
            > 0;
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
