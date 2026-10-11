use super::Scan;
use crate::{
    LibraryError, LibraryResult, SourceId, cue_media_uri, normalize_direct_media_uri,
    source_entity_uri,
};

// Album records can arrive from several files. Track supplied fields without copying their values.
const YEAR: i64 = 1;
const RELEASE_DATE: i64 = 2;
const DATE_ADDED: i64 = 4;
const RELEASE_ID: i64 = 8;
const RELEASE_GROUP_ID: i64 = 16;
const COMPILATION_TRUE: i64 = 32;
const COMPILATION_FALSE: i64 = 64;
const RATING: i64 = 128;

pub(super) const ALBUM_SUPPLIED: &str = "
    (album.year IS NOT NULL)*1 | (album.release_date IS NOT NULL)*2 |
    (album.date_added IS NOT NULL)*4 | (album.musicbrainz_release_id IS NOT NULL)*8 |
    (album.musicbrainz_release_group_id IS NOT NULL)*16 |
    COALESCE(album.is_compilation=1,0)*32 | COALESCE(album.is_compilation=0,0)*64 |
    (album.source_rating IS NOT NULL)*128";

impl Scan {
    #[allow(clippy::too_many_arguments)]
    pub async fn write_album(
        &mut self,
        object_id: &str,
        title: &str,
        normalized_title: &str,
        display_artist: &str,
        sort_text: &str,
        year: Option<i64>,
        release_date: Option<&str>,
        date_added: Option<&str>,
        musicbrainz_release_id: Option<&str>,
        musicbrainz_release_group_id: Option<&str>,
        is_compilation: Option<bool>,
        artwork_binding: Option<&[u8]>,
        favorite: impl Into<Option<bool>>,
        rating: Option<i64>,
        first_seen_at: Option<i64>,
    ) -> LibraryResult<()> {
        self.require_id("album", object_id)?;
        let media_uri = source_entity_uri(&SourceId::new(&self.source_id), "album", object_id);
        let rating = rating
            .filter(|rating| (0..=10).contains(rating))
            .map(|rating| rating * 10);
        self.require_row_bytes(&[
            object_id.as_bytes(),
            title.as_bytes(),
            normalized_title.as_bytes(),
            display_artist.as_bytes(),
            sort_text.as_bytes(),
            release_date.unwrap_or_default().as_bytes(),
            date_added.unwrap_or_default().as_bytes(),
            musicbrainz_release_id.unwrap_or_default().as_bytes(),
            musicbrainz_release_group_id.unwrap_or_default().as_bytes(),
            artwork_binding.unwrap_or_default(),
        ])?;
        let first = self.observe("album", object_id).await?;
        let supplied = i64::from(year.is_some()) * YEAR
            | i64::from(release_date.is_some()) * RELEASE_DATE
            | i64::from(date_added.is_some()) * DATE_ADDED
            | i64::from(musicbrainz_release_id.is_some()) * RELEASE_ID
            | i64::from(musicbrainz_release_group_id.is_some()) * RELEASE_GROUP_ID
            | i64::from(is_compilation == Some(true)) * COMPILATION_TRUE
            | i64::from(is_compilation == Some(false)) * COMPILATION_FALSE
            | i64::from(rating.is_some()) * RATING;
        let changed = self.execute_result(
            sqlx::query(
                "INSERT INTO albums(
                    source_key,object_id,media_uri,title,normalized_title,display_artist,sort_text,
                    year,release_date,date_added,musicbrainz_release_id,musicbrainz_release_group_id,
                    is_compilation,first_seen_at)
                 SELECT ?14,?1,?2,
                    CASE WHEN ?15 THEN ?3 ELSE previous.title END,
                    CASE WHEN ?15 THEN ?4 ELSE previous.normalized_title END,
                    CASE WHEN ?15 THEN ?5 ELSE previous.display_artist END,
                    CASE WHEN ?15 THEN ?6 ELSE previous.sort_text END,
                    CASE WHEN seen.supplied & 1=0 THEN COALESCE(?7,previous.year) ELSE previous.year END,
                    CASE WHEN seen.supplied & 2=0 THEN COALESCE(?8,previous.release_date) ELSE previous.release_date END,
                    CASE WHEN seen.supplied & 4=0 THEN COALESCE(?9,previous.date_added) ELSE previous.date_added END,
                    CASE WHEN seen.supplied & 8=0 THEN COALESCE(?10,previous.musicbrainz_release_id) ELSE previous.musicbrainz_release_id END,
                    CASE WHEN seen.supplied & 16=0 THEN COALESCE(?11,previous.musicbrainz_release_group_id) ELSE previous.musicbrainz_release_group_id END,
                    CASE WHEN previous.album_key IS NULL THEN ?12 ELSE previous.is_compilation END,
                    COALESCE(previous.first_seen_at,?13)
                 FROM (SELECT 1) LEFT JOIN albums previous
                    ON previous.source_key=?14 AND previous.object_id=?1
                 JOIN temp.scan_seen seen ON seen.kind='album' AND seen.object_id=?1 WHERE true
                 ON CONFLICT(source_key,object_id) DO UPDATE SET
                    media_uri=excluded.media_uri,title=excluded.title,
                    normalized_title=excluded.normalized_title,display_artist=excluded.display_artist,
                    sort_text=excluded.sort_text,year=excluded.year,release_date=excluded.release_date,
                    date_added=excluded.date_added,musicbrainz_release_id=excluded.musicbrainz_release_id,
                    musicbrainz_release_group_id=excluded.musicbrainz_release_group_id,
                    is_compilation=excluded.is_compilation,first_seen_at=excluded.first_seen_at
                 WHERE (albums.media_uri,albums.title,albums.normalized_title,albums.display_artist,
                    albums.sort_text,albums.year,albums.release_date,albums.date_added,
                    albums.musicbrainz_release_id,albums.musicbrainz_release_group_id,
                    albums.is_compilation,albums.first_seen_at)
                 IS NOT (excluded.media_uri,excluded.title,excluded.normalized_title,excluded.display_artist,
                    excluded.sort_text,excluded.year,excluded.release_date,excluded.date_added,
                    excluded.musicbrainz_release_id,excluded.musicbrainz_release_group_id,
                    excluded.is_compilation,excluded.first_seen_at)",
            )
            .bind(object_id)
            .bind(media_uri)
            .bind(title)
            .bind(normalized_title)
            .bind(display_artist)
            .bind(sort_text)
            .bind(year)
            .bind(release_date)
            .bind(date_added)
            .bind(musicbrainz_release_id)
            .bind(musicbrainz_release_group_id)
            .bind(is_compilation)
            .bind(first_seen_at)
            .bind(self.source)
            .bind(first),
        ).await?.rows_affected() > 0;
        self.changes.metadata |= changed;
        self.changes.user_data |= self.execute_result(sqlx::query(
            "UPDATE albums AS album SET source_favorite=COALESCE(?2,album.source_favorite),
                source_rating=CASE WHEN seen.supplied & 128=0 THEN COALESCE(?3,album.source_rating) ELSE album.source_rating END
             FROM temp.scan_seen seen
             WHERE album.source_key=?4 AND album.object_id=?1 AND seen.kind='album' AND seen.object_id=?1
               AND ((?2 IS NOT NULL AND album.source_favorite IS NOT ?2)
                 OR (seen.supplied & 128=0 AND ?3 IS NOT NULL AND album.source_rating IS NOT ?3))")
            .bind(object_id).bind(favorite.into()).bind(rating).bind(self.source))
            .await?.rows_affected() > 0;
        self.execute(sqlx::query("UPDATE temp.scan_seen SET supplied=supplied | ?2 WHERE kind='album' AND object_id=?1")
            .bind(object_id).bind(supplied)).await?;
        self.remember_artwork("album", object_id, artwork_binding, first)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn write_track(
        &mut self,
        object_id: &str,
        album_object_id: Option<&str>,
        title: &str,
        display_album: &str,
        display_artist: &str,
        sort_text: &str,
        duration_millis: i64,
        disc_number: i64,
        track_number: i64,
        year: Option<i64>,
        release_date: Option<&str>,
        date_added: Option<&str>,
        media_uri: Option<&str>,
        source_format: Option<&str>,
        comment: Option<&str>,
        bpm: Option<i64>,
        musicbrainz_recording_id: Option<&str>,
        musicbrainz_release_track_id: Option<&str>,
        cue_path: Option<&str>,
        cue_start_millis: Option<i64>,
        cue_end_millis: Option<i64>,
        artwork_binding: Option<&[u8]>,
        favorite: impl Into<Option<bool>>,
        rating: Option<i64>,
        first_seen_at: Option<i64>,
        source_play_count: Option<i64>,
        source_skip_count: Option<i64>,
        source_last_played: Option<i64>,
        source_path: Option<&str>,
        loudness_analysis_key: [u8; 32],
        audio_properties: &crate::AudioProperties,
    ) -> LibraryResult<bool> {
        let normalized_search = format!(
            "{title} {display_artist} {display_album} {} {}",
            comment.unwrap_or_default(),
            year.map(|value| value.to_string()).unwrap_or_default()
        )
        .to_lowercase();
        let audio_properties = serde_json::to_string(audio_properties)?;
        self.require_id("track", object_id)?;
        let direct_uri = media_uri.and_then(normalize_direct_media_uri);
        let media_uri = match (direct_uri, cue_start_millis, cue_end_millis) {
            (Some(file_uri), Some(start), Some(end)) => {
                cue_media_uri(object_id, &file_uri, start, end)
            }
            (Some(uri), None, None) => uri,
            _ => source_entity_uri(&SourceId::new(&self.source_id), "track", object_id),
        };
        let rating = rating
            .filter(|rating| (0..=10).contains(rating))
            .map(|rating| rating * 10);
        if let Some(album_id) = album_object_id {
            self.require_id("album", album_id)?;
        }
        if source_play_count.is_some_and(|value| value < 0)
            || source_skip_count.is_some_and(|value| value < 0)
            || source_last_played.is_some_and(|value| value < 0)
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
            sort_text.as_bytes(),
            release_date.unwrap_or_default().as_bytes(),
            date_added.unwrap_or_default().as_bytes(),
            media_uri.as_bytes(),
            source_path.unwrap_or_default().as_bytes(),
            source_format.unwrap_or_default().as_bytes(),
            audio_properties.as_bytes(),
            comment.unwrap_or_default().as_bytes(),
            musicbrainz_recording_id.unwrap_or_default().as_bytes(),
            musicbrainz_release_track_id.unwrap_or_default().as_bytes(),
            cue_path.unwrap_or_default().as_bytes(),
            artwork_binding.unwrap_or_default(),
            loudness_analysis_key.as_slice(),
        ])?;
        let existed = self.contains_entity("track", object_id, true).await?;
        if !self.observe("track", object_id).await? {
            return Ok(false);
        }
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
        let changed = self.execute_result(
            sqlx::query(
                "INSERT INTO tracks(
                    source_key,object_id,album_key,title,normalized_search,display_album,display_artist,
                    sort_text,duration_millis,disc_number,track_number,year,release_date,date_added,
                    media_uri,source_path,source_format,comment,bpm,musicbrainz_recording_id,
                    musicbrainz_release_track_id,cue_path,cue_start_millis,cue_end_millis,
                    first_seen_at,source_loudness_analysis_key,
                    loudness_analysis_key,audio_properties)
                 SELECT ?27,?1,(SELECT album_key FROM albums WHERE source_key=?27 AND object_id=?2),
                    ?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,
                    ?21,?22,?23,
                    COALESCE(previous.first_seen_at,?24),?25,
                    COALESCE((SELECT access.loudness_analysis_key FROM local_access_files access
                        WHERE access.media_uri=?14
                        ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,
                            access.local_access_file_key LIMIT 1),?25),?26
                 FROM (SELECT 1) LEFT JOIN tracks previous
                    ON previous.media_uri=?14 WHERE true
                 ON CONFLICT(media_uri) DO UPDATE SET
                    source_key=excluded.source_key,object_id=excluded.object_id,indexed=1,album_key=excluded.album_key,title=excluded.title,normalized_search=excluded.normalized_search,
                    display_album=excluded.display_album,display_artist=excluded.display_artist,
                    sort_text=excluded.sort_text,duration_millis=excluded.duration_millis,
                    disc_number=excluded.disc_number,track_number=excluded.track_number,year=excluded.year,
                    release_date=excluded.release_date,date_added=excluded.date_added,media_uri=excluded.media_uri,
                    source_path=excluded.source_path,source_format=excluded.source_format,audio_properties=excluded.audio_properties,
                    comment=excluded.comment,bpm=excluded.bpm,musicbrainz_recording_id=excluded.musicbrainz_recording_id,
                    musicbrainz_release_track_id=excluded.musicbrainz_release_track_id,cue_path=excluded.cue_path,
                    cue_start_millis=excluded.cue_start_millis,cue_end_millis=excluded.cue_end_millis,
                    first_seen_at=excluded.first_seen_at,source_loudness_analysis_key=excluded.source_loudness_analysis_key,
                    loudness_analysis_key=excluded.loudness_analysis_key
                 WHERE (tracks.source_key,tracks.object_id,tracks.indexed,tracks.album_key,tracks.title,tracks.normalized_search,tracks.display_album,tracks.display_artist,
                    tracks.sort_text,tracks.duration_millis,tracks.disc_number,tracks.track_number,tracks.year,
                    tracks.release_date,tracks.date_added,tracks.media_uri,tracks.source_path,tracks.source_format,
                    tracks.audio_properties,tracks.comment,tracks.bpm,tracks.musicbrainz_recording_id,
                    tracks.musicbrainz_release_track_id,tracks.cue_path,tracks.cue_start_millis,tracks.cue_end_millis,
                    tracks.first_seen_at,tracks.source_loudness_analysis_key,
                    tracks.loudness_analysis_key)
                 IS NOT (excluded.source_key,excluded.object_id,1,excluded.album_key,excluded.title,excluded.normalized_search,excluded.display_album,excluded.display_artist,
                    excluded.sort_text,excluded.duration_millis,excluded.disc_number,excluded.track_number,excluded.year,
                    excluded.release_date,excluded.date_added,excluded.media_uri,excluded.source_path,excluded.source_format,
                    excluded.audio_properties,excluded.comment,excluded.bpm,excluded.musicbrainz_recording_id,
                    excluded.musicbrainz_release_track_id,excluded.cue_path,excluded.cue_start_millis,excluded.cue_end_millis,
                    excluded.first_seen_at,excluded.source_loudness_analysis_key,
                    excluded.loudness_analysis_key)",
            )
            .bind(object_id)
            .bind(album_object_id)
            .bind(title)
            .bind(normalized_search)
            .bind(display_album)
            .bind(display_artist)
            .bind(sort_text)
            .bind(duration_millis)
            .bind(disc_number)
            .bind(track_number)
            .bind(year)
            .bind(release_date)
            .bind(date_added)
            .bind(&media_uri)
            .bind(source_path)
            .bind(source_format)
            .bind(comment)
            .bind(bpm)
            .bind(musicbrainz_recording_id)
            .bind(musicbrainz_release_track_id)
            .bind(cue_path)
            .bind(cue_start_millis)
            .bind(cue_end_millis)
            .bind(first_seen_at)
            .bind(loudness_analysis_key.as_slice())
            .bind(audio_properties)
            .bind(self.source),
        ).await?.rows_affected() > 0;
        self.changes.metadata |= changed;
        self.changes.tracks_added |= !existed && changed;
        self.changes.user_data |= self.execute_result(sqlx::query(
            "UPDATE tracks SET source_favorite=COALESCE(?2,source_favorite),source_rating=?3
             WHERE source_key=?4 AND object_id=?1
               AND ((?2 IS NOT NULL AND source_favorite IS NOT ?2) OR source_rating IS NOT ?3)")
            .bind(object_id).bind(favorite.into()).bind(rating).bind(self.source))
            .await?.rows_affected() > 0;
        if let Some(album) = album_object_id {
            self.execute(sqlx::query(
                "INSERT INTO temp.scan_track_albums(track_id,album_id)
                 SELECT ?1,?2 WHERE NOT EXISTS(SELECT 1 FROM albums WHERE source_key=?3 AND object_id=?2)
                 ON CONFLICT(track_id) DO UPDATE SET album_id=excluded.album_id")
                .bind(object_id).bind(album).bind(self.source)).await?;
        }
        if source_play_count.is_some()
            || source_skip_count.is_some()
            || source_last_played.is_some()
        {
            self.changes.user_data |= self.execute_result(sqlx::query(
                "UPDATE tracks SET source_play_count=?3,source_skip_count=?4,source_last_played=?5
                 WHERE source_key=?1 AND object_id=?2
                   AND (source_play_count,source_skip_count,source_last_played) IS NOT (?3,?4,?5)")
                .bind(self.source).bind(object_id).bind(source_play_count)
                .bind(source_skip_count).bind(source_last_played)).await?.rows_affected() > 0;
        }
        self.remember_artwork("track", object_id, artwork_binding, true)
            .await?;
        Ok(true)
    }

    pub async fn write_track_source_loudness(
        &mut self,
        track_object_id: &str,
        integrated_lufs: Option<f64>,
        true_peak: Option<f64>,
        replay_gain_db: Option<f64>,
        replay_gain_peak: Option<f64>,
    ) -> LibraryResult<()> {
        self.write_source_loudness(
            "track",
            track_object_id,
            integrated_lufs,
            true_peak,
            replay_gain_db,
            replay_gain_peak,
        )
        .await
    }

    pub async fn write_album_source_loudness(
        &mut self,
        album_object_id: &str,
        integrated_lufs: Option<f64>,
        true_peak: Option<f64>,
        replay_gain_db: Option<f64>,
        replay_gain_peak: Option<f64>,
    ) -> LibraryResult<()> {
        self.write_source_loudness(
            "album",
            album_object_id,
            integrated_lufs,
            true_peak,
            replay_gain_db,
            replay_gain_peak,
        )
        .await
    }

    /// Full artist records supply their effective sort name, including the display-name fallback.
    /// Sparse credits leave it absent so they retain the existing artist sort name.
    pub async fn write_artist(
        &mut self,
        object_id: &str,
        name: &str,
        normalized_name: &str,
        sort_text: Option<&str>,
        musicbrainz_artist_id: Option<&str>,
        artwork_binding: Option<&[u8]>,
        favorite: Option<bool>,
        rating: Option<i64>,
    ) -> LibraryResult<()> {
        self.require_id("artist", object_id)?;
        let media_uri = source_entity_uri(&SourceId::new(&self.source_id), "artist", object_id);
        let rating = rating
            .filter(|rating| (0..=10).contains(rating))
            .map(|rating| rating * 10);
        self.require_row_bytes(&[
            object_id.as_bytes(),
            name.as_bytes(),
            normalized_name.as_bytes(),
            sort_text.unwrap_or_default().as_bytes(),
            musicbrainz_artist_id.unwrap_or_default().as_bytes(),
            artwork_binding.unwrap_or_default(),
        ])?;
        let first = self.observe("artist", object_id).await?;
        let changed = self.execute_result(sqlx::query(
            "INSERT INTO artists(source_key,object_id,media_uri,name,normalized_name,sort_text,
                musicbrainz_artist_id)
             SELECT ?7,?1,?2,
                CASE WHEN ?5 IS NULL AND previous.normalized_name=?4 THEN previous.name ELSE ?3 END,
                ?4,COALESCE(?5,previous.sort_text,?4),
                CASE WHEN ?5 IS NOT NULL THEN ?6 ELSE COALESCE(?6,previous.musicbrainz_artist_id) END
             FROM (SELECT 1) LEFT JOIN artists previous
                ON previous.source_key=?7 AND previous.object_id=?1 WHERE true
             ON CONFLICT(source_key,object_id) DO UPDATE SET
                media_uri=excluded.media_uri,name=excluded.name,normalized_name=excluded.normalized_name,
                sort_text=excluded.sort_text,musicbrainz_artist_id=excluded.musicbrainz_artist_id
             WHERE (artists.media_uri,artists.name,artists.normalized_name,artists.sort_text,
                artists.musicbrainz_artist_id)
             IS NOT (excluded.media_uri,excluded.name,excluded.normalized_name,excluded.sort_text,
                excluded.musicbrainz_artist_id)")
            .bind(object_id).bind(media_uri).bind(name).bind(normalized_name).bind(sort_text)
            .bind(musicbrainz_artist_id).bind(self.source))
            .await?.rows_affected() > 0;
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
                .bind(favorite)
                .bind(rating)
                .bind(sort_text.is_some())
                .bind(self.source),
            )
            .await?
            .rows_affected()
            > 0;
        if first && favorite.is_none() {
            self.execute(
                sqlx::query("INSERT OR IGNORE INTO temp.scan_unresolved_artists VALUES(?1)")
                    .bind(object_id),
            )
            .await?;
        } else if favorite.is_some() {
            self.execute(
                sqlx::query("DELETE FROM temp.scan_unresolved_artists WHERE object_id=?1")
                    .bind(object_id),
            )
            .await?;
        }
        self.remember_artwork("artist", object_id, artwork_binding, first)
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

    pub async fn write_genre(
        &mut self,
        object_id: &str,
        name: &str,
        normalized_name: &str,
        sort_text: Option<&str>,
        artwork_binding: Option<&[u8]>,
    ) -> LibraryResult<()> {
        self.require_id("genre", object_id)?;
        self.require_row_bytes(&[
            object_id.as_bytes(),
            name.as_bytes(),
            normalized_name.as_bytes(),
            sort_text.unwrap_or_default().as_bytes(),
            artwork_binding.unwrap_or_default(),
        ])?;
        let first = self.observe("genre", object_id).await?;
        let changed = self.execute_result(sqlx::query(
            "INSERT INTO genres(source_key,object_id,name,normalized_name,sort_text)
             SELECT ?5,?1,CASE WHEN ?4 IS NULL AND previous.normalized_name=?3 THEN previous.name ELSE ?2 END,
                ?3,COALESCE(?4,previous.sort_text,?3)
             FROM (SELECT 1) LEFT JOIN genres previous
                ON previous.source_key=?5 AND previous.object_id=?1 WHERE true
             ON CONFLICT(source_key,object_id) DO UPDATE SET
                name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text
             WHERE (genres.name,genres.normalized_name,genres.sort_text)
                IS NOT (excluded.name,excluded.normalized_name,excluded.sort_text)")
            .bind(object_id).bind(name).bind(normalized_name).bind(sort_text).bind(self.source))
            .await?.rows_affected() > 0;
        self.changes.metadata |= changed;
        self.remember_artwork("genre", object_id, artwork_binding, first)
            .await
    }

    pub async fn write_mood(
        &mut self,
        object_id: &str,
        name: &str,
        normalized_name: &str,
        sort_text: &str,
    ) -> LibraryResult<()> {
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
            .bind(object_id).bind(name).bind(normalized_name).bind(sort_text).bind(self.source))
            .await?.rows_affected() > 0;
        Ok(())
    }

    pub async fn write_folder(
        &mut self,
        object_id: &str,
        name: &str,
        normalized_name: &str,
        sort_text: &str,
        artwork_binding: Option<&[u8]>,
    ) -> LibraryResult<()> {
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
            .bind(object_id).bind(name).bind(normalized_name).bind(sort_text).bind(self.source))
            .await?.rows_affected() > 0;
        self.remember_artwork("folder", object_id, artwork_binding, first)
            .await
    }

    /// A membership reference must not replace the section's advertised display name.
    pub async fn write_folder_reference(&mut self, object_id: &str) -> LibraryResult<()> {
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

    async fn write_source_loudness(
        &mut self,
        kind: &'static str,
        object_id: &str,
        integrated_lufs: Option<f64>,
        true_peak: Option<f64>,
        replay_gain_db: Option<f64>,
        replay_gain_peak: Option<f64>,
    ) -> LibraryResult<()> {
        self.require_id("loudness entity", object_id)?;
        if (integrated_lufs.is_none() && replay_gain_db.is_none())
            || integrated_lufs.is_some_and(|value| !value.is_finite())
            || true_peak.is_some_and(|value| !value.is_finite() || value < 0.0)
            || replay_gain_db.is_some_and(|value| !value.is_finite())
            || replay_gain_peak.is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "invalid source loudness fact".to_string(),
            ));
        }
        self.execute(sqlx::query(
            "INSERT INTO temp.scan_loudness(kind,object_id,integrated_lufs,true_peak,replay_gain_db,replay_gain_peak)
             VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(kind,object_id) DO UPDATE SET
                integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak,
                replay_gain_db=excluded.replay_gain_db,replay_gain_peak=excluded.replay_gain_peak")
            .bind(kind).bind(object_id).bind(integrated_lufs).bind(true_peak)
            .bind(replay_gain_db).bind(replay_gain_peak)).await
    }
}

// Clear absent fields only after every album record has supplied its facts.
pub(super) async fn finish_albums(
    connection: &mut sqlx::SqliteConnection,
    source: i64,
) -> LibraryResult<(bool, bool)> {
    let metadata = sqlx::query(
        "UPDATE albums AS album SET
            year=CASE WHEN seen.supplied & 1 THEN album.year END,
            release_date=CASE WHEN seen.supplied & 2 THEN album.release_date END,
            date_added=CASE WHEN seen.supplied & 4 THEN album.date_added END,
            musicbrainz_release_id=CASE WHEN seen.supplied & 8 THEN album.musicbrainz_release_id END,
            musicbrainz_release_group_id=CASE WHEN seen.supplied & 16 THEN album.musicbrainz_release_group_id END,
            is_compilation=CASE WHEN seen.supplied & 32 THEN 1 WHEN seen.supplied & 64 THEN 0 END
         FROM temp.scan_seen seen
         WHERE album.source_key=?1 AND seen.kind='album' AND seen.object_id=album.object_id AND seen.written
           AND (album.year,album.release_date,album.date_added,album.musicbrainz_release_id,
                album.musicbrainz_release_group_id,album.is_compilation)
           IS NOT (CASE WHEN seen.supplied & 1 THEN album.year END,
                CASE WHEN seen.supplied & 2 THEN album.release_date END,
                CASE WHEN seen.supplied & 4 THEN album.date_added END,
                CASE WHEN seen.supplied & 8 THEN album.musicbrainz_release_id END,
                CASE WHEN seen.supplied & 16 THEN album.musicbrainz_release_group_id END,
                CASE WHEN seen.supplied & 32 THEN 1 WHEN seen.supplied & 64 THEN 0 END)")
        .bind(source).execute(&mut *connection).await?.rows_affected() > 0;
    let user_data = sqlx::query(
        "UPDATE albums AS album SET source_rating=NULL FROM temp.scan_seen seen
         WHERE album.source_key=?1 AND seen.kind='album' AND seen.object_id=album.object_id
           AND seen.written AND seen.supplied & 128=0 AND album.source_rating IS NOT NULL",
    )
    .bind(source)
    .execute(connection)
    .await?
    .rows_affected()
        > 0;
    Ok((metadata, user_data))
}
