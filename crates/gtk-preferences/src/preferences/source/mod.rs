use std::cell::{Cell, RefCell};
use std::rc::Rc;

use localization::{tr, trn_with};

use rufin_core::runtime::source::{
    ConfiguredSources, DiscoveredServer, DiscoveryStatus, SourceOperation, SourceProgress,
    SourceProgressStage, SourceSummary,
};

pub(super) mod excluded_folders;
pub(super) mod local_access;
pub mod login;

pub struct SourceState {
    pub configured: RefCell<ConfiguredSources>,
    pub operation: RefCell<SourceOperation>,
    pub discovered_servers: RefCell<Vec<DiscoveredServer>>,
    pub discovery_status: RefCell<DiscoveryStatus>,
    pub discovery_running: Cell<bool>,
    pub discovery_started: Cell<bool>,
    pub discovery_provider: Cell<rufin_core::runtime::source::DiscoveryProvider>,
    pub add_server: RefCell<Option<login::SourceSetupViewHandle>>,
    pub refresh_feedback_generation: Rc<Cell<u64>>,
    pub artwork_preparation_revision: Cell<Option<u64>>,
}

pub fn configured_source_kind_display_name(kind: &str) -> String {
    gtk_widgets::source_labels::source_kind_title(kind).map_or_else(|| kind.to_string(), tr)
}

pub fn half_stars_row(
    shell: &Rc<crate::Preferences>,
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

pub fn playlist_auto_save_row(
    shell: &Rc<crate::Preferences>,
    source: &SourceSummary,
) -> Option<adw::SwitchRow> {
    use adw::prelude::*;
    if !matches!(source.kind.as_str(), "local" | "smb" | "webdav") {
        return None;
    }
    let resource = crate::ui_resource::PLAYLIST_FILE_DIALOG_RESOURCE;
    let row: adw::SwitchRow = gtk_widgets::ui_resource::object(
        &gtk_widgets::ui_resource::builder(resource),
        resource,
        "source_auto_save",
    );
    let owner = shell.products.source.clone();
    let source_id = source.id.clone();
    let weak_row = row.downgrade();
    let weak_shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let Some(row) = weak_row.upgrade() else {
            return;
        };
        match rufin_core::playlist_files::source_auto_save(&owner, source_id.clone())
            .recv()
            .await
        {
            Ok(Ok(enabled)) => row.set_active(enabled),
            Ok(Err(error)) => {
                if let Some(shell) = weak_shell.upgrade() {
                    shell.control_feedback.show_feedback_toast(error);
                }
                return;
            }
            Err(_) => return,
        }
        row.connect_active_notify(move |row| {
            let result = rufin_core::playlist_files::set_source_auto_save(
                &owner,
                source_id.clone(),
                row.is_active(),
            );
            let weak_shell = weak_shell.clone();
            gtk::glib::spawn_future_local(async move {
                if let Ok(Err(error)) = result.recv().await {
                    if let Some(shell) = weak_shell.upgrade() {
                        shell.control_feedback.show_feedback_toast(error);
                    }
                }
            });
        });
    });
    Some(row)
}

pub fn folder_selected_text(count: u64) -> String {
    let label = count.to_string();
    trn_with(
        "{count} folder selected",
        "{count} folders selected",
        count,
        &[("count", label.as_str())],
    )
}

pub fn source_progress_text(progress: &SourceProgress) -> String {
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

pub fn source_operation_text(operation: &SourceOperation) -> Option<String> {
    match operation {
        SourceOperation::Idle => None,
        SourceOperation::Adding { progress } | SourceOperation::Refreshing { progress, .. } => {
            Some(source_progress_text(progress))
        }
        SourceOperation::Switching { .. } => Some(tr("Switching library...")),
        SourceOperation::Failed { message, .. } => Some(message.clone()),
    }
}
