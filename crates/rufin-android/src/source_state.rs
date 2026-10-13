use rufin_core::runtime::SourceHandle;

use crate::host::{AndroidError, error};

#[derive(uniffi::Record)]
pub struct AndroidSourceState {
    pub sources: Vec<rufin_core::runtime::source::SourceSummary>,
    pub selected_source_id: Option<String>,
    pub document_roots: Vec<sources::DocumentRoot>,
}

#[derive(uniffi::Object)]
pub struct AndroidSourceSubscription {
    source: SourceHandle,
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<()>>,
}

impl AndroidSourceSubscription {
    pub(crate) fn new(source: SourceHandle) -> Self {
        let mut changes = source.configuration_changes();
        changes.mark_changed();
        Self {
            source,
            changes: tokio::sync::Mutex::new(changes),
        }
    }
}

#[uniffi::export]
impl AndroidSourceSubscription {
    pub async fn next(&self) -> Result<AndroidSourceState, AndroidError> {
        self.changes.lock().await.changed().await.map_err(error)?;
        let state = self.source.list_sources();
        let selected_local = state.sources.iter().any(|source| {
            Some(&source.id) == state.selected_source_id.as_ref() && source.kind == "local"
        });
        let roots = if selected_local {
            self.source.document_roots().map_err(error)?
        } else {
            Vec::new()
        };
        Ok(AndroidSourceState {
            sources: state.sources.to_vec(),
            selected_source_id: state.selected_source_id.map(|source| source.to_string()),
            document_roots: roots,
        })
    }
}
