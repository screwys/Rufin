#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayIntent {
    Present,
    PlayPause,
    PreviousTrack,
    NextTrack,
    TogglePrivateMode,
    Quit,
}

#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
mod freedesktop {
    use ksni::blocking::TrayMethods;
    use localization::tr;
    use std::sync::OnceLock;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use tracing::warn;

    use app_identity::{APP_ID, STABLE_APP_ID};

    use super::TrayIntent;

    const TRAY_ICON_SIZES: [i32; 5] = [16, 22, 24, 32, 48];
    const APP_ICON_BYTES: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../data/icons/hicolor/512x512/apps/io.github.screwys.Rufin.png"
    ));

    pub struct Tray {
        handle: ksni::blocking::Handle<RufinTray>,
    }

    #[derive(Clone)]
    pub(crate) struct RufinTray {
        sender: Sender<TrayIntent>,
        show_label: String,
        play_pause_label: String,
        previous_label: String,
        next_label: String,
        enable_private_mode_label: String,
        disable_private_mode_label: String,
        quit_label: String,
        private_mode: bool,
    }

    impl RufinTray {
        fn new(sender: Sender<TrayIntent>, private_mode: bool) -> Self {
            Self {
                sender,
                show_label: tr("Show Rufin"),
                play_pause_label: tr("Play/Pause"),
                previous_label: tr("Previous Track"),
                next_label: tr("Next Track"),
                enable_private_mode_label: tr("Enable private mode"),
                disable_private_mode_label: tr("Disable private mode"),
                quit_label: tr("Quit"),
                private_mode,
            }
        }

        fn send_command(&self, command: TrayIntent) {
            let _ = self.sender.send(command);
        }

        fn private_mode_label(&self) -> String {
            if self.private_mode {
                self.disable_private_mode_label.clone()
            } else {
                self.enable_private_mode_label.clone()
            }
        }
    }

    impl ksni::Tray for RufinTray {
        fn id(&self) -> String {
            APP_ID.to_string()
        }

        fn title(&self) -> String {
            "Rufin".to_string()
        }

        fn icon_name(&self) -> String {
            if tray_icon_pixmaps().is_empty() {
                STABLE_APP_ID.to_string()
            } else {
                String::new()
            }
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            tray_icon_pixmaps().clone()
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                icon_name: self.icon_name(),
                icon_pixmap: tray_icon_pixmaps().clone(),
                title: self.title(),
                description: String::new(),
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.send_command(TrayIntent::Present);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::{MenuItem, StandardItem};

            vec![
                StandardItem {
                    label: self.show_label.clone(),
                    activate: Box::new(|tray: &mut RufinTray| {
                        tray.send_command(TrayIntent::Present)
                    }),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: self.play_pause_label.clone(),
                    activate: Box::new(|tray: &mut RufinTray| {
                        tray.send_command(TrayIntent::PlayPause)
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: self.previous_label.clone(),
                    activate: Box::new(|tray: &mut RufinTray| {
                        tray.send_command(TrayIntent::PreviousTrack)
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: self.next_label.clone(),
                    activate: Box::new(|tray: &mut RufinTray| {
                        tray.send_command(TrayIntent::NextTrack)
                    }),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: self.private_mode_label(),
                    activate: Box::new(|tray: &mut RufinTray| {
                        tray.send_command(TrayIntent::TogglePrivateMode)
                    }),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: self.quit_label.clone(),
                    activate: Box::new(|tray: &mut RufinTray| tray.send_command(TrayIntent::Quit)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    impl Tray {
        pub fn start(private_mode: bool) -> Result<(Self, Receiver<TrayIntent>), String> {
            let (sender, receiver) = channel();
            let tray = RufinTray::new(sender, private_mode);
            let handle = tray.disable_dbus_name(true).spawn().map_err(|error| {
                format!("failed to create status notifier tray item: {error:?}")
            })?;
            Ok((Self { handle }, receiver))
        }

        pub fn set_private_mode(&self, private_mode: bool) {
            let _updated = self.handle.update(|tray| {
                tray.private_mode = private_mode;
            });
        }

        pub fn shutdown(self) {
            // KSNI queues shutdown before returning its completion awaiter.
            let _ = self.handle.shutdown();
        }
    }

    fn tray_icon_pixmaps() -> &'static Vec<ksni::Icon> {
        static ICONS: OnceLock<Vec<ksni::Icon>> = OnceLock::new();
        ICONS.get_or_init(build_tray_icon_pixmaps)
    }

    fn build_tray_icon_pixmaps() -> Vec<ksni::Icon> {
        let source = match artwork::decode_rgba(APP_ICON_BYTES, u32::MAX) {
            Ok(source) => source,
            Err(error) => {
                warn!(%error, "failed to load tray icon pixmap");
                return Vec::new();
            }
        };

        TRAY_ICON_SIZES
            .iter()
            .filter_map(|size| {
                source
                    .resized_exact(u32::try_from(*size).ok()?, u32::try_from(*size).ok()?)
                    .ok()
                    .and_then(|image| tray_icon_from_rgba(&image))
            })
            .collect()
    }

    fn tray_icon_from_rgba(image: &artwork::RgbaImage) -> Option<ksni::Icon> {
        let width = i32::try_from(image.width()).ok()?;
        let height = i32::try_from(image.height()).ok()?;
        let rowstride = usize::try_from(image.row_stride()).ok()?;
        let width_usize = usize::try_from(width).ok()?;
        let height_usize = usize::try_from(height).ok()?;
        let pixels = image.rgba();
        let mut data = Vec::with_capacity(width_usize * height_usize * 4);
        for y in 0..height_usize {
            let row = y.checked_mul(rowstride)?;
            for x in 0..width_usize {
                let offset = row.checked_add(x.checked_mul(4)?)?;
                let end = offset.checked_add(4)?;
                let pixel = pixels.get(offset..end)?;
                data.extend_from_slice(&[pixel[3], pixel[0], pixel[1], pixel[2]]);
            }
        }
        Some(ksni::Icon {
            width,
            height,
            data,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::{RufinTray, TRAY_ICON_SIZES, tray_icon_pixmaps};
        use crate::tray::TrayIntent;
        use ksni::Tray;
        use ksni::menu::MenuItem;
        use std::sync::mpsc::channel;

        #[test]
        fn tray_use_controls() {
            let (sender, _receiver) = channel();
            let tray = RufinTray::new(sender, false);
            let items = tray.menu();
            let labels = standard_items(&items)
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>();

            assert_eq!(
                labels,
                vec![
                    "Show Rufin",
                    "Play/Pause",
                    "Previous Track",
                    "Next Track",
                    "Enable private mode",
                    "Quit"
                ]
            );
            assert!(
                standard_items(&items)
                    .iter()
                    .all(|item| item.icon_name.is_empty() && item.shortcut.is_empty())
            );
        }

        #[test]
        fn tray_match_state() {
            let (sender, _receiver) = channel();
            let disabled = RufinTray::new(sender.clone(), false);
            let enabled = RufinTray::new(sender, true);

            assert!(standard_labels(&disabled.menu()).contains(&"Enable private mode"));
            assert!(standard_labels(&enabled.menu()).contains(&"Disable private mode"));
        }

        #[test]
        fn tray_playback_command() {
            let (sender, receiver) = channel();
            let mut tray = RufinTray::new(sender, false);
            let mut items = tray.menu();

            activate_standard_item(&mut items, &mut tray, "Play/Pause");
            activate_standard_item(&mut items, &mut tray, "Previous Track");
            activate_standard_item(&mut items, &mut tray, "Next Track");
            activate_standard_item(&mut items, &mut tray, "Enable private mode");

            assert_eq!(receiver.recv().ok(), Some(TrayIntent::PlayPause));
            assert_eq!(receiver.recv().ok(), Some(TrayIntent::PreviousTrack));
            assert_eq!(receiver.recv().ok(), Some(TrayIntent::NextTrack));
            assert_eq!(receiver.recv().ok(), Some(TrayIntent::TogglePrivateMode));
        }

        #[test]
        fn tray_icon_size() {
            let sizes = tray_icon_pixmaps()
                .iter()
                .map(|icon| (icon.width, icon.height))
                .collect::<Vec<_>>();

            assert_eq!(
                sizes,
                TRAY_ICON_SIZES
                    .iter()
                    .map(|size| (*size, *size))
                    .collect::<Vec<_>>()
            );
        }

        fn standard_items<T>(items: &[MenuItem<T>]) -> Vec<&ksni::menu::StandardItem<T>> {
            items
                .iter()
                .filter_map(|item| match item {
                    MenuItem::Standard(item) => Some(item),
                    _ => None,
                })
                .collect()
        }

        fn standard_labels<T>(items: &[MenuItem<T>]) -> Vec<&str> {
            standard_items(items)
                .iter()
                .map(|item| item.label.as_str())
                .collect()
        }

        fn activate_standard_item(
            items: &mut [MenuItem<RufinTray>],
            tray: &mut RufinTray,
            label: &str,
        ) {
            let item = items
                .iter_mut()
                .find_map(|item| match item {
                    MenuItem::Standard(item) if item.label == label => Some(item),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing tray item {label}"));
            (item.activate)(tray);
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Mutex, Once, OnceLock};

    use localization::tr;
    use tray_icon::TrayIconEvent;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState};
    use tray_icon::{TrayIcon, TrayIconBuilder};

    use super::TrayIntent;
    use crate::APP_ID;

    const APP_ICON_BYTES: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../data/icons/hicolor/512x512/apps/io.github.screwys.Rufin.png"
    ));
    const SHOW_ID: &str = "rufin-tray-show";
    const PLAY_PAUSE_ID: &str = "rufin-tray-play-pause";
    const PREVIOUS_ID: &str = "rufin-tray-previous";
    const NEXT_ID: &str = "rufin-tray-next";
    const PRIVATE_MODE_ID: &str = "rufin-tray-private-mode";
    const QUIT_ID: &str = "rufin-tray-quit";

    static INSTALL_EVENT_HANDLERS: Once = Once::new();
    static EVENT_SENDER: OnceLock<Mutex<Option<Sender<TrayIntent>>>> = OnceLock::new();

    pub struct Tray {
        icon: TrayIcon,
        private_mode_item: MenuItem,
    }

    impl Tray {
        pub fn start(private_mode: bool) -> Result<(Self, Receiver<TrayIntent>), String> {
            install_event_handlers();
            let (sender, receiver) = channel();
            let menu = Menu::new();
            let show = MenuItem::with_id(SHOW_ID, tr("Show Rufin"), true, None);
            let play_pause = MenuItem::with_id(PLAY_PAUSE_ID, tr("Play/Pause"), true, None);
            let previous = MenuItem::with_id(PREVIOUS_ID, tr("Previous Track"), true, None);
            let next = MenuItem::with_id(NEXT_ID, tr("Next Track"), true, None);
            let private_mode_item = MenuItem::with_id(
                PRIVATE_MODE_ID,
                private_mode_label(private_mode),
                true,
                None,
            );
            let quit = MenuItem::with_id(QUIT_ID, tr("Quit"), true, None);
            let separator_one = PredefinedMenuItem::separator();
            let separator_two = PredefinedMenuItem::separator();
            let separator_three = PredefinedMenuItem::separator();
            menu.append_items(&[
                &show,
                &separator_one,
                &play_pause,
                &previous,
                &next,
                &separator_two,
                &private_mode_item,
                &separator_three,
                &quit,
            ])
            .map_err(|error| format!("failed to build the tray menu: {error}"))?;

            let builder = TrayIconBuilder::new()
                .with_id(APP_ID)
                .with_tooltip("Rufin")
                .with_menu(Box::new(menu))
                .with_icon(build_tray_icon()?);
            let builder = builder.with_menu_on_left_click(false);
            let icon = builder
                .build()
                .map_err(|error| format!("failed to create the system tray icon: {error}"))?;
            set_event_sender(Some(sender));
            Ok((
                Self {
                    icon,
                    private_mode_item,
                },
                receiver,
            ))
        }

        pub fn set_private_mode(&self, private_mode: bool) {
            self.private_mode_item
                .set_text(private_mode_label(private_mode));
        }

        pub fn shutdown(self) {
            let _ = self.icon.set_visible(false);
            set_event_sender(None);
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            set_event_sender(None);
        }
    }

    fn private_mode_label(private_mode: bool) -> String {
        if private_mode {
            tr("Disable private mode")
        } else {
            tr("Enable private mode")
        }
    }

    fn install_event_handlers() {
        INSTALL_EVENT_HANDLERS.call_once(|| {
            MenuEvent::set_event_handler(Some(|event: MenuEvent| {
                if let Some(intent) = menu_intent(event.id.as_ref()) {
                    send_event(intent);
                }
            }));
            TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
                if matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                ) {
                    send_event(TrayIntent::Present);
                }
            }));
        });
    }

    fn event_sender() -> &'static Mutex<Option<Sender<TrayIntent>>> {
        EVENT_SENDER.get_or_init(|| Mutex::new(None))
    }

    fn set_event_sender(sender: Option<Sender<TrayIntent>>) {
        if let Ok(mut current) = event_sender().lock() {
            *current = sender;
        }
    }

    fn send_event(intent: TrayIntent) {
        if let Ok(current) = event_sender().lock()
            && let Some(sender) = current.as_ref()
        {
            let _ = sender.send(intent);
        }
    }

    fn menu_intent(id: &str) -> Option<TrayIntent> {
        Some(match id {
            SHOW_ID => TrayIntent::Present,
            PLAY_PAUSE_ID => TrayIntent::PlayPause,
            PREVIOUS_ID => TrayIntent::PreviousTrack,
            NEXT_ID => TrayIntent::NextTrack,
            PRIVATE_MODE_ID => TrayIntent::TogglePrivateMode,
            QUIT_ID => TrayIntent::Quit,
            _ => return None,
        })
    }

    fn build_tray_icon() -> Result<tray_icon::Icon, String> {
        let source = artwork::decode_rgba(APP_ICON_BYTES, u32::MAX)
            .map_err(|error| format!("failed to decode the tray icon: {error}"))?;
        let image = source
            .resized_exact(32, 32)
            .map_err(|error| format!("failed to resize the tray icon: {error}"))?;
        let row_stride = usize::try_from(image.row_stride())
            .map_err(|error| format!("invalid tray icon row stride: {error}"))?;
        let row_bytes = usize::try_from(image.width())
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| "invalid tray icon width".to_string())?;
        let mut rgba = Vec::with_capacity(row_bytes * usize::try_from(image.height()).unwrap_or(0));
        for row in image.rgba().chunks(row_stride) {
            rgba.extend_from_slice(
                row.get(..row_bytes)
                    .ok_or_else(|| "invalid tray icon pixel rows".to_string())?,
            );
        }
        tray_icon::Icon::from_rgba(rgba, image.width(), image.height())
            .map_err(|error| format!("failed to create the tray icon: {error}"))
    }

    #[cfg(test)]
    mod tests {
        use super::{menu_intent, private_mode_label};
        use crate::tray::TrayIntent;

        #[test]
        fn native_tray_menu_maps_every_command() {
            assert_eq!(menu_intent("rufin-tray-show"), Some(TrayIntent::Present));
            assert_eq!(
                menu_intent("rufin-tray-play-pause"),
                Some(TrayIntent::PlayPause)
            );
            assert_eq!(
                menu_intent("rufin-tray-previous"),
                Some(TrayIntent::PreviousTrack)
            );
            assert_eq!(menu_intent("rufin-tray-next"), Some(TrayIntent::NextTrack));
            assert_eq!(
                menu_intent("rufin-tray-private-mode"),
                Some(TrayIntent::TogglePrivateMode)
            );
            assert_eq!(menu_intent("rufin-tray-quit"), Some(TrayIntent::Quit));
            assert_eq!(menu_intent("another-app"), None);
        }

        #[test]
        fn native_tray_private_mode_label_follows_state() {
            assert_eq!(private_mode_label(false), "Enable private mode");
            assert_eq!(private_mode_label(true), "Disable private mode");
        }
    }
}

#[cfg(not(any(
    target_os = "windows",
    all(unix, not(any(target_os = "android", target_vendor = "apple")))
)))]
mod unsupported {
    use std::sync::mpsc::Receiver;

    use super::TrayIntent;

    pub struct Tray;

    impl Tray {
        pub fn start(_private_mode: bool) -> Result<(Self, Receiver<TrayIntent>), String> {
            Err("system tray integration is unavailable on this platform".to_string())
        }

        pub fn set_private_mode(&self, _private_mode: bool) {}

        pub fn shutdown(self) {}
    }
}

#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
pub use freedesktop::Tray;
#[cfg(not(any(
    target_os = "windows",
    all(unix, not(any(target_os = "android", target_vendor = "apple")))
)))]
pub use unsupported::Tray;
#[cfg(target_os = "windows")]
pub use windows::Tray;
