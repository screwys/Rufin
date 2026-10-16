//! Owns one bounded lyrics cache row keyed by media URI, authority, role, language, and script.
//! Fetching and provider policy remain outside Library.

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{Connection, FromRow, Row};

use crate::{Database, LibraryError, LibraryResult, ReadCancellation};

const LYRICS_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LyricsCacheRow {
    pub media_uri: String,
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
    media_uri: String,
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
            media_uri: value.media_uri,
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
    pub async fn clear_lyrics_cache(&self, active_media_uris: &[String]) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("DELETE FROM lyrics_cache WHERE NOT EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=lyrics_cache.media_uri AND locator.origin='download') AND media_uri NOT IN (SELECT value FROM json_each(?1))")
            .bind(serde_json::to_string(active_media_uris)?)
            .execute(connection).await?;
        Ok(())
    }

    pub async fn lyrics_cache_revision(&self) -> LibraryResult<i64> {
        let mut connection = self.acquire_reader().await?;
        Ok(
            sqlx::query_scalar("SELECT revision FROM lyrics_cache_revision WHERE singleton=1")
                .fetch_one(&mut *connection)
                .await?,
        )
    }

    pub async fn lyrics_cache_changes(
        &self,
        after: (i64, i64),
        through: i64,
    ) -> LibraryResult<Vec<((i64, i64), LyricsCacheRow)>> {
        let (_export, mut connection) = self.acquire_export().await?;
        let mut rows = sqlx::query("SELECT cache_revision,rowid,media_uri,authority,role,language,script,cache_input_digest,lyrics,updated_at FROM lyrics_cache AS cache WHERE (cache_revision,rowid)>(?1,?2) AND cache_revision<=?3 AND (authority<>'source' OR NOT EXISTS(SELECT 1 FROM lyrics_cache newer WHERE newer.media_uri=cache.media_uri AND newer.authority='source' AND newer.cache_input_digest=cache.cache_input_digest AND (newer.updated_at,newer.role='original',newer.rowid)>(cache.updated_at,cache.role='original',cache.rowid))) ORDER BY cache_revision,rowid LIMIT 128")
            .bind(after.0).bind(after.1).bind(through).fetch(&mut *connection);
        let mut page = Vec::new();
        let mut bytes = 0;
        while let Some(row) = rows.try_next().await? {
            let revision = (row.get(0), row.get(1));
            let row: LyricsCacheRow = LyricsScalar::from_row(&row)?.try_into()?;
            bytes += row.lyrics.len();
            page.push((revision, row));
            if bytes >= 1024 * 1024 {
                break;
            }
        }
        Ok(page)
    }

    pub async fn import_lyrics_cache(
        &self,
        rows: Vec<LyricsCacheRow>,
        replace_stale_source: bool,
    ) -> LibraryResult<Vec<LyricsCacheRow>> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let mut imported = Vec::new();
        for row in rows {
            if row.lyrics.len() > LYRICS_PAYLOAD_BYTES {
                return Err(LibraryError::InvalidRequest(
                    "lyrics cache row is too large".into(),
                ));
            }
            if sqlx::query("INSERT INTO lyrics_cache(media_uri,authority,role,language,script,cache_input_digest,lyrics,updated_at,cache_revision) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,-1) ON CONFLICT(media_uri,authority,role,language,script) DO UPDATE SET cache_input_digest=excluded.cache_input_digest,lyrics=excluded.lyrics,updated_at=excluded.updated_at,cache_revision=-1 WHERE ?9 AND lyrics_cache.authority='source' AND lyrics_cache.cache_input_digest<>excluded.cache_input_digest")
                .bind(&row.media_uri).bind(&row.authority).bind(&row.role).bind(&row.language).bind(&row.script)
                .bind(row.cache_input_digest.as_slice()).bind(&row.lyrics).bind(row.updated_at).bind(replace_stale_source)
                .execute(&mut *transaction).await?.rows_affected()!=0
            { imported.push(row); }
        }
        transaction.commit().await?;
        Ok(imported)
    }

    pub async fn lyrics_cache_for_role(
        &self,
        media_uri: &str,
        role: &str,
        language: &str,
        script: &str,
        cache_input_digest: [u8; 32],
        authority: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<LyricsCacheRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, LyricsScalar>(
            "SELECT media_uri,authority,role,language,script,cache_input_digest,lyrics,updated_at
             FROM lyrics_cache WHERE media_uri=?1
               AND (?6='source' OR (role=?2 AND language=?3 AND script=?4))
               AND (authority='external' OR cache_input_digest=?5)
               AND authority=?6 ORDER BY updated_at DESC,role='original' DESC LIMIT 1",
        )
        .bind(media_uri)
        .bind(role)
        .bind(language)
        .bind(script)
        .bind(cache_input_digest.as_slice())
        .bind(authority)
        .fetch_optional(&mut *connection)
        .await;
        result?.map(TryInto::try_into).transpose()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn write_lyrics_cache(
        &self,
        media_uri: &str,
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
        sqlx::query("INSERT INTO lyrics_cache(media_uri,authority,role,language,script,cache_input_digest,lyrics,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(media_uri,authority,role,language,script) DO UPDATE SET cache_input_digest=excluded.cache_input_digest,lyrics=excluded.lyrics,updated_at=excluded.updated_at,cache_revision=max(lyrics_cache.cache_revision,0)")
            .bind(media_uri).bind(authority).bind(role).bind(language).bind(script)
            .bind(cache_input_digest.as_slice()).bind(lyrics).bind(updated_at).execute(connection).await?;
        drop(writer);
        self.lyrics_cache_written();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn remove_lyrics_cache(
        &self,
        media_uri: &str,
        authority: &str,
        role: &str,
        language: &str,
        script: &str,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query("DELETE FROM lyrics_cache WHERE media_uri=?1 AND authority=?2 AND role=?3 AND language=?4 AND script=?5")
            .bind(media_uri).bind(authority).bind(role).bind(language).bind(script)
            .execute(connection).await?.rows_affected()==1)
    }

    pub async fn remove_track_lyrics_by_authority(
        &self,
        media_uri: &str,
        authority: &str,
    ) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(
            sqlx::query("DELETE FROM lyrics_cache WHERE media_uri=?1 AND authority=?2")
                .bind(media_uri)
                .bind(authority)
                .execute(connection)
                .await?
                .rows_affected(),
        )
    }
}
