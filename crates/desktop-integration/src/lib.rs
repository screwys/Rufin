//! Desktop protocols driven by Rufin's accepted playback projection.
//!
//! Each integration keeps only the state required by its external protocol.
//! GTK window policy and Rufin's current-media authority remain with their
//! existing owners.

mod discord;
mod media_controls;
mod notification;
mod tray;

#[cfg(target_os = "windows")]
use tracing::warn;

#[cfg(any(
    target_os = "windows",
    all(unix, not(any(target_os = "android", target_vendor = "apple")))
))]
pub(crate) use app_identity::APP_ID;

pub use discord::{DEFAULT_CLIENT_ID, Discord, DisplayType, LinkType, Settings};
pub use media_controls::MediaControls;
pub use notification::{
    Notifications, now_playing_notification_can_send, now_playing_notification_should_withdraw,
};
pub use tray::{Tray, TrayIntent};

pub struct Platform {
    #[cfg(target_os = "windows")]
    _com: Option<winsafe::guard::CoUninitializeGuard>,
}

impl Platform {
    pub fn initialize() -> Self {
        #[cfg(target_os = "windows")]
        {
            let com = winsafe::CoInitializeEx(
                winsafe::co::COINIT::APARTMENTTHREADED | winsafe::co::COINIT::DISABLE_OLE1DDE,
            )
            .map_err(|error| {
                warn!(?error, "failed to initialize Windows desktop integration");
            })
            .ok();
            if let Err(error) = winsafe::SetCurrentProcessExplicitAppUserModelID(APP_ID) {
                warn!(?error, "failed to set the Windows application identity");
            }
            return Self { _com: com };
        }
        #[cfg(not(target_os = "windows"))]
        Self {}
    }
}
