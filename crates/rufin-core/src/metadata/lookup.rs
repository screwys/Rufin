use super::{MetadataCandidate, MetadataEditor};
use localization::{msgid, tr};
use sources::SourceMetadataError as Error;
use std::sync::Arc;

pub(super) struct CachedLookup {
    candidate: MetadataCandidate,
    value: Arc<metadata_lookup::MetadataLookup>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl MetadataEditor {
    pub async fn lookup(&self, query: String) -> Result<Vec<MetadataCandidate>, Error> {
        if !self
            .source
            .shared
            .settings
            .load()
            .ui
            .allows_external_metadata_lookup()
        {
            return Ok(Vec::new());
        }
        let mut values = {
            let state = self.state();
            let mut values = state.values();
            for field in &state.mixed {
                values.remove(field);
            }
            values
        };
        let kind = if self
            .batch
            .as_ref()
            .is_some_and(|scope| scope.indices.len() > 1)
        {
            values.insert(
                "title".into(),
                values.get("album").cloned().unwrap_or_default(),
            );
            sources::SourceEntityKind::Album
        } else {
            self.draft.kind
        };
        if query.trim().is_empty()
            && let Some(candidate) = metadata_lookup::metadata_identity(kind, &values)
        {
            return Ok(vec![candidate]);
        }
        self.source
            .shared
            .runtime
            .spawn_blocking(move || metadata_lookup::search_metadata(kind, &values, &query))
            .await
            .map_err(|e| Error::Write(e.to_string()))?
            .map_err(Error::Write)
    }

    pub async fn preview_lookup(
        &self,
        candidate: Option<MetadataCandidate>,
        fill_missing: bool,
    ) -> Result<(), Error> {
        if !self
            .source
            .shared
            .settings
            .load()
            .ui
            .allows_external_metadata_lookup()
        {
            return Ok(());
        }
        let lookup = if let Some(candidate) = candidate.clone() {
            let cached = self
                .lookup
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .filter(|cached| {
                    cached.candidate.id == candidate.id
                        && cached.candidate.kind == candidate.kind
                        && cached.candidate.from_fingerprint == candidate.from_fingerprint
                })
                .map(|cached| cached.value.clone());
            if let Some(value) = cached {
                Some(value)
            } else {
                let request = candidate.clone();
                let value = Arc::new(
                    self.source
                        .shared
                        .runtime
                        .spawn_blocking(move || metadata_lookup::lookup_metadata(&request))
                        .await
                        .map_err(|e| Error::Write(e.to_string()))?
                        .map_err(Error::Write)?,
                );
                self.lookup
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .replace(CachedLookup {
                        candidate,
                        value: value.clone(),
                    });
                Some(value)
            }
        } else {
            None
        };
        if let Some(scope) = &self.batch {
            scope.batch.stage(self).await?;
            let result = scope
                .batch
                .preview_lookup(&scope.indices, lookup, candidate, fill_missing)
                .await;
            self.reload_batch().await?;
            return result;
        }
        let mut state = self.state();
        state.clear_preview();
        let Some(lookup) = lookup else { return Ok(()) };
        let changes = lookup
            .changes(
                self.draft.kind,
                &state.values(),
                state
                    .scan
                    .as_ref()
                    .map_or(&[], |(_, matches)| matches.recordings.as_slice()),
            )
            .ok_or_else(|| Error::InvalidInput(tr(msgid("No matching track in this release"))))?;
        state.preview_identified(&self.draft, changes, fill_missing);
        state.lookup_preview = candidate;
        Ok(())
    }
}
