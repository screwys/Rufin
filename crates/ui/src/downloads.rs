use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use artwork::ArtworkBinding;
use downloads::{
    DownloadEvent, DownloadFeedbackKind, DownloadQueueItem, DownloadQueueSnapshot, DownloadSubject,
};
use gtk::glib;
use library::{SourceArtwork, SourceId};
use localization::{tr, track_count_text};

use crate::routes::library_fields::smart_playlist_display_name;
use crate::runtime::SelectedLibrary;
use crate::shell::Shell;
use crate::shell::cover::{ArtworkTile, cover_fetch_size_for_display};

struct DownloadBadgeBinding {
    image: glib::WeakRef<gtk::Image>,
    collection: bool,
    downloaded: Rc<dyn Fn(&SelectedLibrary) -> bool>,
}

pub(crate) enum OperationFeedbackKind {
    DownloadStarted,
    DownloadQueued,
    PlaylistAdded { destination: String },
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
                        || previous.downloaded_tracks > snapshot.downloaded_tracks
                });
                self.refresh_download_badges(collection_changed);
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
        self.downloads.feedback_opens_queue.set(matches!(
            feedback.kind,
            OperationFeedbackKind::DownloadStarted | OperationFeedbackKind::DownloadQueued
        ));
        self.present_download_feedback();
    }

    fn show_download_notice(&self, message: &str) {
        self.chrome
            .operation_feedback
            .add_css_class("operation-feedback-notice");
        self.chrome.operation_feedback_artwork.set_visible(false);
        self.chrome.operation_feedback_title.set_text(message);
        self.chrome.operation_feedback_subtitle.set_visible(false);
        self.downloads.feedback_opens_queue.set(false);
        self.present_download_feedback();
    }

    fn present_download_feedback(&self) {
        let generation = self.downloads.feedback_generation.get().wrapping_add(1);
        self.downloads.feedback_generation.set(generation);
        self.chrome.operation_feedback.set_visible(true);
        let card = self.chrome.operation_feedback.clone();
        let active_generation = Rc::clone(&self.downloads.feedback_generation);
        glib::timeout_add_local_once(Duration::from_secs(5), move || {
            if active_generation.get() == generation {
                card.set_visible(false);
            }
        });
    }

    pub(crate) fn download_subject_title(&self, subject: &DownloadSubject) -> String {
        let selected = self.selected_library();
        let loaded = selected.as_ref().map(|selected| &selected.library);
        match subject {
            DownloadSubject::Rule(downloads::DownloadRule::EntireLibrary) => tr("Entire Library"),
            DownloadSubject::Rule(downloads::DownloadRule::Favorites) => tr("Favorites"),
            DownloadSubject::Rule(downloads::DownloadRule::AllPlaylists) => tr("All Playlists"),
            DownloadSubject::Rule(downloads::DownloadRule::LatestFiveAlbums) => {
                tr("5 Latest Albums")
            }
            DownloadSubject::Track(id) => loaded
                .and_then(|loaded| loaded.track(id).ok().flatten())
                .map(|track| track.title.clone())
                .unwrap_or_else(|| tr("Track")),
            DownloadSubject::Album(id) => loaded
                .and_then(|loaded| loaded.album(id).ok().flatten())
                .map(|album| album.title.clone())
                .unwrap_or_else(|| tr("Album")),
            DownloadSubject::Artist(id) => loaded
                .and_then(|loaded| loaded.artist(id).ok().flatten())
                .map(|artist| artist.name.clone())
                .unwrap_or_else(|| tr("Artist")),
            DownloadSubject::Genre(id) => loaded
                .and_then(|loaded| loaded.genre(id).ok().flatten())
                .map(|genre| genre.name.clone())
                .unwrap_or_else(|| tr("Genre")),
            DownloadSubject::Mood(id) => selected
                .as_ref()
                .and_then(|selected| {
                    selected
                        .library
                        .moods(selected.music_folder_id.as_ref())
                        .ok()
                })
                .and_then(|moods| {
                    moods
                        .iter()
                        .find(|mood| &mood.mood.id == id)
                        .map(|mood| mood.mood.name.clone())
                })
                .unwrap_or_else(|| tr("Mood")),
            DownloadSubject::Playlist(id) => loaded
                .and_then(|loaded| loaded.playlists().ok())
                .and_then(|playlists| {
                    playlists
                        .iter()
                        .find(|playlist| &playlist.playlist.id == id)
                        .map(|playlist| playlist.playlist.name.clone())
                })
                .unwrap_or_else(|| tr("Playlist")),
            DownloadSubject::SmartPlaylist(id) => loaded
                .and_then(|loaded| loaded.smart_playlist(id).ok().flatten())
                .map(|playlist| smart_playlist_display_name(&playlist))
                .unwrap_or_else(|| tr("Smart Playlist")),
            DownloadSubject::Prepared { title, .. } => {
                title.clone().unwrap_or_else(|| tr("Selection"))
            }
        }
    }

    pub(crate) fn download_source_artwork(self: &Rc<Self>, size: i32) -> gtk::Widget {
        let bindings = self
            .selected_library()
            .as_deref()
            .map(|selected| {
                let bindings = selected
                    .library
                    .source_artwork()
                    .unwrap_or_default()
                    .iter()
                    .take(4)
                    .map(ArtworkBinding::source_artwork)
                    .collect::<Vec<_>>();
                bindings
            })
            .unwrap_or_default();
        self.cover_group_projection_for_artwork(&bindings, size, size)
            .widget()
    }

    pub(crate) fn download_subject_artwork(
        self: &Rc<Self>,
        subject: &DownloadSubject,
        size: i32,
    ) -> gtk::Widget {
        let selected = self.selected_library();
        let bindings = selected.as_ref().and_then(|selected| match subject {
            DownloadSubject::Track(id) => selected
                .library
                .track(id)
                .ok()
                .flatten()
                .map(|track| vec![ArtworkBinding::track(&track)]),
            DownloadSubject::Album(id) => selected
                .library
                .album(id)
                .ok()
                .flatten()
                .map(|album| vec![ArtworkBinding::album(&album)]),
            DownloadSubject::Artist(id) => selected
                .library
                .artist_overview(id, selected.music_folder_id.as_ref())
                .ok()
                .flatten()
                .map(|artist| vec![ArtworkBinding::artist(&artist.summary.artwork)]),
            DownloadSubject::Genre(id) => selected
                .library
                .genre_detail(id, selected.music_folder_id.as_ref())
                .ok()
                .flatten()
                .map(|genre| {
                    ArtworkBinding::genre_slots(
                        &genre.summary.genre,
                        &genre.summary.representative_albums,
                    )
                }),
            DownloadSubject::Mood(id) => selected
                .library
                .mood_detail(id, selected.music_folder_id.as_ref())
                .ok()
                .flatten()
                .map(|mood| {
                    ArtworkBinding::mood_slots(
                        &mood.summary.mood,
                        &mood.summary.representative_albums,
                    )
                }),
            DownloadSubject::Playlist(id) => selected
                .library
                .playlist_detail(id)
                .ok()
                .flatten()
                .map(|playlist| {
                    ArtworkBinding::playlist_slots(
                        &playlist.summary.playlist,
                        &playlist.summary.representative_albums,
                        self.settings.current.borrow().prefer_server_playlist_covers,
                    )
                }),
            DownloadSubject::SmartPlaylist(id) => selected
                .library
                .smart_playlist_detail(id, selected.music_folder_id.as_ref())
                .ok()
                .flatten()
                .map(|playlist| {
                    ArtworkBinding::smart_playlist_slots(
                        &playlist.summary.smart_playlist,
                        &playlist.summary.representative_albums,
                    )
                }),
            DownloadSubject::Rule(rule) => Self::download_rule_artwork(selected, *rule),
            DownloadSubject::Prepared { .. } => None,
        });
        drop(selected);
        let Some(bindings) = bindings.filter(|bindings| !bindings.is_empty()) else {
            return self.download_source_artwork(size);
        };
        if bindings.len() > 1 {
            return self
                .cover_group_projection_for_artwork(&bindings, size, size)
                .widget();
        }
        let tile = ArtworkTile::new(size);
        self.bind_artwork_tile(
            &tile,
            bindings.into_iter().next().expect("one artwork binding"),
            size,
            cover_fetch_size_for_display(size),
        );
        tile.widget()
    }

    fn download_rule_artwork(
        selected: &SelectedLibrary,
        rule: downloads::DownloadRule,
    ) -> Option<Vec<ArtworkBinding>> {
        if rule == downloads::DownloadRule::EntireLibrary {
            let folders = selected.library.music_folders().ok()?;
            let bindings = folders
                .iter()
                .filter(|folder| {
                    selected
                        .music_folder_id
                        .as_ref()
                        .is_none_or(|selected_id| &folder.id == selected_id)
                })
                .filter_map(|folder| folder.image_ref.clone())
                .take(4)
                .map(|image_ref| ArtworkBinding::source_artwork(&SourceArtwork::Native(image_ref)))
                .collect::<Vec<_>>();
            return (!bindings.is_empty()).then_some(bindings);
        }

        let tracks = match rule {
            downloads::DownloadRule::EntireLibrary => unreachable!(),
            downloads::DownloadRule::Favorites => selected
                .library
                .favorite_download_track_list(selected.music_folder_id.as_ref()),
            downloads::DownloadRule::AllPlaylists => selected
                .library
                .all_playlist_track_list(selected.music_folder_id.as_ref()),
            downloads::DownloadRule::LatestFiveAlbums => selected
                .library
                .latest_album_track_list(selected.music_folder_id.as_ref(), 5),
        }
        .ok()?;
        let mut seen = HashSet::new();
        let mut bindings = Vec::new();
        for position in 0..tracks.len() {
            let Some(track) = tracks.track(position).ok().flatten() else {
                continue;
            };
            let binding = ArtworkBinding::track(&track);
            if seen.insert(binding.clone()) {
                bindings.push(binding);
                if bindings.len() == 4 {
                    break;
                }
            }
        }
        (!bindings.is_empty()).then_some(bindings)
    }

    pub(crate) fn connect_operation_feedback(self: &Rc<Self>) {
        let close_card = self.chrome.operation_feedback.clone();
        let close_generation = Rc::clone(&self.downloads.feedback_generation);
        self.chrome
            .operation_feedback_close
            .connect_clicked(move |_| {
                close_generation.set(close_generation.get().wrapping_add(1));
                close_card.set_visible(false);
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
        });
        self.chrome.operation_feedback.add_controller(click);
    }

    pub(crate) fn download_badge(
        self: &Rc<Self>,
        collection: bool,
        downloaded: impl Fn(&SelectedLibrary) -> bool + 'static,
    ) -> gtk::Image {
        let image = gtk::Image::from_icon_name("rufin-folder-download-symbolic");
        image.add_css_class("downloaded-badge");
        image.set_pixel_size(14);
        image.set_tooltip_text(Some(&tr("Downloaded")));
        let binding = DownloadBadgeBinding {
            image: image.downgrade(),
            collection,
            downloaded: Rc::new(downloaded),
        };
        let downloaded = self
            .selected_library()
            .as_deref()
            .is_some_and(|selected| (binding.downloaded)(selected));
        self.set_download_badge_visible(&image, downloaded);
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
            self.refresh_download_badges(true);
        }
    }

    fn refresh_download_badges(&self, include_collections: bool) {
        let selected = self.selected_library();
        self.downloads.badges.borrow_mut().retain(|_, binding| {
            let Some(image) = binding.image.upgrade() else {
                return false;
            };
            if include_collections || !binding.collection {
                self.set_download_badge_visible(
                    &image,
                    selected
                        .as_ref()
                        .is_some_and(|selected| (binding.downloaded)(selected)),
                );
            }
            true
        });
    }
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
    }
}

#[cfg(test)]
mod tests {
    use downloads::{DownloadQueueItem, DownloadQueueState, DownloadSubject};
    use library::{SourceId, StreamQuality, TrackId};

    use super::{OperationFeedbackKind, operation_feedback_subtitle, reorder_queue_items};

    #[test]
    fn operation_feedback_names_the_action_before_the_track_count() {
        assert_eq!(
            operation_feedback_subtitle(&OperationFeedbackKind::DownloadStarted, "7 tracks"),
            "Download started · 7 tracks"
        );
        assert_eq!(
            operation_feedback_subtitle(&OperationFeedbackKind::DownloadQueued, "1 track"),
            "Download queued · 1 track"
        );
        assert_eq!(
            operation_feedback_subtitle(
                &OperationFeedbackKind::PlaylistAdded {
                    destination: "Road Trip".to_string(),
                },
                "7 tracks",
            ),
            "Added to Road Trip · 7 tracks"
        );
    }

    #[test]
    fn optimistic_queue_order_moves_the_active_job_to_its_drop_position() {
        let source_id = SourceId::fake(1);
        let mut jobs = ["active", "second", "third"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| DownloadQueueItem {
                id: id.to_string(),
                source_id: source_id.clone(),
                subject: DownloadSubject::Track(TrackId::fake(index)),
                quality: StreamQuality::Original,
                completed_tracks: 0,
                total_tracks: 1,
                state: if index == 0 {
                    DownloadQueueState::Downloading
                } else {
                    DownloadQueueState::Queued
                },
            })
            .collect::<Vec<_>>();

        assert!(reorder_queue_items(&mut jobs, "active", "third", true));
        assert_eq!(
            jobs.iter().map(|job| job.id.as_str()).collect::<Vec<_>>(),
            ["second", "third", "active"]
        );
    }
}
