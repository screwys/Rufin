use super::ProductHandles;
pub use library::CacheUsage;

#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheKind {
    Waveforms,
    Loudness,
    Fingerprints,
    MetadataLookups,
    Artwork,
    Lyrics,
    JapaneseDictionary,
}

impl CacheKind {
    pub(crate) const ALL: [Self; 7] = [
        Self::Waveforms,
        Self::Loudness,
        Self::Fingerprints,
        Self::MetadataLookups,
        Self::Artwork,
        Self::Lyrics,
        Self::JapaneseDictionary,
    ];

    /// Clears this cache in place, while the runtime runs.
    pub(crate) async fn clear(
        self,
        library: &library::Catalog,
        downloads: &crate::downloads::Downloads,
        lyrics: &crate::lyrics::LyricsHandle,
    ) -> Result<(), String> {
        match self {
            Self::MetadataLookups => library.clear_metadata_lookup_cache().await,
            Self::Fingerprints => library.clear_fingerprint_cache().await,
            Self::Waveforms => library.clear_waveform_cache().await,
            Self::Loudness => library.clear_loudness_cache().await,
            Self::JapaneseDictionary => return lyrics.clear_dictionary_cache().await,
            Self::Artwork => return downloads.clear_artwork_cache().await.map_err(string),
            Self::Lyrics => return downloads.clear_lyrics_cache().await.map_err(string),
        }
        .map_err(string)
    }
}

fn string(error: impl std::fmt::Display) -> String {
    error.to_string()
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
        for kind in CacheKind::ALL {
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
            .prepare_storage_reset(reset, self.connect.secret_keys())
    }

    /// Stops playback and scrobbling, then saves the settings and closes the Catalog, which
    /// writes its image once for the next start, or with `close_storage` closes every store
    /// so a reset can remove them. It blocks, so it runs off the async runtime.
    pub fn shutdown(&self, close_storage: bool) -> Result<(), String> {
        self.playback.shutdown();
        self.scrobbling.shutdown();
        if close_storage {
            return self.runtime.block_on(self.close_storage());
        }
        let flushed = self.settings.flush();
        let closed = self.runtime.block_on(self.library.as_ref().clone().close());
        flushed.and(closed.map_err(|error| error.to_string()))
    }

    pub async fn close_storage(&self) -> Result<(), String> {
        self.connect.close_storage().await?;
        self.source.close_storage().await;
        self.downloads.shutdown().await?;
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.flush())
            .await
            .map_err(|e| e.to_string())??;
        self.library
            .as_ref()
            .clone()
            .close()
            .await
            .map_err(|error| error.to_string())
    }

    async fn cache_usage(&self, kind: CacheKind) -> Result<CacheUsage, String> {
        match kind {
            CacheKind::MetadataLookups => self
                .library
                .metadata_lookup_cache_usage()
                .await
                .map_err(|error| error.to_string()),
            CacheKind::Fingerprints => self
                .library
                .fingerprint_cache_usage()
                .await
                .map_err(|error| error.to_string()),
            CacheKind::JapaneseDictionary => self.lyrics.dictionary_cache_usage().await,
            CacheKind::Waveforms => self
                .library
                .waveform_cache_usage()
                .await
                .map_err(|error| error.to_string()),
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

    pub async fn clear_cached_data(&self, kind: CacheKind) -> Result<(), String> {
        kind.clear(&self.library, &self.downloads, &self.lyrics)
            .await
    }
}
