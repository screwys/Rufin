use adw::prelude::*;
use artwork::ArtworkBinding;
use downloads::{DownloadQueueSnapshot, DownloadSubject};
use gtk::glib;
use localization::tr;
use sources::SourceId;
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

pub struct OperationFeedback {
    pub subject: DownloadSubject,
    pub preview_uris: Vec<String>,
    pub item_count: usize,
    pub kind: OperationFeedbackKind,
}

pub struct DownloadsState {
    settings: Rc<crate::settings::SettingsState>,
    pub snapshots: RefCell<HashMap<Option<SourceId>, Arc<DownloadQueueSnapshot>>>,
    queue_refresh: RefCell<Option<Weak<dyn Fn()>>>,
    badges: Rc<RefCell<HashMap<usize, DownloadBadgeBinding>>>,
}

impl DownloadsState {
    pub fn new(settings: Rc<crate::settings::SettingsState>) -> Rc<Self> {
        Rc::new(Self {
            settings,
            snapshots: Default::default(),
            queue_refresh: Default::default(),
            badges: Default::default(),
        })
    }

    pub fn set_queue_refresh(&self, refresh: &Rc<dyn Fn()>) {
        self.queue_refresh.replace(Some(Rc::downgrade(refresh)));
    }

    pub fn refresh_queue(&self) {
        let refresh = self.queue_refresh.borrow().as_ref().and_then(Weak::upgrade);
        if let Some(refresh) = refresh {
            refresh();
        } else {
            self.queue_refresh.borrow_mut().take();
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
        image.set_visible(self.settings.current.borrow().show_downloaded_badges && downloaded);
    }
    pub fn set_downloaded_badges_visible(self: &Rc<Self>, visible: bool) {
        if self
            .settings
            .set_app_setting("downloaded badge setting", visible, |settings| {
                &mut settings.show_downloaded_badges
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

pub fn download_subject_title(subject: &DownloadSubject) -> String {
    match subject {
        DownloadSubject::Rule(downloads::DownloadRule::EntireLibrary) => tr("Entire Library"),
        DownloadSubject::Rule(downloads::DownloadRule::Favorites) => tr("Favorites"),
        DownloadSubject::Rule(downloads::DownloadRule::AllPlaylists) => tr("All Playlists"),
        DownloadSubject::Rule(downloads::DownloadRule::LatestFiveAlbums) => tr("5 Latest Albums"),
        DownloadSubject::Prepared { title, .. } => title.clone().unwrap_or_else(|| tr("Selection")),
    }
}

pub fn media_artwork(
    artwork: &Rc<crate::artwork::ArtworkState>,
    database: &Arc<library::Database>,
    runtime: &tokio::runtime::Handle,
    media_uris: &[String],
    size: i32,
) -> gtk::Widget {
    let projection = artwork.cover_group_projection_for_artwork(&[], size, size);
    let widget = projection.widget();
    let database = Arc::clone(database);
    let media_uris = media_uris.iter().take(4).cloned().collect::<Vec<_>>();
    let task = runtime.spawn(async move {
        database
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
    artwork: &Rc<crate::artwork::ArtworkState>,
    database: &Arc<library::Database>,
    runtime: &tokio::runtime::Handle,
    source_id: SourceId,
    rule: downloads::DownloadRule,
    size: i32,
) -> gtk::Widget {
    let projection = artwork.cover_group_projection_for_artwork(&[], size, size);
    let widget = projection.widget();
    let database = Arc::clone(database);
    let task = runtime.spawn(async move {
        let cancellation = library::ReadCancellation::new();
        let Some(source) = database
            .cached_source(source_id.as_str(), &cancellation)
            .await?
        else {
            return Ok::<_, library::LibraryError>(Vec::new());
        };
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
