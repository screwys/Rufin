use super::{MetadataCandidate, MetadataEditor};
use localization::{msgid, tr};
use sources::SourceMetadataError as Error;

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

    pub async fn apply_lookup(
        &self,
        candidate: MetadataCandidate,
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
        let lookup = self
            .source
            .shared
            .runtime
            .spawn_blocking(move || metadata_lookup::lookup_metadata(&candidate))
            .await
            .map_err(|e| Error::Write(e.to_string()))?
            .map_err(Error::Write)?;
        if let Some(scope) = &self.batch {
            scope.batch.stage(self).await?;
            let result = scope
                .batch
                .apply_lookup(&scope.indices, lookup, fill_missing)
                .await;
            self.reload_batch().await?;
            return result;
        }
        let mut state = self.state();
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
        state.apply_identified(&self.draft, changes, fill_missing);
        Ok(())
    }
}
