use crate::shell::shell::Shell;
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::glib;
use localization::tr;
use localization::track_count_text;
use rufin_core::downloads::DownloadSubject;
use rufin_core::downloads::{DownloadEvent, DownloadFeedbackKind};
use sources::SourceId;
use std::time::Duration;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::{Rc, Weak},
    sync::Arc,
};
struct DownloadBadgeBinding {
    image: glib::WeakRef<gtk::Image>,
    downloaded: Cell<bool>,
}

pub fn media_download_badge(media_uri: &str, downloaded: bool) -> bool {
    downloaded && !media_uri.starts_with("file:") && !media_uri.starts_with("rufin:cue/")
}

pub enum OperationFeedbackKind {
    DownloadStarted,
    DownloadQueued,
    PlaylistAdded { destination: String },
    PlaylistRemoved { destination: String },
}

pub enum OperationArtwork {
    MediaUris(Vec<String>),
    Bindings(Vec<ArtworkBinding>),
}

pub struct OperationFeedback {
    pub title: String,
    pub artwork: OperationArtwork,
    pub item_count: usize,
    pub kind: OperationFeedbackKind,
}

pub struct DownloadsState {
    settings: Rc<crate::widgets::settings::SettingsState>,
    queue_refresh: RefCell<Option<(SourceId, Weak<dyn Fn()>)>>,
    badges: Rc<RefCell<HashMap<usize, DownloadBadgeBinding>>>,
}

impl DownloadsState {
    pub fn new(settings: Rc<crate::widgets::settings::SettingsState>) -> Rc<Self> {
        Rc::new(Self {
            settings,
            queue_refresh: Default::default(),
            badges: Default::default(),
        })
    }

    pub fn set_queue_refresh(&self, source_id: SourceId, refresh: &Rc<dyn Fn()>) {
        self.queue_refresh
            .replace(Some((source_id, Rc::downgrade(refresh))));
    }

    pub fn refresh_queue(&self, source_id: Option<&SourceId>) {
        let refresh = self
            .queue_refresh
            .borrow()
            .as_ref()
            .filter(|(source, _)| Some(source) == source_id)
            .and_then(|(_, refresh)| refresh.upgrade());
        if let Some(refresh) = refresh {
            refresh();
        }
    }
}

impl DownloadsState {
    pub fn download_badge(self: &Rc<Self>, _collection: bool) -> gtk::Image {
        let image = gtk::Image::new();
        self.register_download_badge(&image);
        image
    }
    pub fn register_download_badge(self: &Rc<Self>, image: &gtk::Image) {
        image.set_icon_name(Some("rufin-folder-download-symbolic"));
        image.add_css_class("downloaded-badge");
        image.set_pixel_size(14);
        image.set_tooltip_text(Some(&tr("Downloaded")));
        let binding = DownloadBadgeBinding {
            image: image.downgrade(),
            downloaded: Cell::new(false),
        };
        self.set_download_badge_visible(&image, false);
        let identity = image.as_ptr() as usize;
        self.badges.borrow_mut().insert(identity, binding);
        let badges = Rc::downgrade(&self.badges);
        let _ = image.add_weak_ref_notify_local(move || {
            let Some(badges) = badges.upgrade() else {
                return;
            };
            badges.borrow_mut().remove(&identity);
        });
    }
    pub fn bind_download_badge(self: &Rc<Self>, image: &gtk::Image, downloaded: bool) {
        let identity = image.as_ptr() as usize;
        if let Some(binding) = self.badges.borrow().get(&identity) {
            binding.downloaded.set(downloaded);
        }
        self.set_download_badge_visible(image, downloaded);
    }
    pub fn clear_download_badge(&self, image: &gtk::Image) {
        let identity = image.as_ptr() as usize;
        if let Some(binding) = self.badges.borrow().get(&identity) {
            binding.downloaded.set(false);
        }
        image.set_visible(false);
    }
    pub fn set_download_badge_visible(&self, image: &gtk::Image, downloaded: bool) {
        image.set_visible(
            self.settings
                .current
                .borrow()
                .application
                .show_downloaded_badges
                && downloaded,
        );
    }
    pub fn set_downloaded_badges_visible(self: &Rc<Self>, visible: bool) {
        if self
            .settings
            .set_app_setting("downloaded badge setting", visible, |settings| {
                &mut settings.application.show_downloaded_badges
            })
            .is_some()
        {
            self.refresh_download_badges();
        }
    }
    fn refresh_download_badges(&self) {
        self.badges.borrow_mut().retain(|_, binding| {
            let Some(image) = binding.image.upgrade() else {
                return false;
            };
            self.set_download_badge_visible(&image, binding.downloaded.get());
            true
        });
    }
}

/// What a download rule is called.
pub fn download_rule_title(rule: rufin_core::downloads::DownloadRule) -> &'static str {
    use rufin_core::downloads::DownloadRule;
    match rule {
        DownloadRule::EntireLibrary => localization::msgid("Entire Library"),
        DownloadRule::Favorites => localization::msgid("Favorites"),
        DownloadRule::AllPlaylists => localization::msgid("All Playlists"),
        DownloadRule::LatestFiveAlbums => localization::msgid("5 Latest Albums"),
    }
}

pub fn download_subject_title(subject: &DownloadSubject) -> String {
    match subject {
        DownloadSubject::Rule(rule) => tr(download_rule_title(*rule)),
        DownloadSubject::Prepared { title, .. } => title.clone().unwrap_or_else(|| tr("Selection")),
    }
}

pub fn media_artwork(
    artwork: &Rc<crate::widgets::artwork::ArtworkState>,
    catalog: &Arc<library::Catalog>,
    runtime: &tokio::runtime::Handle,
    media_uris: &[String],
    size: i32,
) -> gtk::Widget {
    let projection = artwork.cover_group_projection_for_artwork(&[], size, size, true);
    let widget = projection.widget();
    let catalog = Arc::clone(catalog);
    let media_uris = media_uris.iter().take(4).cloned().collect::<Vec<_>>();
    let task = runtime.spawn(async move {
        catalog
            .track_artwork_bindings(&media_uris, &library::ReadCancellation::new())
            .await
    });
    let shell = Rc::downgrade(artwork);
    glib::spawn_future_local(async move {
        let Some(bindings) = task.await.ok().and_then(Result::ok) else {
            return;
        };
        let Some(shell) = shell.upgrade() else { return };
        if bindings.is_empty() {
            return;
        }
        let bindings = bindings
            .iter()
            .map(|binding| ArtworkBinding::opaque(binding))
            .collect::<Vec<_>>();
        projection.replace(&shell, &bindings);
    });
    widget
}

pub fn download_rule_artwork(
    artwork: &Rc<crate::widgets::artwork::ArtworkState>,
    catalog: &Arc<library::Catalog>,
    runtime: &tokio::runtime::Handle,
    source_id: SourceId,
    rule: rufin_core::downloads::DownloadRule,
    size: i32,
) -> gtk::Widget {
    let projection = artwork.cover_group_projection_for_artwork(&[], size, size, true);
    let widget = projection.widget();
    let catalog = Arc::clone(catalog);
    let task = runtime.spawn(async move {
        let cancellation = library::ReadCancellation::new();
        catalog.loaded().await;
        let Some(source) = catalog.cached_source(source_id.as_str()) else {
            return Ok::<_, library::LibraryError>(Vec::new());
        };
        let scope = match rule {
            rufin_core::downloads::DownloadRule::EntireLibrary => {
                library::RepresentativeArtworkScope::AllTracks
            }
            rufin_core::downloads::DownloadRule::Favorites => {
                library::RepresentativeArtworkScope::FavoriteTracks
            }
            rufin_core::downloads::DownloadRule::AllPlaylists => {
                library::RepresentativeArtworkScope::PlaylistTracks
            }
            rufin_core::downloads::DownloadRule::LatestFiveAlbums => {
                library::RepresentativeArtworkScope::LatestAlbums(5)
            }
        };
        catalog
            .representative_artwork_page(source.source, None, scope, 4, &cancellation)
            .await
    });
    let shell = Rc::downgrade(artwork);
    glib::spawn_future_local(async move {
        let Ok(Ok(bindings)) = task.await else { return };
        let Some(shell) = shell.upgrade() else { return };
        let bindings = bindings
            .iter()
            .map(|binding| ArtworkBinding::opaque(binding))
            .collect::<Vec<_>>();
        projection.replace(&shell, &bindings);
    });
    widget
}

const OPERATION_FEEDBACK_DURATION: Duration = Duration::from_secs(3);

impl Shell {
    pub(crate) fn apply_download_event(self: &Rc<Self>, event: DownloadEvent) {
        match event {
            DownloadEvent::Queue { summary, .. } => {
                self.downloads.refresh_queue(summary.source_id.as_ref())
            }
            event @ (DownloadEvent::Changed { .. } | DownloadEvent::SubjectChanged { .. }) => {
                self.apply_download_change_to_mounted_route(&event);
            }
            DownloadEvent::Feedback(feedback) => {
                self.show_operation_feedback(&OperationFeedback {
                    title: crate::downloads::download_subject_title(&feedback.subject),
                    artwork: OperationArtwork::MediaUris(feedback.preview_uris),
                    item_count: feedback.item_count,
                    kind: match feedback.kind {
                        DownloadFeedbackKind::Started => OperationFeedbackKind::DownloadStarted,
                        DownloadFeedbackKind::Queued => OperationFeedbackKind::DownloadQueued,
                    },
                });
            }
            DownloadEvent::Notice(message) => self.show_download_notice(&message),
        }
    }

    pub(crate) fn show_operation_feedback(self: &Rc<Self>, feedback: &OperationFeedback) {
        self.show_operation_feedback_with_action(feedback, None);
    }

    pub(crate) fn show_undoable_operation_feedback(
        self: &Rc<Self>,
        feedback: &OperationFeedback,
        undo: impl FnOnce() + 'static,
    ) {
        self.show_operation_feedback_with_action(feedback, Some(Box::new(undo)));
    }

    fn show_operation_feedback_with_action(
        self: &Rc<Self>,
        feedback: &OperationFeedback,
        action: Option<Box<dyn FnOnce()>>,
    ) {
        crate::preferences::preferences::dialogs::release_notes::dismiss_release_notification(self);
        let title = &feedback.title;
        let count = track_count_text(feedback.item_count as u64);
        let subtitle = operation_feedback_subtitle(&feedback.kind, &count);
        self.chrome
            .operation_feedback
            .remove_css_class("operation-feedback-notice");
        self.chrome.operation_feedback_artwork.set_visible(true);
        while let Some(child) = self.chrome.operation_feedback_artwork.first_child() {
            self.chrome.operation_feedback_artwork.remove(&child);
        }
        let artwork = match &feedback.artwork {
            OperationArtwork::MediaUris(uris) => crate::downloads::media_artwork(
                &self.artwork,
                &self.products.library,
                &self.products.runtime,
                uris,
                48,
            ),
            OperationArtwork::Bindings(bindings) => self
                .artwork
                .cover_group_projection_for_artwork(bindings, 48, 48, true)
                .widget(),
        };
        self.chrome.operation_feedback_artwork.append(&artwork);
        self.chrome.operation_feedback_title.set_text(title);
        self.chrome.operation_feedback_subtitle.set_visible(true);
        self.chrome.operation_feedback_subtitle.set_text(&subtitle);
        let opens_queue = matches!(
            feedback.kind,
            OperationFeedbackKind::DownloadStarted | OperationFeedbackKind::DownloadQueued
        );
        self.download_feedback.feedback_opens_queue.set(opens_queue);
        self.chrome.operation_feedback_action.set_label(&tr("Undo"));
        self.chrome
            .operation_feedback_action
            .set_visible(action.is_some());
        self.download_feedback.feedback_action.replace(action);
        self.present_download_feedback();
    }

    fn show_download_notice(self: &Rc<Self>, message: &str) {
        self.chrome
            .operation_feedback
            .add_css_class("operation-feedback-notice");
        self.chrome.operation_feedback_artwork.set_visible(false);
        self.chrome.operation_feedback_title.set_text(message);
        self.chrome.operation_feedback_subtitle.set_visible(false);
        self.download_feedback.feedback_opens_queue.set(false);
        self.chrome.operation_feedback_action.set_visible(false);
        self.download_feedback.feedback_action.borrow_mut().take();
        self.present_download_feedback();
    }

    fn present_download_feedback(self: &Rc<Self>) {
        let generation = self
            .download_feedback
            .feedback_generation
            .get()
            .wrapping_add(1);
        self.download_feedback.feedback_generation.set(generation);
        self.chrome.operation_feedback.set_visible(true);
        let shell = Rc::downgrade(self);
        glib::timeout_add_local_once(OPERATION_FEEDBACK_DURATION, move || {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            if shell.download_feedback.feedback_generation.get() == generation {
                shell.chrome.operation_feedback.set_visible(false);
                shell.download_feedback.feedback_action.borrow_mut().take();
            }
        });
    }

    pub(crate) fn connect_operation_feedback(self: &Rc<Self>) {
        let action_shell = Rc::downgrade(self);
        self.chrome
            .operation_feedback_action
            .connect_clicked(move |_| {
                let Some(shell) = action_shell.upgrade() else {
                    return;
                };
                shell.download_feedback.feedback_generation.set(
                    shell
                        .download_feedback
                        .feedback_generation
                        .get()
                        .wrapping_add(1),
                );
                shell.chrome.operation_feedback.set_visible(false);
                let action = shell.download_feedback.feedback_action.borrow_mut().take();
                if let Some(action) = action {
                    action();
                }
            });

        let click = gtk::GestureClick::new();
        let weak_shell = Rc::downgrade(self);
        click.connect_released(move |_, _, _, _| {
            let Some(shell) = weak_shell.upgrade() else {
                return;
            };
            if shell.download_feedback.feedback_opens_queue.get() {
                crate::preferences::preferences::present_downloads_preferences_dialog(&shell);
            }
            shell.chrome.operation_feedback.set_visible(false);
            shell.download_feedback.feedback_action.borrow_mut().take();
        });
        self.chrome.operation_feedback.add_controller(click);
    }
}

fn operation_feedback_subtitle(kind: &OperationFeedbackKind, count: &str) -> String {
    match kind {
        OperationFeedbackKind::DownloadStarted => {
            format!("{} · {count}", tr("Download started"))
        }
        OperationFeedbackKind::DownloadQueued => {
            format!("{} · {count}", tr("Download queued"))
        }
        OperationFeedbackKind::PlaylistAdded { destination } => {
            format!("{} {destination} · {count}", tr("Added to"))
        }
        OperationFeedbackKind::PlaylistRemoved { destination } => {
            format!("{} {destination} · {count}", tr("Removed from"))
        }
    }
}

#[derive(Default)]
pub(crate) struct DownloadFeedbackState {
    feedback_generation: Rc<Cell<u64>>,
    feedback_opens_queue: Cell<bool>,
    feedback_action: RefCell<Option<Box<dyn FnOnce()>>>,
}
