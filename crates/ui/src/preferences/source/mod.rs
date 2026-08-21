use std::cell::{Cell, RefCell};
use std::rc::Rc;

use localization::{tr, trn_with};

use crate::runtime::source::{
    ConfiguredSources, DiscoveredServer, DiscoveryStatus, SourceOperation, SourceProgress,
    SourceProgressStage, SourceSummary,
};

pub(crate) mod field_layout;
pub(super) mod local_access;
pub(crate) mod login;
pub(crate) mod selector;

pub(crate) struct SourceState {
    pub(crate) configured: RefCell<ConfiguredSources>,
    pub(crate) operation: RefCell<SourceOperation>,
    pub(crate) discovered_servers: RefCell<Vec<DiscoveredServer>>,
    pub(crate) discovery_status: RefCell<DiscoveryStatus>,
    pub(crate) discovery_running: Cell<bool>,
    pub(crate) discovery_started: Cell<bool>,
    pub(crate) add_server: RefCell<Option<login::SourceSetupViewHandle>>,
    pub(crate) refresh_feedback_generation: Rc<Cell<u64>>,
}

impl SourceState {
    pub(crate) fn login_screen_active(&self) -> bool {
        self.configured.borrow().first_run
    }
}

pub(crate) fn configured_source_display_name(source: &SourceSummary) -> String {
    let name = source.name.trim();
    if name.is_empty() {
        configured_source_kind_display_name(&source.kind)
    } else {
        name.to_string()
    }
}

pub(crate) fn configured_source_kind_display_name(kind: &str) -> String {
    login::source_kind_title(kind).map_or_else(|| kind.to_string(), tr)
}

pub(crate) fn configured_source_icon_name(source: &SourceSummary) -> &'static str {
    login::source_kind_icon_name(&source.kind).unwrap_or("rufin-network-server-symbolic")
}

pub(crate) fn folder_selected_text(count: u64) -> String {
    let label = count.to_string();
    trn_with(
        "{count} folder selected",
        "{count} folders selected",
        count,
        &[("count", label.as_str())],
    )
}

pub(crate) fn source_progress_text(progress: &SourceProgress) -> String {
    let subject = match progress.stage {
        SourceProgressStage::Connecting => return tr("Connecting to music server..."),
        SourceProgressStage::Albums => "albums",
        SourceProgressStage::Tracks => "tracks",
        SourceProgressStage::Artists => "artists",
        SourceProgressStage::Genres => "genres",
        SourceProgressStage::Playlists => "playlists",
        SourceProgressStage::Home => "Home",
        SourceProgressStage::Artwork => "artwork",
        SourceProgressStage::Files => return tr("Caching local library..."),
        SourceProgressStage::Finalizing => return tr("Preparing library..."),
    };
    match progress.total {
        Some(total) => format!(
            "Fetching {subject}, {}/{total} fetched...",
            progress.completed
        ),
        None if progress.completed > 0 => {
            format!("Fetching {subject}, {} fetched...", progress.completed)
        }
        None => format!("Fetching {subject}..."),
    }
}

pub(crate) fn source_operation_text(operation: &SourceOperation) -> Option<String> {
    match operation {
        SourceOperation::Idle => None,
        SourceOperation::Adding { progress } | SourceOperation::Refreshing { progress, .. } => {
            Some(source_progress_text(progress))
        }
        SourceOperation::Switching { .. } => Some(tr("Switching library...")),
        SourceOperation::Failed { message, .. } => Some(message.clone()),
    }
}
