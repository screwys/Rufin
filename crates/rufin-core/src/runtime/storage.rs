use super::ProductHandles;
pub use library::CacheUsage;

#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheKind {
    Waveforms,
    Loudness,
    Fingerprints,
    Artwork,
    Lyrics,
    JapaneseDictionary,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone, Debug)]
pub struct CacheSummary {
    pub kind: CacheKind,
    pub usage: Option<CacheUsage>,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone, Debug)]
pub struct StorageSummary {
    pub caches: Vec<CacheSummary>,
    pub total_bytes: Option<u64>,
    pub errors: Vec<String>,
}

impl ProductHandles {
    pub async fn storage_summary(&self, clear: Option<CacheKind>) -> StorageSummary {
        let mut summary = StorageSummary {
            caches: Vec::new(),
            total_bytes: Some(0),
            errors: Vec::new(),
        };
        if let Some(kind) = clear
            && let Err(error) = self.clear_cached_data(kind).await
        {
            summary.errors.push(error);
        }
        for kind in [
            CacheKind::Waveforms,
            CacheKind::Loudness,
            CacheKind::Fingerprints,
            CacheKind::Artwork,
            CacheKind::Lyrics,
            CacheKind::JapaneseDictionary,
        ] {
            let usage = match self.cache_usage(kind).await {
                Ok(usage) => {
                    summary.total_bytes = summary.total_bytes.map(|total| total + usage.bytes);
                    Some(usage)
                }
                Err(error) => {
                    summary.total_bytes = None;
                    summary.errors.push(error);
                    None
                }
            };
            summary.caches.push(CacheSummary { kind, usage });
        }
        summary
    }

    pub fn prepare_storage_reset(
        &self,
        reset: crate::settings::StorageReset,
    ) -> Result<crate::settings::StorageResetRequest, String> {
        self.settings
            .prepare_storage_reset(reset, self.connect.status().settings)
    }

    pub async fn close_storage(&self) -> Result<(), String> {
        self.connect.close_storage().await?;
        self.source.close_storage().await;
        self.downloads.shutdown().await?;
        self.library
            .as_ref()
            .clone()
            .close()
            .await
            .map_err(|error| error.to_string())
    }

    async fn cache_usage(&self, kind: CacheKind) -> Result<CacheUsage, String> {
        match kind {
            CacheKind::Fingerprints => self
                .library
                .fingerprint_cache_usage()
                .await
                .map_err(|error| error.to_string()),
            CacheKind::JapaneseDictionary => self.lyrics.dictionary_cache_usage().await,
            CacheKind::Waveforms => self.playback.waveform_cache_usage().await,
            CacheKind::Loudness => self
                .library
                .loudness_cache_usage()
                .await
                .map_err(|error| error.to_string()),
            CacheKind::Lyrics => self
                .library
                .lyrics_cache_usage()
                .await
                .map_err(|error| error.to_string()),
            CacheKind::Artwork => self
                .artwork
                .cache_usage()
                .await
                .map_err(|error| error.to_string()),
        }
    }

    async fn clear_cached_data(&self, kind: CacheKind) -> Result<(), String> {
        match kind {
            CacheKind::Fingerprints => self
                .library
                .clear_fingerprint_cache()
                .await
                .map_err(|error| error.to_string()),
            CacheKind::JapaneseDictionary => self.lyrics.clear_dictionary_cache().await,
            CacheKind::Waveforms => self.playback.clear_waveform_cache().await,
            CacheKind::Loudness => self
                .library
                .clear_loudness_cache()
                .await
                .map_err(|error| error.to_string()),
            CacheKind::Artwork => self
                .downloads
                .clear_artwork_cache()
                .await
                .map_err(|error| error.to_string()),
            CacheKind::Lyrics => self
                .downloads
                .clear_lyrics_cache()
                .await
                .map_err(|error| error.to_string()),
        }
    }
}
