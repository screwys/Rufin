//! Owns one bounded lyrics cache row keyed by Track, authority, role, language, and script.
//! Fetching and provider policy remain outside Library.

use sqlx::FromRow;

use crate::{Database, LibraryError, LibraryResult, ReadCancellation, SourceKey, TrackKey};

const LYRICS_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct LyricsCacheRow {
    pub authority: String,
    pub role: String,
    pub language: String,
    pub script: String,
    pub cache_input_digest: [u8; 32],
    pub lyrics: String,
    pub updated_at: i64,
}

#[derive(FromRow)]
struct LyricsScalar {
    authority: String,
    role: String,
    language: String,
    script: String,
    cache_input_digest: Vec<u8>,
    lyrics: String,
    updated_at: i64,
}

impl TryFrom<LyricsScalar> for LyricsCacheRow {
    type Error = LibraryError;
    fn try_from(value: LyricsScalar) -> Result<Self, Self::Error> {
        Ok(Self {
            authority: value.authority,
            role: value.role,
            language: value.language,
            script: value.script,
            cache_input_digest: value.cache_input_digest.try_into().map_err(|_| {
                LibraryError::InvalidStore("lyrics input digest is not 32 bytes".to_string())
            })?,
            lyrics: value.lyrics,
            updated_at: value.updated_at,
        })
    }
}

impl Database {
    pub async fn lyrics_cache_for_role(
        &self,
        source: SourceKey,
        track: TrackKey,
        role: &str,
        language: &str,
        script: &str,
        cache_input_digest: [u8; 32],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<LyricsCacheRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, LyricsScalar>(
            "SELECT authority,role,language,script,cache_input_digest,lyrics,updated_at
             FROM lyrics_cache WHERE source_key=?1 AND track_key=?2 AND role=?3
               AND language=?4 AND script=?5 AND cache_input_digest=?6
             ORDER BY CASE authority WHEN 'source' THEN 0 ELSE 1 END,updated_at DESC LIMIT 1",
        )
        .bind(source)
        .bind(track)
        .bind(role)
        .bind(language)
        .bind(script)
        .bind(cache_input_digest.as_slice())
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        result?.map(TryInto::try_into).transpose()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn write_lyrics_cache(
        &self,
        source: SourceKey,
        track: TrackKey,
        authority: &str,
        role: &str,
        language: &str,
        script: &str,
        cache_input_digest: [u8; 32],
        lyrics: &str,
        updated_at: i64,
    ) -> LibraryResult<()> {
        if authority.is_empty()
            || role.is_empty()
            || updated_at < 0
            || lyrics.len() > LYRICS_PAYLOAD_BYTES
        {
            return Err(LibraryError::InvalidRequest(
                "invalid lyrics cache row".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("INSERT INTO lyrics_cache(source_key,track_key,authority,role,language,script,cache_input_digest,lyrics,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(source_key,track_key,authority,role,language,script) DO UPDATE SET cache_input_digest=excluded.cache_input_digest,lyrics=excluded.lyrics,updated_at=excluded.updated_at")
            .bind(source).bind(track).bind(authority).bind(role).bind(language).bind(script)
            .bind(cache_input_digest.as_slice()).bind(lyrics).bind(updated_at).execute(connection).await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn remove_lyrics_cache(
        &self,
        source: SourceKey,
        track: TrackKey,
        authority: &str,
        role: &str,
        language: &str,
        script: &str,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query("DELETE FROM lyrics_cache WHERE source_key=?1 AND track_key=?2 AND authority=?3 AND role=?4 AND language=?5 AND script=?6")
            .bind(source).bind(track).bind(authority).bind(role).bind(language).bind(script)
            .execute(connection).await?.rows_affected()==1)
    }

    pub async fn remove_track_lyrics_by_authority(
        &self,
        source: SourceKey,
        track: TrackKey,
        authority: &str,
    ) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query(
            "DELETE FROM lyrics_cache WHERE source_key=?1 AND track_key=?2 AND authority=?3",
        )
        .bind(source)
        .bind(track)
        .bind(authority)
        .execute(connection)
        .await?
        .rows_affected())
    }
}
