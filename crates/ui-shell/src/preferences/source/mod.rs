use std::cell::{Cell, RefCell};
use std::rc::Rc;
use ui_shared::source_labels::{configured_source_display_name, configured_source_icon_name};

use localization::{tr, trn_with};

use rufin_core::runtime::source::{
    ConfiguredSources, DiscoveredServer, DiscoveryStatus, SourceOperation, SourceProgress,
    SourceProgressStage, SourceSummary,
};

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
    pub(crate) discovery_provider: Cell<rufin_core::runtime::source::DiscoveryProvider>,
    pub(crate) add_server: RefCell<Option<login::SourceSetupViewHandle>>,
    pub(crate) refresh_feedback_generation: Rc<Cell<u64>>,
    pub(crate) artwork_preparation_revision: Cell<Option<u64>>,
}

pub(crate) fn configured_source_kind_display_name(kind: &str) -> String {
    ui_shared::source_labels::source_kind_title(kind).map_or_else(|| kind.to_string(), tr)
}

pub(crate) fn half_stars_row(
    shell: &Rc<crate::shell::Shell>,
    source: &SourceSummary,
) -> Option<adw::SwitchRow> {
    if !matches!(source.kind.as_str(), "navidrome" | "subsonic" | "local") {
        return None;
    }
    let row = adw::SwitchRow::builder()
        .title(tr("Enable partial star ratings"))
        .subtitle(tr(
            "This is not supported natively by this source, but Rufin can show partial stars and then round them up for the source",
        ))
        .active(source.half_stars_enabled)
        .build();
    let source_handle = shell.products.source.clone();
    let source_id = source.id.clone();
    row.connect_active_notify(move |row| {
        source_handle.set_half_stars(source_id.clone(), row.is_active());
    });
    Some(row)
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
        SourceProgressStage::Files => "files",
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
