//! Ordinary cache files are shared on demand.
use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum CacheRequest {
    Lyrics { query: LyricsCacheQuery },
    MediaAssets { uri: String },
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct LyricsCacheQuery {
    pub media_uri: String,
    pub authority: String,
    pub role: String,
    pub language: String,
    pub script: String,
    pub input: [u8; 32],
}

impl LyricsCacheQuery {
    fn accepts(&self, row: &library::LyricsCacheRow) -> bool {
        row.media_uri == self.media_uri
            && row.authority == self.authority
            && if self.authority == "source" {
                row.cache_input_digest == self.input
            } else {
                row.role == self.role && row.language == self.language && row.script == self.script
            }
    }
}

impl ConnectOwner {
    pub(super) async fn answer_artwork_stream(
        &self,
        query: artwork::CacheImageRequest,
        offset: u64,
        validator: Option<&str>,
    ) -> Result<rufin_connect::media::PeerByteResponse, String> {
        use rufin_connect::media::{PeerByteResponse, open_file};
        let Some(entry) = self
            .source
            .shared
            .artwork
            .cached_family_file(&query)
            .map_err(error)?
        else {
            return Ok(PeerByteResponse {
                metadata: serde_json::Value::Null,
                reader: Box::pin(std::io::Cursor::new(Vec::new())),
            });
        };
        let (metadata, reader) = open_file(&entry.path, offset, validator)
            .await
            .map_err(error)?;
        let mut metadata = serde_json::to_value(metadata).map_err(error)?;
        metadata["key"] = serde_json::Value::String(entry.key);
        Ok(PeerByteResponse {
            metadata,
            reader: Box::pin(reader),
        })
    }

    pub(super) async fn answer_cache(
        &self,
        request: CacheRequest,
    ) -> Result<serde_json::Value, String> {
        match request {
            CacheRequest::Lyrics { query } => {
                let row = self
                    .database
                    .lyrics_cache_for_role(
                        &query.media_uri,
                        &query.role,
                        &query.language,
                        &query.script,
                        query.input,
                        &query.authority,
                        &library::ReadCancellation::new(),
                    )
                    .await
                    .map_err(error)?;
                serde_json::to_value(row).map_err(error)
            }
            CacheRequest::MediaAssets { uri } => {
                serde_json::to_value(self.media_assets(&uri).await?).map_err(error)
            }
        }
    }

    pub(super) async fn fetch_cached_artwork(
        &self,
        query: artwork::CacheImageRequest,
    ) -> Option<(String, tempfile::TempPath)> {
        let settings = self.status().settings;
        if !settings.enabled {
            return None;
        }
        let session = self.active().await.ok()?;
        session
            .stop
            .run_until_cancelled(Box::pin(async {
                for peer in self
                    .status()
                    .devices
                    .into_iter()
                    .filter(|peer| peer.reachable && peer.enrolled)
                {
                    let result = async {
                        use rufin_connect::media::{FileMetadata, receive_file};
                        let network = self.connected_network().await?;
                        let request = |offset, validator| {
                            serde_json::to_vec(&super::media::ByteRequest::Artwork {
                                family: query.family.clone(),
                                size: match query.size {
                                    sources::ImageSize::Original => None,
                                    sources::ImageSize::Thumbnail(size) => Some(size),
                                },
                                offset,
                                validator,
                            })
                            .map_err(error)
                        };
                        let mut response = network
                            .stream_request(&peer.id, request(0, None)?)
                            .await
                            .map_err(error)?;
                        if response.metadata.is_null() {
                            return Ok(None);
                        }
                        let key = response.metadata["key"]
                            .as_str()
                            .filter(|key| query.select_key([*key]).is_some())
                            .ok_or("The peer returned a different artwork entry")?
                            .to_owned();
                        let mut metadata: FileMetadata =
                            serde_json::from_value(response.metadata).map_err(error)?;
                        let identity = format!("{}:{key}:{:?}", peer.id, metadata.validator);
                        let _transfer = network.file_transfer(&identity).await;
                        let partial = self.directory.join(format!(
                            "cache-transfer-{}.partial",
                            blake3::hash(identity.as_bytes()).to_hex()
                        ));
                        let offset = tokio::fs::metadata(&partial)
                            .await
                            .map(|file| file.len())
                            .unwrap_or_default()
                            .min(metadata.length);
                        if offset > 0 && metadata.validator.is_some() {
                            response = network
                                .stream_request(
                                    &peer.id,
                                    request(offset, metadata.validator.clone())?,
                                )
                                .await
                                .map_err(error)?;
                            if response.metadata["key"].as_str() != Some(&key) {
                                return Err("The peer's artwork entry changed".into());
                            }
                            metadata = serde_json::from_value(response.metadata).map_err(error)?;
                        }
                        receive_file(
                            response.reader,
                            &metadata,
                            &partial,
                            session.stop.clone(),
                            tokio::sync::watch::channel(0).0,
                        )
                        .await
                        .map_err(error)?;
                        let output = tempfile::NamedTempFile::new_in(&self.directory)
                            .map_err(error)?
                            .into_temp_path();
                        tokio::fs::rename(&partial, &output).await.map_err(error)?;
                        Ok::<_, String>(Some((key, output)))
                    }
                    .await;
                    match result {
                        Ok(Some(image)) => return Some(image),
                        Ok(None) => {}
                        Err(error) => {
                            tracing::debug!(%error, "Could not read a peer's cached artwork")
                        }
                    }
                }
                None
            }))
            .await
            .flatten()
    }

    pub(crate) async fn fetch_cached_lyrics(
        &self,
        query: LyricsCacheQuery,
    ) -> Option<library::LyricsCacheRow> {
        let settings = self.status().settings;
        if !settings.enabled {
            return None;
        }
        let session = self.active().await.ok()?;
        session
            .stop
            .run_until_cancelled(async {
                let mut found = None;
                for peer in self
                    .status()
                    .devices
                    .into_iter()
                    .filter(|peer| peer.reachable && peer.enrolled)
                {
                    let result = self
                        .request(
                            &peer.id,
                            Request::CacheFile {
                                asset: CacheRequest::Lyrics {
                                    query: query.clone(),
                                },
                            },
                        )
                        .await
                        .and_then(|value| {
                            serde_json::from_value::<Option<library::LyricsCacheRow>>(value)
                                .map_err(error)
                        });
                    match result {
                        Ok(Some(row)) if query.accepts(&row) => {
                            found = Some(row);
                            break;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            tracing::debug!(%error, "Could not read a peer's cached lyrics")
                        }
                    }
                }
                let row = found?;
                if let Err(error) = self
                    .database
                    .import_lyrics_cache(vec![row.clone()], true)
                    .await
                {
                    tracing::warn!(%error, "Could not save shared lyrics");
                }
                Some(row)
            })
            .await
            .flatten()
    }
}
