//! What playing a track needs, read by key in one statement: where its audio is, its CUE
//! window, its gains and what writing its lyrics needs.

use sqlx::Row as _;

use crate::{
    Catalog, LibraryResult, LocalLocatorWrite, LoudnessMeasurement, NativeGain, SourceId,
    SourceKey, TrackKey,
};

#[derive(Clone, Debug)]
pub struct Playable {
    pub track: TrackKey,
    pub source: Option<SourceKey>,
    pub source_id: Option<SourceId>,
    pub source_kind: Option<String>,
    pub media_uri: String,
    pub object_id: Option<String>,
    pub format: Option<String>,
    pub source_path: Option<String>,
    pub cue_path: Option<String>,
    /// A downloaded copy, the asked quality first.
    pub download: Option<String>,
    /// A local locator, the mapping first.
    pub locator: Option<LocalLocatorWrite>,
    /// The file row of the backing audio.
    pub file: Option<PlayableFile>,
    /// The CUE window; None for a one-track CUE, which plays whole.
    pub window: Option<(u64, u64)>,
    pub audio_hash: Option<[u8; 32]>,
    pub channels: Option<u32>,
    pub gains: Gains,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayableFile {
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub size_bytes: Option<i64>,
    pub revision: Option<String>,
}

impl Playable {
    /// The file a local locator or a Local source's file row names, as a URI GStreamer opens.
    pub fn local_uri(&self) -> Option<String> {
        if let Some(locator) = &self.locator {
            return Some(locator.access_uri.clone());
        }
        let file = self
            .file
            .as_ref()
            .filter(|_| self.source_kind.as_deref() == Some("local"))?;
        crate::document_access_uri(&file.path)
            .or_else(|| file.path.starts_with("content:").then(|| file.path.clone()))
            .or_else(|| url::Url::from_file_path(&file.path).ok().map(String::from))
    }
}

/// The gains the source reported and the ones analysis measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gains {
    pub track: Option<NativeGain>,
    /// The track's album tag, else the first member's.
    pub album: Option<NativeGain>,
    pub analysed: Option<LoudnessMeasurement>,
    pub album_analysed: Option<LoudnessMeasurement>,
}

impl Gains {
    /// The track's gain: the source's, else the analysed one.
    pub fn track_loudness(&self, hash: Option<[u8; 32]>) -> Option<LoudnessMeasurement> {
        self.track
            .as_ref()
            .map(|gain| measurement(gain, hash))
            .or_else(|| self.analysed.clone())
    }

    pub fn album_loudness(&self, hash: Option<[u8; 32]>) -> Option<LoudnessMeasurement> {
        self.album
            .as_ref()
            .map(|gain| measurement(gain, hash))
            .or_else(|| self.album_analysed.clone())
    }
}

fn measurement(gain: &NativeGain, hash: Option<[u8; 32]>) -> LoudnessMeasurement {
    LoudnessMeasurement {
        analysis_key: hash.unwrap_or_default(),
        integrated_lufs: gain.integrated_lufs,
        true_peak: gain.true_peak,
        replay_gain_db: gain.replay_gain_db,
        replay_gain_peak: gain.replay_gain_peak,
    }
}

impl Catalog {
    /// One reader statement by key.
    pub async fn playable(
        &self,
        track: TrackKey,
        quality: &str,
    ) -> LibraryResult<Option<Playable>> {
        let whole_file = crate::local::whole_file_cue_sql("track", "details");
        let gained = |path: &str| {
            format!(
                "json_type({path})='object' AND(json_extract({path},'$.integrated_lufs') IS NOT NULL OR json_extract({path},'$.replay_gain_db') IS NOT NULL)"
            )
        };
        let own = gained("details.metadata,'$.album_loudness'");
        let member = gained("member_details.metadata,'$.album_loudness'");
        let sql = format!(
            "SELECT track.source_key,track.object_id,track.media_uri,track.source_format,details.source_path,details.cue_path,details.cue_start_millis,
                CASE WHEN {whole_file} THEN NULL ELSE details.cue_end_millis END,
                file.path,file.root,file.relative_path,json_array(file.size_bytes,file.revision),
                (SELECT path FROM downloads WHERE media_uri=track.media_uri ORDER BY quality=?2 DESC,rowid LIMIT 1),
                (SELECT json_array(origin,path,root,relative_path,access_uri) FROM local_locators WHERE media_uri=track.media_uri
                    ORDER BY CASE origin WHEN 'mapping' THEN 0 ELSE 1 END,local_access_file_key LIMIT 1),
                details.audio_hash,
                json_extract(details.metadata,'$.audio_properties.channels'),
                CASE WHEN json_type(details.metadata,'$.source_loudness')='object' THEN json(json_extract(details.metadata,'$.source_loudness')) END,
                CASE WHEN {own} THEN json(json_extract(details.metadata,'$.album_loudness')) ELSE (
                    SELECT json(json_extract(member_details.metadata,'$.album_loudness')) FROM tracks member JOIN track_details member_details USING(track_key)
                    WHERE member.album_key=track.album_key AND member.indexed=1 AND {member}
                    ORDER BY coalesce(member_details.cue_path,member_details.source_path),member.disc_number,member.track_number,member.object_id LIMIT 1) END,
                measured.integrated_lufs,measured.true_peak,album.integrated_lufs,album.true_peak
             FROM tracks track LEFT JOIN track_details details USING(track_key)
             LEFT JOIN files file ON file.source_key=track.source_key AND file.path=details.source_path
             LEFT JOIN loudness measured ON measured.audio_hash=details.audio_hash
             LEFT JOIN album_loudness album ON album.members=(
                SELECT CASE WHEN count(*)=count(member_details.audio_hash) THEN group_concat(substr(hex(member_details.audio_hash),1,16),'' ORDER BY member_details.audio_hash) END
                FROM tracks member JOIN track_details member_details USING(track_key) WHERE member.album_key=track.album_key AND member.indexed=1)
             WHERE track.track_key=?1"
        );
        let mut connection = self.acquire_reader().await?;
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(track)
            .bind(quality)
            .fetch_optional(&mut *connection)
            .await?;
        drop(connection);
        let Some(row) = row else {
            return Ok(None);
        };
        let source: Option<i64> = row.try_get(0)?;
        let object_id: Option<String> = row.try_get(1)?;
        let media_uri: String = row.try_get(2)?;
        let format: Option<String> = row.try_get(3)?;
        let source_path: Option<String> = row.try_get(4)?;
        let cue_path: Option<String> = row.try_get(5)?;
        let start: Option<i64> = row.try_get(6)?;
        let end: Option<i64> = row.try_get(7)?;
        let path: Option<String> = row.try_get(8)?;
        let root: Option<String> = row.try_get(9)?;
        let relative_path: Option<String> = row.try_get(10)?;
        let revision: Option<String> = row.try_get(11)?;
        let download: Option<String> = row.try_get(12)?;
        let locator: Option<String> = row.try_get(13)?;
        let hash: Option<Vec<u8>> = row.try_get(14)?;
        let channels: Option<i64> = row.try_get(15)?;
        let track_gain: Option<String> = row.try_get(16)?;
        let album_gain: Option<String> = row.try_get(17)?;
        let lufs: Option<f64> = row.try_get(18)?;
        let peak: Option<f64> = row.try_get(19)?;
        let album_lufs: Option<f64> = row.try_get(20)?;
        let album_peak: Option<f64> = row.try_get(21)?;
        let source = source.map(SourceKey::from_raw);
        let (source_id, source_kind) = source
            .and_then(|source| {
                let snapshot = self.snapshot();
                let source = snapshot.source(source)?;
                Some((SourceId::new(source.object.clone()), source.kind.clone()))
            })
            .unzip();
        let locator = locator
            .map(|locator| {
                serde_json::from_str::<(String, String, String, String, String)>(&locator)
            })
            .transpose()?
            .map(
                |(origin, path, root, relative_path, access_uri)| LocalLocatorWrite {
                    source_id: source_id.as_ref().map(|id| id.as_str().to_owned()),
                    media_uri: media_uri.clone(),
                    origin,
                    path,
                    root,
                    relative_path,
                    access_uri,
                },
            );
        let audio_hash = hash.and_then(|hash| hash.try_into().ok());
        let parse = |json: Option<String>| {
            json.map(|json| serde_json::from_str::<NativeGain>(&json))
                .transpose()
        };
        let analysed = |lufs: Option<f64>, peak: Option<f64>| {
            (lufs.is_some() || peak.is_some()).then(|| LoudnessMeasurement {
                analysis_key: audio_hash.unwrap_or_default(),
                integrated_lufs: lufs,
                true_peak: peak,
                replay_gain_db: None,
                replay_gain_peak: None,
            })
        };
        let window = match (start, end) {
            (Some(0), None) => None,
            (Some(start), Some(end)) if start >= 0 && end > start => {
                Some((start as u64, end as u64))
            }
            _ => crate::cue_media_parts(&media_uri)
                .map(|(_, _, start, end)| (start as u64, end as u64)),
        };
        Ok(Some(Playable {
            track,
            source,
            source_id,
            source_kind: source_kind.flatten(),
            media_uri,
            object_id,
            format,
            source_path,
            cue_path,
            download,
            locator,
            file: path
                .zip(root)
                .zip(relative_path)
                .map(|((path, root), relative_path)| {
                    let (size_bytes, revision) = revision
                        .and_then(|facts| serde_json::from_str(&facts).ok())
                        .unwrap_or_default();
                    PlayableFile {
                        path,
                        root,
                        relative_path,
                        size_bytes,
                        revision,
                    }
                }),
            window,
            audio_hash,
            channels: channels.and_then(|channels| u32::try_from(channels).ok()),
            gains: Gains {
                track: parse(track_gain)?,
                album: parse(album_gain)?,
                analysed: analysed(lufs, peak),
                album_analysed: analysed(album_lufs, album_peak),
            },
        }))
    }
}
