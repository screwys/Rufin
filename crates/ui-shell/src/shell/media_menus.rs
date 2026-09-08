use crate::shell::Shell;
use std::rc::{Rc, Weak};
use ui_shared::media_menus::MediaMenus;
pub(super) fn build(
    weak: &Weak<Shell>,
    products: &rufin_core::runtime::ProductHandles,
    settings: Rc<ui_shared::settings::SettingsState>,
) -> Rc<MediaMenus> {
    Rc::new(MediaMenus {
        library: products.library.clone(),
        runtime: products.runtime.clone(),
        source: products.source.clone(),
        queue: products.playback.queue.clone(),
        radio: products.playback.radio.clone(),
        downloads: products.downloads.clone(),
        settings,
        navigate: {
            let weak = weak.clone();
            Rc::new(move |route| {
                if let Some(shell) = weak.upgrade() {
                    shell.navigate(route);
                }
            })
        },
        set_favorite: {
            let weak = weak.clone();
            Rc::new(move |target, favorite| {
                if let Some(shell) = weak.upgrade() {
                    shell.set_favorite_with_feedback(target, favorite, None);
                }
            })
        },
        edit_metadata: {
            let weak = weak.clone();
            Rc::new(move |item| {
                if let Some(shell) = weak.upgrade() {
                    crate::preferences::dialogs::metadata::present_metadata_dialog(&shell, item);
                }
            })
        },
        set_sidebar_pin: {
            let weak = weak.clone();
            Rc::new(move |pin, pinned| {
                if let Some(shell) = weak.upgrade() {
                    shell.set_sidebar_pin(pin, pinned);
                }
            })
        },
        export_playlist_dialog: {
            let weak = weak.clone();
            Rc::new(move |target, name| {
                if let Some(shell) = weak.upgrade() {
                    shell.export_playlist_dialog(target, name);
                }
            })
        },
        rename_playlist_dialog: {
            let weak = weak.clone();
            Rc::new(move |key, name| {
                if let Some(shell) = weak.upgrade() {
                    shell.rename_playlist_dialog(key, name);
                }
            })
        },
        edit_smart_playlist_dialog: {
            let weak = weak.clone();
            Rc::new(move |row| {
                if let Some(shell) = weak.upgrade() {
                    shell.edit_smart_playlist_dialog(row);
                }
            })
        },
        publish_smart_playlist_change: {
            let weak = weak.clone();
            Rc::new(move |change, settled| {
                if let Some(shell) = weak.upgrade() {
                    shell.publish_smart_playlist_change(change, settled);
                }
            })
        },
        operation_feedback: {
            let weak = weak.clone();
            Rc::new(move |feedback, undo| {
                if let Some(shell) = weak.upgrade() {
                    if let Some(undo) = undo {
                        shell.show_undoable_operation_feedback(feedback, move || undo());
                    } else {
                        shell.show_operation_feedback(feedback);
                    };
                }
            })
        },
        picker_target: {
            let weak = weak.clone();
            Rc::new(move |surface, target| {
                if let Some(shell) = weak.upgrade() {
                    crate::shell::playlist_picker::append_context_menu_picker(
                        surface, &shell, target,
                    );
                }
            })
        },
        picker_payload: {
            let weak = weak.clone();
            Rc::new(move |surface, payload| {
                if let Some(shell) = weak.upgrade() {
                    crate::shell::playlist_picker::append_context_menu_picker_source(
                        surface, &shell, payload,
                    );
                }
            })
        },
        play_target: {
            let weak = weak.clone();
            Rc::new(move |target, placement| {
                if let Some(shell) = weak.upgrade() {
                    crate::shell::catalog_actions::play_target(target, &shell, placement);
                }
            })
        },
        download: {
            let weak = weak.clone();
            Rc::new(move |target| {
                if let Some(shell) = weak.upgrade() {
                    crate::shell::catalog_actions::download(target, &shell);
                }
            })
        },
        remove_download: {
            let weak = weak.clone();
            Rc::new(move |target| {
                if let Some(shell) = weak.upgrade() {
                    crate::shell::catalog_actions::remove_download(target, &shell);
                }
            })
        },
        projected_item_favorite: {
            let weak = weak.clone();
            Rc::new(move |target, fallback| {
                weak.upgrade().map_or(fallback, |shell| {
                    shell.projected_item_favorite(target, fallback)
                })
            })
        },
        half_stars_enabled: {
            let weak = weak.clone();
            Rc::new(move |uri, source| {
                weak.upgrade()
                    .is_some_and(|shell| shell.half_stars_enabled(uri, source))
            })
        },
        source_download_available: {
            let weak = weak.clone();
            Rc::new(move |source_id| {
                weak.upgrade().is_some_and(|shell| {
                    shell
                        .source
                        .configured
                        .borrow()
                        .sources
                        .iter()
                        .any(|source| &source.id == source_id && source.kind != "local")
                })
            })
        },
    })
}
