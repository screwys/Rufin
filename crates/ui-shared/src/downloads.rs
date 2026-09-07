use adw::prelude::*;
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
