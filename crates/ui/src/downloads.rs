use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use artwork::ArtworkBinding;
use downloads::{
    DownloadEvent, DownloadFeedbackKind, DownloadQueueItem, DownloadQueueSnapshot, DownloadSubject,
};
use gtk::glib;
use localization::{tr, track_count_text};
use sources::SourceId;

use crate::shell::Shell;

const OPERATION_FEEDBACK_DURATION: Duration = Duration::from_secs(3);

struct DownloadBadgeBinding {
    image: glib::WeakRef<gtk::Image>,
    downloaded: Cell<bool>,
}

pub(crate) enum OperationFeedbackKind {
    DownloadStarted,
    DownloadQueued,
    PlaylistAdded { destination: String },
    PlaylistRemoved { destination: String },
}

pub(crate) struct OperationFeedback {
    pub(crate) subject: DownloadSubject,
    pub(crate) item_count: usize,
    pub(crate) kind: OperationFeedbackKind,
}

#[derive(Default)]
pub(crate) struct DownloadsState {
    pub(crate) snapshots: RefCell<HashMap<SourceId, Arc<DownloadQueueSnapshot>>>,
    queue_refresh: RefCell<Option<Weak<dyn Fn()>>>,
    feedback_generation: Rc<Cell<u64>>,
    feedback_opens_queue: Cell<bool>,
    feedback_action: RefCell<Option<Box<dyn FnOnce()>>>,
    badges: Rc<RefCell<HashMap<usize, DownloadBadgeBinding>>>,
}

impl DownloadsState {
    pub(crate) fn set_queue_refresh(&self, refresh: &Rc<dyn Fn()>) {
        self.queue_refresh.replace(Some(Rc::downgrade(refresh)));
    }

    pub(crate) fn refresh_queue(&self) {
        let refresh = self.queue_refresh.borrow().as_ref().and_then(Weak::upgrade);
        if let Some(refresh) = refresh {
            refresh();
        } else {
            self.queue_refresh.borrow_mut().take();
        }
    }
}

impl Shell {
    pub(crate) fn apply_download_event(self: &Rc<Self>, event: DownloadEvent) {
        match event {
            DownloadEvent::Queue {
                source_id,
                snapshot,
            } => {
                let previous = self
                    .downloads
                    .snapshots
                    .borrow_mut()
                    .insert(source_id, Arc::clone(&snapshot));
                let collection_changed = previous.as_ref().is_none_or(|previous| {
                    previous.jobs.len() != snapshot.jobs.len()
                        || previous.downloaded_tracks != snapshot.downloaded_tracks
                });
                if collection_changed {
                    self.replace_current_route_when_ready();
                }
                self.downloads.refresh_queue();
            }
            DownloadEvent::Feedback(feedback) => {
                self.show_operation_feedback(&OperationFeedback {
                    subject: feedback.subject,
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

    pub(crate) fn move_download_job(
        self: &Rc<Self>,
        source_id: SourceId,
        job_id: String,
        target_job_id: String,
        after: bool,
    ) -> bool {
        {
            let mut snapshots = self.downloads.snapshots.borrow_mut();
            let Some(snapshot) = snapshots.get_mut(&source_id) else {
                return false;
            };
            let mut jobs = snapshot.jobs.to_vec();
            if !reorder_queue_items(&mut jobs, &job_id, &target_job_id, after) {
                return false;
            }
            *snapshot = Arc::new(DownloadQueueSnapshot {
                jobs: jobs.into(),
                downloaded_tracks: snapshot.downloaded_tracks,
                paused: snapshot.paused,
            });
        }
        self.downloads.refresh_queue();
        self.products
            .downloads
            .move_job(source_id, job_id, target_job_id, after);
        true
    }

    pub(crate) fn show_operation_feedback(self: &Rc<Self>, feedback: &OperationFeedback) {
        self.show_operation_feedback_with_action(feedback, None);
    }

    pub(crate) fn show_undoable_operation_feedback(
        self: &Rc<Self>,
        feedback: &OperationFeedback,
        undo: impl Fn() + 'static,
    ) {
        self.show_operation_feedback_with_action(feedback, Some(Box::new(undo)));
    }

    fn show_operation_feedback_with_action(
        self: &Rc<Self>,
        feedback: &OperationFeedback,
        action: Option<Box<dyn FnOnce()>>,
    ) {
        crate::preferences::dialogs::release_notes::dismiss_release_notification(self);
        let title = self.download_subject_title(&feedback.subject);
        let count = track_count_text(feedback.item_count as u64);
        let subtitle = operation_feedback_subtitle(&feedback.kind, &count);
        self.chrome
            .operation_feedback
            .remove_css_class("operation-feedback-notice");
        self.chrome.operation_feedback_artwork.set_visible(true);
        while let Some(child) = self.chrome.operation_feedback_artwork.first_child() {
            self.chrome.operation_feedback_artwork.remove(&child);
        }
        self.chrome
            .operation_feedback_artwork
            .append(&self.download_subject_artwork(&feedback.subject, 48));
        self.chrome.operation_feedback_title.set_text(&title);
        self.chrome.operation_feedback_subtitle.set_visible(true);
        self.chrome.operation_feedback_subtitle.set_text(&subtitle);
        let opens_queue = matches!(
            feedback.kind,
            OperationFeedbackKind::DownloadStarted | OperationFeedbackKind::DownloadQueued
        );
        self.downloads.feedback_opens_queue.set(opens_queue);
        self.chrome.operation_feedback_action.set_label(&tr("Undo"));
        self.chrome
            .operation_feedback_action
            .set_visible(action.is_some());
        self.downloads.feedback_action.replace(action);
        self.present_download_feedback();
    }

    fn show_download_notice(self: &Rc<Self>, message: &str) {
        self.chrome
            .operation_feedback
            .add_css_class("operation-feedback-notice");
        self.chrome.operation_feedback_artwork.set_visible(false);
        self.chrome.operation_feedback_title.set_text(message);
        self.chrome.operation_feedback_subtitle.set_visible(false);
        self.downloads.feedback_opens_queue.set(false);
        self.chrome.operation_feedback_action.set_visible(false);
        self.downloads.feedback_action.borrow_mut().take();
        self.present_download_feedback();
    }

    fn present_download_feedback(self: &Rc<Self>) {
        let generation = self.downloads.feedback_generation.get().wrapping_add(1);
        self.downloads.feedback_generation.set(generation);
        self.chrome.operation_feedback.set_visible(true);
        let shell = Rc::downgrade(self);
        glib::timeout_add_local_once(OPERATION_FEEDBACK_DURATION, move || {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            if shell.downloads.feedback_generation.get() == generation {
                shell.chrome.operation_feedback.set_visible(false);
                shell.downloads.feedback_action.borrow_mut().take();
            }
        });
    }

    pub(crate) fn download_subject_title(&self, subject: &DownloadSubject) -> String {
        match subject {
            DownloadSubject::Rule(downloads::DownloadRule::EntireLibrary) => tr("Entire Library"),
            DownloadSubject::Rule(downloads::DownloadRule::Favorites) => tr("Favorites"),
            DownloadSubject::Rule(downloads::DownloadRule::AllPlaylists) => tr("All Playlists"),
            DownloadSubject::Rule(downloads::DownloadRule::LatestFiveAlbums) => {
                tr("5 Latest Albums")
            }
            DownloadSubject::Track(_) => tr("Track"),
            DownloadSubject::Album(_) => tr("Album"),
            DownloadSubject::Artist(_) => tr("Artist"),
            DownloadSubject::Genre(_) => tr("Genre"),
            DownloadSubject::Mood(_) => tr("Mood"),
            DownloadSubject::Playlist(_) => tr("Playlist"),
            DownloadSubject::SmartPlaylist(_) => tr("Smart Playlist"),
            DownloadSubject::Prepared { title, .. } => {
                title.clone().unwrap_or_else(|| tr("Selection"))
            }
        }
    }

    pub(crate) fn download_subject_artwork(
        self: &Rc<Self>,
        subject: &DownloadSubject,
        size: i32,
    ) -> gtk::Widget {
        let projection = self.cover_group_projection_for_artwork(&[], size, size);
        let widget = projection.widget();
        let Some(selected) = self.selected_library().as_deref().cloned() else {
            return widget;
        };
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let subject = subject.clone();
        let prefer_server_playlist_covers =
            self.settings.current.borrow().prefer_server_playlist_covers;
        let task = selected.runtime.spawn(async move {
            download_subject_artwork_bindings(
                &database,
                source,
                folder,
                &subject,
                prefer_server_playlist_covers,
            )
            .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            let Some(bindings) = task.await.ok().and_then(Result::ok) else {
                return;
            };
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

    pub(crate) fn connect_operation_feedback(self: &Rc<Self>) {
        let action_shell = Rc::downgrade(self);
        self.chrome
            .operation_feedback_action
            .connect_clicked(move |_| {
                let Some(shell) = action_shell.upgrade() else {
                    return;
                };
                shell
                    .downloads
                    .feedback_generation
                    .set(shell.downloads.feedback_generation.get().wrapping_add(1));
                shell.chrome.operation_feedback.set_visible(false);
                let action = shell.downloads.feedback_action.borrow_mut().take();
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
            if shell.downloads.feedback_opens_queue.get() {
                crate::preferences::present_downloads_preferences_dialog(&shell);
            }
            shell.chrome.operation_feedback.set_visible(false);
            shell.downloads.feedback_action.borrow_mut().take();
        });
        self.chrome.operation_feedback.add_controller(click);
    }

    pub(crate) fn download_badge(self: &Rc<Self>, _collection: bool) -> gtk::Image {
        let image = gtk::Image::from_icon_name("rufin-folder-download-symbolic");
        image.add_css_class("downloaded-badge");
        image.set_pixel_size(14);
        image.set_tooltip_text(Some(&tr("Downloaded")));
        let binding = DownloadBadgeBinding {
            image: image.downgrade(),
            downloaded: Cell::new(false),
        };
        self.set_download_badge_visible(&image, false);
        let identity = image.as_ptr() as usize;
        self.downloads.badges.borrow_mut().insert(identity, binding);
        let badges = Rc::downgrade(&self.downloads.badges);
        let _ = image.add_weak_ref_notify_local(move || {
            let Some(badges) = badges.upgrade() else {
                return;
            };
            badges.borrow_mut().remove(&identity);
        });
        image
    }

    pub(crate) fn bind_download_badge(self: &Rc<Self>, image: &gtk::Image, downloaded: bool) {
        let identity = image.as_ptr() as usize;
        if let Some(binding) = self.downloads.badges.borrow().get(&identity) {
            binding.downloaded.set(downloaded);
        }
        self.set_download_badge_visible(image, downloaded);
    }

    pub(crate) fn clear_download_badge(&self, image: &gtk::Image) {
        let identity = image.as_ptr() as usize;
        if let Some(binding) = self.downloads.badges.borrow().get(&identity) {
            binding.downloaded.set(false);
        }
        image.set_visible(false);
    }

    pub(crate) fn set_download_badge_visible(&self, image: &gtk::Image, downloaded: bool) {
        image.set_visible(self.settings.current.borrow().show_downloaded_badges && downloaded);
    }

    pub(crate) fn set_downloaded_badges_visible(self: &Rc<Self>, visible: bool) {
        if self
            .update_app_settings("downloaded badge setting", |settings| {
                if settings.show_downloaded_badges == visible {
                    return false;
                }
                settings.show_downloaded_badges = visible;
                true
            })
            .is_some()
        {
            self.refresh_download_badges();
        }
    }

    fn refresh_download_badges(&self) {
        self.downloads.badges.borrow_mut().retain(|_, binding| {
            let Some(image) = binding.image.upgrade() else {
                return false;
            };
            self.set_download_badge_visible(&image, binding.downloaded.get());
            true
        });
    }
}

async fn download_subject_artwork_bindings(
    database: &library::Database,
    source: library::SourceKey,
    folder: Option<library::FolderKey>,
    subject: &DownloadSubject,
    prefer_server_playlist_covers: bool,
) -> library::LibraryResult<Vec<Vec<u8>>> {
    let cancellation = library::ReadCancellation::new();
    let mut bindings = match subject {
        DownloadSubject::Rule(rule) => {
            let scope = match rule {
                downloads::DownloadRule::EntireLibrary => {
                    library::RepresentativeArtworkScope::AllTracks
                }
                downloads::DownloadRule::Favorites => {
                    library::RepresentativeArtworkScope::FavoriteTracks
                }
                downloads::DownloadRule::AllPlaylists => {
                    library::RepresentativeArtworkScope::PlaylistTracks
                }
                downloads::DownloadRule::LatestFiveAlbums => {
                    library::RepresentativeArtworkScope::LatestAlbums(5)
                }
            };
            database
                .representative_artwork_page(source, folder, scope, 4, &cancellation)
                .await?
        }
        DownloadSubject::Track(key) => database
            .track_rows(source, &[*key], &cancellation)
            .await?
            .pop()
            .and_then(|row| row.artwork_binding)
            .into_iter()
            .collect(),
        DownloadSubject::Album(key) => database
            .album_rows(source, &[*key], folder, &cancellation)
            .await?
            .pop()
            .and_then(|row| row.artwork_binding)
            .into_iter()
            .collect(),
        DownloadSubject::Artist(key) => database
            .artist_rows(source, &[*key], false, folder, &cancellation)
            .await?
            .pop()
            .and_then(|row| row.artwork_binding)
            .into_iter()
            .collect(),
        DownloadSubject::Genre(key) => database
            .genre_rows(source, &[*key], folder, &cancellation)
            .await?
            .pop()
            .map(|row| {
                row.artwork_binding
                    .into_iter()
                    .chain(row.representative_artwork)
                    .take(4)
                    .collect()
            })
            .unwrap_or_default(),
        DownloadSubject::Mood(key) => database
            .mood_rows(source, &[*key], folder, &cancellation)
            .await?
            .pop()
            .map(|row| row.representative_artwork)
            .unwrap_or_default(),
        DownloadSubject::Playlist(key) => database
            .playlist_rows(source, &[*key], folder, &cancellation)
            .await?
            .pop()
            .map(|row| {
                if prefer_server_playlist_covers {
                    row.artwork_binding
                        .into_iter()
                        .chain(row.representative_artwork)
                        .take(4)
                        .collect()
                } else if row.representative_artwork.is_empty() {
                    row.artwork_binding.into_iter().collect()
                } else {
                    row.representative_artwork
                }
            })
            .unwrap_or_default(),
        DownloadSubject::SmartPlaylist(key) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .try_into()
                .unwrap_or(i64::MAX);
            database
                .smart_playlist_rows(source, &[*key], folder, now, &cancellation)
                .await?
                .pop()
                .map(|row| row.artwork_bindings)
                .unwrap_or_default()
        }
        DownloadSubject::Prepared { .. } => Vec::new(),
    };
    if bindings.is_empty() {
        bindings = database
            .artwork_preparation_page(source, None, 4, &cancellation)
            .await?;
    }
    Ok(bindings)
}

fn reorder_queue_items(
    jobs: &mut Vec<DownloadQueueItem>,
    job_id: &str,
    target_job_id: &str,
    after: bool,
) -> bool {
    if job_id == target_job_id {
        return false;
    }
    let Some(source_index) = jobs.iter().position(|job| job.id == job_id) else {
        return false;
    };
    let job = jobs.remove(source_index);
    let Some(target_index) = jobs
        .iter()
        .position(|candidate| candidate.id == target_job_id)
    else {
        jobs.insert(source_index, job);
        return false;
    };
    jobs.insert(target_index + usize::from(after), job);
    true
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
