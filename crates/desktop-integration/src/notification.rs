use gio::prelude::ApplicationExt;
#[cfg(target_os = "windows")]
use gio::prelude::FileExt;
#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
use glib::prelude::*;
use playback::{PlaybackView, TransportStatus};
use std::cell::Cell;
#[cfg(target_os = "windows")]
use std::cell::RefCell;
#[cfg(not(target_os = "windows"))]
use std::fs;
#[cfg(not(target_os = "windows"))]
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
#[cfg(any(
    target_os = "windows",
    all(unix, not(any(target_os = "android", target_vendor = "apple")))
))]
use tracing::warn;
#[cfg(target_os = "windows")]
use windows::Data::Xml::Dom::XmlDocument;
#[cfg(target_os = "windows")]
use windows::Foundation::TypedEventHandler;
#[cfg(target_os = "windows")]
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier};
#[cfg(target_os = "windows")]
use windows::core::{HSTRING, IInspectable};

#[cfg(any(
    target_os = "windows",
    all(unix, not(any(target_os = "android", target_vendor = "apple")))
))]
use crate::APP_ID;
#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
use app_identity::DISPLAY_NAME;
#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
const APP_NAME: &str = DISPLAY_NAME;
#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
const DBUS_TIMEOUT_MSEC: i32 = 1_000;
#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
const NOTIFICATIONS_BUS_NAME: &str = "org.freedesktop.Notifications";
#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
const NOTIFICATIONS_INTERFACE: &str = "org.freedesktop.Notifications";
#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
const NOTIFICATIONS_OBJECT_PATH: &str = "/org/freedesktop/Notifications";
#[cfg(not(target_os = "windows"))]
const NOW_PLAYING_NOTIFICATION_ID: &str = "now-playing";
#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
const NOW_PLAYING_NOTIFICATION_TIMEOUT_MSEC: i32 = -1;
#[cfg(not(target_os = "windows"))]
const NOTIFICATION_ARTWORK_SIZE: u32 = 96;
#[cfg(target_os = "windows")]
const WINDOWS_NOTIFICATION_TAG: &str = "now-playing";
#[cfg(target_os = "windows")]
const WINDOWS_NOTIFICATION_GROUP: &str = "rufin";

#[cfg(not(target_os = "windows"))]
fn notification_icon_path(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    notification_icon_bytes(&bytes)
}

#[cfg(not(target_os = "windows"))]
fn notification_icon_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    artwork::square_thumbnail_png(bytes, NOTIFICATION_ARTWORK_SIZE).ok()
}

#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct NativeNowPlayingNotification {
    title: String,
    body: String,
    artwork_uri: Option<String>,
}

pub struct Notifications {
    application: gio::Application,
    #[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
    notification_id: Cell<u32>,
    notification_run: Cell<Option<playback::RunId>>,
    sendable: Cell<bool>,
    #[cfg(target_os = "windows")]
    windows_toast: RefCell<Option<WindowsToast>>,
    #[cfg(target_os = "windows")]
    activation_tx: async_channel::Sender<()>,
}

impl Notifications {
    pub fn new(application: gio::Application) -> Rc<Self> {
        #[cfg(target_os = "windows")]
        let (activation_tx, activation_rx) = async_channel::bounded(1);
        let notifications = Rc::new(Self {
            application,
            #[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
            notification_id: Cell::new(0),
            notification_run: Cell::new(None),
            sendable: Cell::new(false),
            #[cfg(target_os = "windows")]
            windows_toast: RefCell::new(None),
            #[cfg(target_os = "windows")]
            activation_tx,
        });
        #[cfg(target_os = "windows")]
        {
            let weak = Rc::downgrade(&notifications);
            glib::spawn_future_local(async move {
                while activation_rx.recv().await.is_ok() {
                    let Some(notifications) = weak.upgrade() else {
                        break;
                    };
                    notifications.application.activate();
                }
            });
            if let Err(error) = remove_windows_notification_history() {
                warn!(%error, "failed to clear Windows notification history");
            }
        }
        notifications
    }

    pub fn observe(
        self: &Rc<Self>,
        player: Option<&PlaybackView>,
        enabled: bool,
        artwork_path: Option<PathBuf>,
        refresh_current: bool,
    ) {
        self.sendable
            .set(now_playing_notification_can_send(enabled, player));
        if now_playing_notification_should_withdraw(enabled, player) {
            self.withdraw();
            return;
        }
        if !self.sendable.get() {
            return;
        }
        let Some(player) = player else {
            return;
        };
        let Some(run) = player
            .transport
            .current
            .as_ref()
            .and_then(|media| media.id.run)
        else {
            return;
        };
        if !refresh_current && self.notification_run.get() == Some(run) {
            return;
        }
        self.notification_run.set(Some(run));
        let Some(entry) = player.transport.current.as_ref() else {
            return;
        };
        let title = entry.track.title.clone();
        let body = format!("{} - {}", entry.track.artist, entry.track.album);
        let notifications = Rc::clone(self);
        glib::spawn_future_local(async move {
            if !notifications.matches(run) {
                return;
            }

            #[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
            {
                let artwork_uri = artwork_path
                    .as_deref()
                    .and_then(now_playing_notification_artwork_uri);
                let native_notification = NativeNowPlayingNotification {
                    title: title.clone(),
                    body: body.clone(),
                    artwork_uri,
                };
                let replaces_id = notifications.notification_id.get();
                match send_freedesktop_now_playing_notification(&native_notification, replaces_id)
                    .await
                {
                    Ok(notification_id) => {
                        if notifications.matches(run) {
                            notifications
                                .application
                                .withdraw_notification(NOW_PLAYING_NOTIFICATION_ID);
                            notifications.notification_id.set(notification_id);
                        } else {
                            close_freedesktop_now_playing_notification(notification_id).await;
                        }
                    }
                    Err(error) => {
                        warn!(%error, "failed to send Freedesktop now-playing notification");
                        if replaces_id != 0 {
                            notifications.notification_id.set(0);
                            close_freedesktop_now_playing_notification(replaces_id).await;
                        }
                        if notifications.matches(run) {
                            send_gio_now_playing_notification(
                                &notifications.application,
                                title,
                                body,
                                artwork_path,
                            )
                            .await;
                        }
                    }
                }
            }
            #[cfg(target_os = "windows")]
            if notifications.matches(run) {
                notifications.clear_windows_toast();
                let artwork_uri = artwork_path
                    .as_deref()
                    .map(|path| gio::File::for_path(path).uri().to_string());
                match send_windows_now_playing_notification(
                    &title,
                    &body,
                    artwork_uri.as_deref(),
                    notifications.activation_tx.clone(),
                ) {
                    Ok(toast) => {
                        notifications.windows_toast.replace(Some(toast));
                    }
                    Err(error) => warn!(%error, "failed to send Windows now-playing notification"),
                }
            }
            #[cfg(all(
                not(target_os = "windows"),
                not(all(unix, not(any(target_os = "android", target_vendor = "apple"))))
            ))]
            if notifications.matches(run) {
                send_gio_now_playing_notification(
                    &notifications.application,
                    title,
                    body,
                    artwork_path,
                )
                .await;
            }
        });
    }

    pub fn withdraw(&self) {
        #[cfg(not(target_os = "windows"))]
        self.application
            .withdraw_notification(NOW_PLAYING_NOTIFICATION_ID);
        #[cfg(target_os = "windows")]
        self.clear_windows_toast();
        self.notification_run.set(None);
        self.sendable.set(false);
        #[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
        {
            let notification_id = self.notification_id.replace(0);
            if notification_id != 0 {
                glib::spawn_future_local(async move {
                    close_freedesktop_now_playing_notification(notification_id).await;
                });
            }
        }
    }

    fn matches(&self, run: playback::RunId) -> bool {
        self.sendable.get() && self.notification_run.get() == Some(run)
    }

    #[cfg(target_os = "windows")]
    fn clear_windows_toast(&self) {
        if let Some(toast) = self.windows_toast.borrow_mut().take()
            && let Err(error) = toast.notifier.Hide(&toast.notification)
        {
            warn!(%error, "failed to hide Windows now-playing notification");
        }
        if let Err(error) = remove_windows_notification_history() {
            warn!(%error, "failed to remove Windows now-playing notification");
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for Notifications {
    fn drop(&mut self) {
        self.clear_windows_toast();
    }
}

#[cfg(target_os = "windows")]
struct WindowsToast {
    notifier: ToastNotifier,
    notification: ToastNotification,
}

#[cfg(target_os = "windows")]
fn send_windows_now_playing_notification(
    title: &str,
    body: &str,
    artwork_uri: Option<&str>,
    activation_tx: async_channel::Sender<()>,
) -> Result<WindowsToast, String> {
    let document = XmlDocument::new()
        .map_err(|error| format!("failed to create the toast document: {error}"))?;
    document
        .LoadXml(&HSTRING::from(windows_toast_xml(title, body, artwork_uri)))
        .map_err(|error| format!("failed to load the toast document: {error}"))?;
    let notification = ToastNotification::CreateToastNotification(&document)
        .map_err(|error| format!("failed to create the toast notification: {error}"))?;
    notification
        .SetTag(&HSTRING::from(WINDOWS_NOTIFICATION_TAG))
        .map_err(|error| format!("failed to identify the toast notification: {error}"))?;
    notification
        .SetGroup(&HSTRING::from(WINDOWS_NOTIFICATION_GROUP))
        .map_err(|error| format!("failed to group the toast notification: {error}"))?;
    notification
        .Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
            move |_, _| {
                let _ = activation_tx.try_send(());
                Ok(())
            },
        ))
        .map_err(|error| format!("failed to listen for toast activation: {error}"))?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
        .map_err(|error| format!("failed to create the toast notifier: {error}"))?;
    notifier
        .Show(&notification)
        .map_err(|error| format!("failed to show the toast notification: {error}"))?;
    Ok(WindowsToast {
        notifier,
        notification,
    })
}

#[cfg(target_os = "windows")]
fn remove_windows_notification_history() -> Result<(), String> {
    let history = ToastNotificationManager::History()
        .map_err(|error| format!("failed to access notification history: {error}"))?;
    history
        .RemoveGroupedTagWithId(
            &HSTRING::from(WINDOWS_NOTIFICATION_TAG),
            &HSTRING::from(WINDOWS_NOTIFICATION_GROUP),
            &HSTRING::from(APP_ID),
        )
        .map_err(|error| format!("failed to remove notification history: {error}"))
}

#[cfg(target_os = "windows")]
fn windows_toast_xml(title: &str, body: &str, artwork_uri: Option<&str>) -> String {
    let artwork = artwork_uri.map_or_else(String::new, |uri| {
        format!(
            r#"<image placement="appLogoOverride" hint-crop="circle" src="{}"/>"#,
            escape_xml(uri)
        )
    });
    format!(
        r#"<toast><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text>{artwork}</binding></visual></toast>"#,
        escape_xml(title),
        escape_xml(body)
    )
}

#[cfg(target_os = "windows")]
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
fn now_playing_notification_artwork_uri(path: &Path) -> Option<String> {
    glib::filename_to_uri(path, None)
        .ok()
        .map(|uri| uri.to_string())
}

#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
fn now_playing_notification_hints(artwork_uri: Option<&str>) -> glib::VariantDict {
    let hints = glib::VariantDict::new(None);
    hints.insert("desktop-entry", APP_ID);
    hints.insert("transient", true);
    if let Some(uri) = artwork_uri {
        hints.insert("image-path", uri);
        hints.insert("image_path", uri);
    }
    hints
}

#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
fn now_playing_notification_parameters(
    notification: &NativeNowPlayingNotification,
    replaces_id: u32,
) -> glib::Variant {
    (
        APP_NAME.to_string(),
        replaces_id,
        APP_ID.to_string(),
        notification.title.clone(),
        notification.body.clone(),
        Vec::<String>::new(),
        now_playing_notification_hints(notification.artwork_uri.as_deref()),
        NOW_PLAYING_NOTIFICATION_TIMEOUT_MSEC,
    )
        .to_variant()
}

#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
async fn send_freedesktop_now_playing_notification(
    notification: &NativeNowPlayingNotification,
    replaces_id: u32,
) -> Result<u32, glib::Error> {
    let connection = gio::bus_get_future(gio::BusType::Session).await?;
    let parameters = now_playing_notification_parameters(notification, replaces_id);
    let reply_type = glib::VariantTy::new("(u)").ok();
    let reply = connection
        .call_future(
            Some(NOTIFICATIONS_BUS_NAME),
            NOTIFICATIONS_OBJECT_PATH,
            NOTIFICATIONS_INTERFACE,
            "Notify",
            Some(&parameters),
            reply_type,
            gio::DBusCallFlags::NONE,
            DBUS_TIMEOUT_MSEC,
        )
        .await?;
    Ok(reply.try_child_get::<u32>(0).ok().flatten().unwrap_or(0))
}

#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
async fn close_freedesktop_now_playing_notification(notification_id: u32) {
    if notification_id == 0 {
        return;
    }
    let Ok(connection) = gio::bus_get_future(gio::BusType::Session).await else {
        return;
    };
    let parameters = (notification_id,).to_variant();
    let _closed = connection
        .call_future(
            Some(NOTIFICATIONS_BUS_NAME),
            NOTIFICATIONS_OBJECT_PATH,
            NOTIFICATIONS_INTERFACE,
            "CloseNotification",
            Some(&parameters),
            None,
            gio::DBusCallFlags::NONE,
            DBUS_TIMEOUT_MSEC,
        )
        .await;
}

#[cfg(not(target_os = "windows"))]
async fn send_gio_now_playing_notification(
    application: &gio::Application,
    title: String,
    body: String,
    artwork_path: Option<PathBuf>,
) {
    let icon_bytes = match artwork_path {
        Some(path) => gio::spawn_blocking(move || notification_icon_path(&path))
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let notification = gio::Notification::new(&title);
    notification.set_body(Some(&body));
    if let Some(bytes) = icon_bytes {
        let bytes = glib::Bytes::from_owned(bytes);
        notification.set_icon(&gio::BytesIcon::new(&bytes));
    }
    application.send_notification(Some(NOW_PLAYING_NOTIFICATION_ID), &notification);
}

pub fn now_playing_notification_can_send(enabled: bool, player: Option<&PlaybackView>) -> bool {
    enabled
        && player.is_some_and(|player| {
            matches!(
                player.transport.state,
                TransportStatus::Playing | TransportStatus::Buffering
            ) && player.transport.current.is_some()
        })
}

pub fn now_playing_notification_should_withdraw(
    enabled: bool,
    player: Option<&PlaybackView>,
) -> bool {
    !enabled
        || player.is_none_or(|player| {
            player.transport.current.is_none() || player.transport.state == TransportStatus::Stopped
        })
}

#[cfg(test)]
mod tests {
    #[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
    use std::path::Path;

    #[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
    use super::{
        APP_ID, APP_NAME, NOW_PLAYING_NOTIFICATION_TIMEOUT_MSEC, NativeNowPlayingNotification,
        now_playing_notification_artwork_uri, now_playing_notification_hints,
        now_playing_notification_parameters,
    };
    #[cfg(not(target_os = "windows"))]
    use super::{NOTIFICATION_ARTWORK_SIZE, notification_icon_bytes};

    #[cfg(target_os = "windows")]
    use super::windows_toast_xml;

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_toast_escapes_track_text_and_artwork_uri() {
        let xml = windows_toast_xml(
            "A & <B>",
            "Artist's \"Album\"",
            Some("file:///C:/Cover & Art.png"),
        );

        assert_eq!(
            xml,
            r#"<toast><visual><binding template="ToastGeneric"><text>A &amp; &lt;B&gt;</text><text>Artist&apos;s &quot;Album&quot;</text><image placement="appLogoOverride" hint-crop="circle" src="file:///C:/Cover &amp; Art.png"/></binding></visual></toast>"#
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn notification_artwork_is_square_and_thumbnail_sized() {
        let cover = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/icons/hicolor/512x512/apps/io.github.screwys.Rufin.png"
        ));
        let bytes = notification_icon_bytes(cover).expect("notification bytes");
        let icon = artwork::decode_rgba(&bytes, u32::MAX).expect("notification image");

        assert_eq!(icon.width(), NOTIFICATION_ARTWORK_SIZE);
        assert_eq!(icon.height(), NOTIFICATION_ARTWORK_SIZE);
    }

    #[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
    #[test]
    fn freedesktop_notification_hints_include_identity_and_artwork() {
        let hints = now_playing_notification_hints(Some("file:///music/cover.png"));

        assert_eq!(
            hints.lookup::<String>("desktop-entry").unwrap().as_deref(),
            Some(APP_ID)
        );
        assert_eq!(hints.lookup::<bool>("transient").unwrap(), Some(true));
        assert_eq!(
            hints.lookup::<String>("image-path").unwrap().as_deref(),
            Some("file:///music/cover.png")
        );
        assert_eq!(
            hints.lookup::<String>("image_path").unwrap().as_deref(),
            Some("file:///music/cover.png")
        );
    }

    #[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
    #[test]
    fn freedesktop_notification_parameters_replace_the_previous_notification() {
        let notification = NativeNowPlayingNotification {
            title: "Track".to_string(),
            body: "Artist - Album".to_string(),
            artwork_uri: None,
        };
        let parameters = now_playing_notification_parameters(&notification, 41);

        assert_eq!(
            parameters.try_child_get::<String>(0).unwrap().as_deref(),
            Some(APP_NAME)
        );
        assert_eq!(parameters.try_child_get::<u32>(1).unwrap(), Some(41));
        assert_eq!(
            parameters.try_child_get::<String>(3).unwrap().as_deref(),
            Some("Track")
        );
        assert_eq!(
            parameters.try_child_get::<String>(4).unwrap().as_deref(),
            Some("Artist - Album")
        );
        assert_eq!(
            parameters.try_child_get::<i32>(7).unwrap(),
            Some(NOW_PLAYING_NOTIFICATION_TIMEOUT_MSEC)
        );
    }

    #[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
    #[test]
    fn notification_artwork_path_becomes_a_file_uri() {
        let uri = now_playing_notification_artwork_uri(Path::new("/tmp/cover art.png"))
            .expect("absolute artwork path");

        assert_eq!(uri, "file:///tmp/cover%20art.png");
    }
}
