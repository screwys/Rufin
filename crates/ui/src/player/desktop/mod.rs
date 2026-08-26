pub(crate) mod lifecycle;

#[cfg(target_os = "linux")]
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;
#[cfg(not(target_os = "macos"))]
use std::sync::mpsc::Receiver;
use std::sync::mpsc::{TryRecvError, channel};
use std::time::{Duration, Instant};

use adw::prelude::*;
#[cfg(target_os = "linux")]
use ashpd::desktop::background::Background;
use gtk::glib;
use playback::{CurrentMedia, PlaybackView, PositionDiscontinuity, TransportHandle};
use sources::SourceId;
use tracing::{info, warn};

use crate::Settings as UiSettings;
use crate::shell::Shell;
use crate::shell::cover::THUMB_COVER_SIZE;

#[cfg(not(target_os = "macos"))]
const TRAY_POLL_INTERVAL: Duration = Duration::from_millis(120);
const QUIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const QUIT_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackgroundAccess {
    Unavailable,
    Pending,
    Granted,
    Denied,
}

pub(crate) struct DesktopState {
    pub(crate) media_controls: Rc<desktop_integration::MediaControls>,
    pub(crate) notifications: Rc<desktop_integration::Notifications>,
    tray: RefCell<Option<desktop_integration::Tray>>,
    tray_command_source: RefCell<Option<glib::SourceId>>,
    #[cfg(target_os = "linux")]
    background_access: Cell<BackgroundAccess>,
}

impl DesktopState {
    pub(crate) fn new(application: &adw::Application, transport: TransportHandle) -> Self {
        Self {
            media_controls: desktop_integration::MediaControls::start(transport),
            notifications: desktop_integration::Notifications::new(application.clone().upcast()),
            tray: RefCell::new(None),
            tray_command_source: RefCell::new(None),
            #[cfg(target_os = "linux")]
            background_access: Cell::new(BackgroundAccess::Unavailable),
        }
    }
}

pub(crate) fn now_playing_notification_can_send(
    settings: &UiSettings,
    player: Option<&PlaybackView>,
) -> bool {
    desktop_integration::now_playing_notification_can_send(settings.allows_notifications(), player)
}

pub(crate) fn now_playing_notification_should_withdraw(
    settings: &UiSettings,
    player: Option<&PlaybackView>,
) -> bool {
    desktop_integration::now_playing_notification_should_withdraw(
        settings.allows_notifications(),
        player,
    )
}

impl Shell {
    pub(crate) fn request_quit(self: &Rc<Self>, reason: &'static str) {
        if self.quitting.replace(true) {
            return;
        }
        info!(reason, "stopping Rufin");
        self.save_window_state();
        self.chrome.window.set_visible(false);
        self.shutdown_tray();

        let transport = self.products.playback.transport.clone();
        let (completed, completion) = channel();
        if let Err(error) = std::thread::Builder::new()
            .name("rufin-shutdown".to_string())
            .spawn(move || {
                transport.shutdown();
                let _ = completed.send(());
            })
        {
            warn!(%error, "could not start playback shutdown");
            self.chrome.application.quit();
            return;
        }

        let application = self.chrome.application.clone();
        let started_at = Instant::now();
        glib::timeout_add_local(QUIT_POLL_INTERVAL, move || match completion.try_recv() {
            Ok(()) => {
                info!(
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "playback shutdown finished"
                );
                application.quit();
                glib::ControlFlow::Break
            }
            Err(TryRecvError::Disconnected) => {
                warn!(
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "playback shutdown worker stopped before reporting completion"
                );
                application.quit();
                glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) if started_at.elapsed() >= QUIT_TIMEOUT => {
                warn!(
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "playback shutdown did not finish before the quit deadline"
                );
                application.quit();
                glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
        });
    }

    pub(crate) fn notify_now_playing(self: &Rc<Self>, player: Option<&PlaybackView>) {
        self.observe_now_playing_notification(player, false);
    }

    pub(crate) fn refresh_now_playing_notification(self: &Rc<Self>, player: Option<&PlaybackView>) {
        self.observe_now_playing_notification(player, true);
    }

    fn observe_now_playing_notification(
        self: &Rc<Self>,
        player: Option<&PlaybackView>,
        refresh_current: bool,
    ) {
        let source_id = self
            .selected_library()
            .as_deref()
            .map(|selected| selected.artwork.source_id.clone());
        let artwork = player.and_then(|player| {
            player.transport.current.as_deref().and_then(|media| {
                self.current_playback_cached_artwork_path(
                    source_id.as_ref()?,
                    media,
                    THUMB_COVER_SIZE,
                )
                .map(|artwork| artwork.path)
            })
        });
        self.desktop.notifications.observe(
            player,
            self.settings.current.borrow().allows_notifications(),
            artwork,
            refresh_current,
        );
    }

    pub(crate) fn withdraw_now_playing_notification(&self) {
        self.desktop.notifications.withdraw();
    }

    pub(crate) fn update_media_controls(&self) {
        self.update_media_controls_after(None);
    }

    pub(crate) fn update_media_controls_after(&self, discontinuity: Option<PositionDiscontinuity>) {
        let playback = self.selected_playback();
        let source_id = self
            .selected_library()
            .as_deref()
            .map(|selected| selected.artwork.source_id.clone());
        let art_url = playback.as_ref().and_then(|playback| {
            playback
                .transport
                .current
                .as_deref()
                .and_then(|media| self.current_art_url(source_id.as_ref()?, media))
        });
        self.desktop
            .media_controls
            .observe(playback.as_deref(), art_url, discontinuity);
    }

    pub(crate) fn update_media_controls_position_after(
        &self,
        position_millis: Option<u64>,
        discontinuity: Option<PositionDiscontinuity>,
    ) {
        self.desktop
            .media_controls
            .observe_position(position_millis, discontinuity);
    }

    fn current_art_url(&self, source_id: &SourceId, media: &CurrentMedia) -> Option<String> {
        let artwork =
            self.current_playback_cached_artwork_path(source_id, media, THUMB_COVER_SIZE)?;
        glib::filename_to_uri(artwork.path, None)
            .ok()
            .map(|uri| uri.to_string())
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn set_tray_enabled(self: &Rc<Self>, enabled: bool) {
        if self
            .update_app_settings("tray setting", |settings| {
                if settings.tray_enabled == enabled
                    || (!enabled && settings.keep_running_after_close)
                {
                    return false;
                }
                settings.tray_enabled = enabled;
                if !enabled {
                    settings.start_minimized = false;
                }
                true
            })
            .is_none()
        {
            return;
        }
        if enabled {
            self.ensure_tray();
        } else {
            self.shutdown_tray();
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn set_keep_running_after_close(self: &Rc<Self>, enabled: bool) {
        if self
            .update_app_settings("keep running after close setting", |settings| {
                if settings.keep_running_after_close == enabled {
                    return false;
                }
                settings.keep_running_after_close = enabled;
                if enabled {
                    settings.tray_enabled = true;
                }
                true
            })
            .is_none()
        {
            return;
        }
        if enabled {
            self.ensure_tray();
            self.request_background_access();
        }
    }

    #[cfg(target_os = "linux")]
    fn request_background_access(self: &Rc<Self>) {
        let settings = self.settings.current.borrow();
        if !ashpd::is_sandboxed()
            || (!settings.keep_running_after_close && !settings.start_minimized)
            || matches!(
                self.desktop.background_access.get(),
                BackgroundAccess::Pending | BackgroundAccess::Granted
            )
        {
            return;
        }
        self.desktop
            .background_access
            .set(BackgroundAccess::Pending);
        let shell = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let reason = localization::tr("Keep Rufin running after closing the window");
            let access = match Background::request()
                .reason(reason.as_str())
                .auto_start(false)
                .send()
                .await
                .and_then(|request| request.response())
            {
                Ok(response) if response.run_in_background() => {
                    info!("background access granted");
                    BackgroundAccess::Granted
                }
                Ok(_) => {
                    warn!("background access was not granted");
                    BackgroundAccess::Denied
                }
                Err(error) => {
                    warn!(%error, "could not request background access");
                    BackgroundAccess::Denied
                }
            };
            let Some(shell) = shell.upgrade() else {
                return;
            };
            shell.desktop.background_access.set(access);
            let tray_unavailable = shell.desktop.tray.borrow().is_none();
            if access == BackgroundAccess::Denied
                && shell.settings.current.borrow().keep_running_after_close
                && !shell.chrome.window.is_visible()
                && tray_unavailable
            {
                shell.request_quit("background access denied");
            }
        });
    }

    #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
    fn request_background_access(self: &Rc<Self>) {}

    #[cfg(target_os = "linux")]
    fn background_access_available(&self) -> bool {
        matches!(
            self.desktop.background_access.get(),
            BackgroundAccess::Pending | BackgroundAccess::Granted
        )
    }

    #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
    fn background_access_available(&self) -> bool {
        false
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn set_start_minimized_enabled(self: &Rc<Self>, enabled: bool) {
        if self
            .update_app_settings("start minimized setting", |settings| {
                if settings.start_minimized == enabled || (enabled && !settings.tray_enabled) {
                    return false;
                }
                settings.start_minimized = enabled;
                true
            })
            .is_none()
        {
            return;
        }
        if enabled {
            self.ensure_tray();
            self.request_background_access();
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn ensure_tray(self: &Rc<Self>) -> bool {
        if self.desktop.tray.borrow().is_some() {
            return true;
        }
        let private_mode = self.settings.current.borrow().private_mode;
        let (tray, receiver) = match desktop_integration::Tray::start(private_mode) {
            Ok(started) => started,
            Err(error) => {
                warn!(%error);
                return false;
            }
        };
        *self.desktop.tray.borrow_mut() = Some(tray);
        self.install_tray_command_pump(receiver);
        true
    }

    fn shutdown_tray(&self) {
        if let Some(source) = self.desktop.tray_command_source.borrow_mut().take() {
            source.remove();
        }
        if let Some(tray) = self.desktop.tray.borrow_mut().take() {
            tray.shutdown();
        }
    }

    pub(crate) fn refresh_tray_private_mode(&self) {
        let private_mode = self.settings.current.borrow().private_mode;
        if let Some(tray) = self.desktop.tray.borrow().as_ref() {
            tray.set_private_mode(private_mode);
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn install_tray_command_pump(
        self: &Rc<Self>,
        receiver: Receiver<desktop_integration::TrayIntent>,
    ) {
        if let Some(source) = self.desktop.tray_command_source.borrow_mut().take() {
            source.remove();
        }
        let shell = Rc::clone(self);
        let source = glib::timeout_add_local(TRAY_POLL_INTERVAL, move || {
            while let Ok(intent) = receiver.try_recv() {
                match intent {
                    desktop_integration::TrayIntent::Present => shell.present_from_tray(),
                    desktop_integration::TrayIntent::PlayPause => {
                        shell.products.playback.transport.play_pause();
                    }
                    desktop_integration::TrayIntent::PreviousTrack => {
                        shell.products.playback.transport.previous();
                    }
                    desktop_integration::TrayIntent::NextTrack => {
                        shell.products.playback.transport.next();
                    }
                    desktop_integration::TrayIntent::TogglePrivateMode => {
                        let enabled = !shell.settings.current.borrow().private_mode;
                        shell.set_private_mode(enabled);
                    }
                    desktop_integration::TrayIntent::Quit => {
                        shell.request_quit("tray action");
                        return glib::ControlFlow::Break;
                    }
                }
            }
            glib::ControlFlow::Continue
        });
        *self.desktop.tray_command_source.borrow_mut() = Some(source);
    }

    #[cfg(not(target_os = "macos"))]
    fn present_from_tray(&self) {
        crate::application::present_window(&self.chrome.window);
    }
}

pub(crate) fn install_desktop_lifecycle(shell: &Rc<Shell>) {
    #[cfg(not(target_os = "macos"))]
    {
        let settings = shell.settings.current.borrow().clone();
        if settings.tray_enabled {
            shell.ensure_tray();
        }
        if settings.keep_running_after_close || settings.start_minimized {
            shell.request_background_access();
        }
    }
    let close_shell = Rc::clone(shell);
    shell
        .chrome
        .window
        .connect_close_request(move |_| close_window(&close_shell));
}

#[cfg(target_os = "macos")]
fn close_window(shell: &Rc<Shell>) -> glib::Propagation {
    shell.save_window_state();
    if let Err(error) =
        gtk::prelude::WidgetExt::activate_action(&shell.chrome.window, "gtkinternal.hide", None)
    {
        warn!(%error, "could not hide Rufin");
    }
    glib::Propagation::Stop
}

#[cfg(not(target_os = "macos"))]
fn close_window(shell: &Rc<Shell>) -> glib::Propagation {
    let settings = shell.settings.current.borrow().clone();
    let tray_available = settings.tray_enabled && shell.ensure_tray();
    if settings.keep_running_after_close {
        shell.request_background_access();
    }
    if close_should_hide(
        settings.keep_running_after_close,
        tray_available,
        host_is_sandboxed(),
        shell.background_access_available(),
    ) {
        shell.save_window_state();
        shell.chrome.window.set_visible(false);
    } else {
        shell.request_quit("window close");
    }
    glib::Propagation::Stop
}

#[cfg(not(target_os = "macos"))]
fn close_should_hide(
    keep_running: bool,
    tray_available: bool,
    sandboxed: bool,
    background_access_available: bool,
) -> bool {
    if !keep_running {
        return false;
    }
    if tray_available {
        return true;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return !sandboxed || background_access_available;
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = (sandboxed, background_access_available);
        false
    }
}

#[cfg(not(target_os = "macos"))]
fn host_is_sandboxed() -> bool {
    #[cfg(target_os = "linux")]
    {
        return ashpd::is_sandboxed();
    }
    #[cfg(not(target_os = "linux"))]
    false
}

#[cfg(target_os = "macos")]
pub(crate) fn present_initial_window(shell: &Rc<Shell>, _force_visible: bool) {
    crate::application::present_window(&shell.chrome.window);
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn present_initial_window(shell: &Rc<Shell>, force_visible: bool) {
    let settings = shell.settings.current.borrow().clone();
    let tray_available = settings.tray_enabled && settings.start_minimized && shell.ensure_tray();
    if !force_visible && settings.tray_enabled && settings.start_minimized && tray_available {
        shell.chrome.window.set_visible(false);
    } else {
        crate::application::present_window(&shell.chrome.window);
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::close_should_hide;

    #[test]
    fn close_setting_off_never_hides() {
        assert!(!close_should_hide(false, true, false, true));
    }

    #[cfg(unix)]
    #[test]
    fn unix_without_a_tray_needs_native_or_portal_support() {
        assert!(close_should_hide(true, false, false, false));
        assert!(close_should_hide(true, false, true, true));
        assert!(!close_should_hide(true, false, true, false));
    }
}
