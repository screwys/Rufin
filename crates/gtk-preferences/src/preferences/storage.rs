use std::rc::Rc;

use adw::prelude::*;
use rufin_core::runtime::{CacheKind, CacheUsage, ProductHandles, StorageSummary};

use crate::Preferences;

struct CacheRow {
    kind: CacheKind,
    count: gtk::glib::WeakRef<gtk::Label>,
    size: gtk::glib::WeakRef<gtk::Label>,
    clear: gtk::glib::WeakRef<gtk::Button>,
}

struct StorageView {
    page: gtk::glib::WeakRef<adw::PreferencesPage>,
    total: gtk::glib::WeakRef<gtk::Label>,
    spinner: gtk::glib::WeakRef<gtk::Spinner>,
    error: gtk::glib::WeakRef<gtk::Label>,
    unavailable: gtk::glib::GString,
    rows: Vec<CacheRow>,
    reset_buttons: Vec<gtk::glib::WeakRef<gtk::Button>>,
    products: ProductHandles,
}

pub(super) fn page(
    shell: &Rc<Preferences>,
    navigation_controls: &super::PreferencesNavigationControls,
) -> gtk::Widget {
    let resource = crate::ui_resource::STORAGE_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        navigation: adw::NavigationView,
        storage_page: adw::PreferencesPage,
        reset_group: adw::PreferencesGroup,
        clear_all_cache: gtk::Button,
        clear_all_data: gtk::Button,
        artwork_download: adw::ComboRow,
        lyrics_download: adw::ComboRow,
        total_size: gtk::Label,
        spinner: gtk::Spinner,
        error: gtk::Label,
        unavailable_text: gtk::Label,
        waveforms_count: gtk::Label, waveforms_size: gtk::Label, clear_waveforms: gtk::Button,
        loudness_count: gtk::Label, loudness_size: gtk::Label, clear_loudness: gtk::Button,
        lookups_count: gtk::Label, lookups_size: gtk::Label, clear_lookups: gtk::Button,
        fingerprints_count: gtk::Label, fingerprints_size: gtk::Label, clear_fingerprints: gtk::Button,
        artwork_count: gtk::Label, artwork_size: gtk::Label, clear_artwork: gtk::Button,
        lyrics_count: gtk::Label, lyrics_size: gtk::Label, clear_lyrics: gtk::Button,
        dictionary_size: gtk::Label, clear_dictionary: gtk::Button,
    });
    navigation_controls.set_navigation(&navigation);
    navigation_controls.set_nested_page_visible(false);
    let controls = navigation_controls.clone();
    navigation.connect_popped(move |_, _| controls.set_nested_page_visible(false));
    storage_page.add(&reset_group);
    let settings = shell.settings.persistence.load();
    artwork_download.set_selected(u32::from(
        settings.application.download_artwork_automatically,
    ));
    lyrics_download.set_selected(u32::from(
        settings.application.download_lyrics_automatically,
    ));
    let settings = Rc::clone(&shell.settings);
    artwork_download.connect_selected_notify(move |row| {
        settings.set_app_setting(
            "automatic artwork download",
            row.selected() == 1,
            |settings| &mut settings.application.download_artwork_automatically,
        );
    });
    let settings = Rc::clone(&shell.settings);
    lyrics_download.connect_selected_notify(move |row| {
        settings.set_app_setting(
            "automatic lyrics download",
            row.selected() == 1,
            |settings| &mut settings.application.download_lyrics_automatically,
        );
    });

    let controls = [
        (
            CacheKind::MetadataLookups,
            Some(lookups_count),
            lookups_size,
            clear_lookups,
        ),
        (
            CacheKind::Fingerprints,
            Some(fingerprints_count),
            fingerprints_size,
            clear_fingerprints,
        ),
        (
            CacheKind::Waveforms,
            Some(waveforms_count),
            waveforms_size,
            clear_waveforms,
        ),
        (
            CacheKind::Loudness,
            Some(loudness_count),
            loudness_size,
            clear_loudness,
        ),
        (
            CacheKind::Artwork,
            Some(artwork_count),
            artwork_size,
            clear_artwork,
        ),
        (
            CacheKind::Lyrics,
            Some(lyrics_count),
            lyrics_size,
            clear_lyrics,
        ),
        (
            CacheKind::JapaneseDictionary,
            None,
            dictionary_size,
            clear_dictionary,
        ),
    ];
    let view = Rc::new(StorageView {
        page: storage_page.downgrade(),
        total: total_size.downgrade(),
        spinner: spinner.downgrade(),
        error: error.downgrade(),
        unavailable: unavailable_text.label(),
        rows: controls
            .iter()
            .map(|(kind, count, size, clear)| CacheRow {
                kind: *kind,
                count: count.as_ref().map(ObjectExt::downgrade).unwrap_or_default(),
                size: size.downgrade(),
                clear: clear.downgrade(),
            })
            .collect(),
        reset_buttons: vec![clear_all_cache.downgrade(), clear_all_data.downgrade()],
        products: shell.products.clone(),
    });
    for (kind, _, _, button) in controls {
        let view = view.clone();
        button.connect_clicked(move |_| view.confirm_clear(kind));
    }
    for (button, kind, confirmation) in [
        (
            clear_all_cache,
            rufin_core::settings::StorageReset::KeepCredentials,
            "clear_cache_confirmation",
        ),
        (
            clear_all_data,
            rufin_core::settings::StorageReset::AllData,
            "clear_data_confirmation",
        ),
    ] {
        let weak = Rc::downgrade(shell);
        let view = view.clone();
        button.connect_clicked(move |_| {
            let view = view.clone();
            let weak = weak.clone();
            gtk::glib::spawn_future_local(async move {
                let Some(page) = view.page.upgrade() else {
                    return;
                };
                let resource = crate::ui_resource::STORAGE_PREFERENCES_RESOURCE;
                let builder = gtk_widgets::ui_resource::builder(resource);
                let dialog: adw::AlertDialog =
                    gtk_widgets::ui_resource::object(&builder, resource, confirmation);
                if dialog.choose_future(Some(&page)).await != "clear" {
                    return;
                }
                view.set_busy(true);
                let result = view.products.prepare_storage_reset(kind);
                match result {
                    Ok(request) => {
                        if let Some(shell) = weak.upgrade() {
                            (shell.effects)(crate::Effect::ResetStorage(Box::new(request)));
                        }
                    }
                    Err(message) => {
                        view.set_busy(false);
                        if let Some(error) = view.error.upgrade() {
                            error.set_label(&message);
                            error.set_visible(true);
                        }
                    }
                }
            });
        });
    }
    storage_page.connect_map(move |_| view.load(None));
    navigation.upcast()
}

impl StorageView {
    fn confirm_clear(self: &Rc<Self>, kind: CacheKind) {
        let view = self.clone();
        gtk::glib::spawn_future_local(async move {
            let Some(page) = view.page.upgrade() else {
                return;
            };
            let resource = crate::ui_resource::STORAGE_PREFERENCES_RESOURCE;
            let builder = gtk_widgets::ui_resource::builder(resource);
            let dialog: adw::AlertDialog =
                gtk_widgets::ui_resource::object(&builder, resource, "cache_entry_confirmation");
            if dialog.choose_future(Some(&page)).await == "clear" {
                view.load(Some(kind));
            }
        });
    }

    fn load(self: &Rc<Self>, clear: Option<CacheKind>) {
        if self
            .spinner
            .upgrade()
            .is_some_and(|spinner| spinner.is_spinning())
        {
            return;
        }
        self.set_busy(true);
        let view = self.clone();
        let products = self.products.clone();
        let task = self
            .products
            .runtime
            .spawn(async move { products.storage_summary(clear).await });
        let cancel = task.abort_handle();
        let teardown = if clear.is_none() {
            self.page
                .upgrade()
                .map(|page| page.connect_destroy(move |_| cancel.abort()))
        } else {
            None
        };
        gtk::glib::spawn_future_local(async move {
            let result = task.await;
            let Some(page) = view.page.upgrade() else {
                return;
            };
            if let Some(teardown) = teardown {
                page.disconnect(teardown);
            }
            let summary = result.unwrap_or_else(|error| StorageSummary {
                caches: Vec::new(),
                total_bytes: None,
                errors: vec![error.to_string()],
            });
            view.set_busy(false);
            for row in &view.rows {
                let usage = summary
                    .caches
                    .iter()
                    .find(|cache| cache.kind == row.kind)
                    .and_then(|cache| cache.usage);
                row.update(usage, &view.unavailable);
            }
            if let Some(label) = view.total.upgrade() {
                let size = summary
                    .total_bytes
                    .map(gtk::glib::format_size)
                    .unwrap_or_else(|| view.unavailable.clone());
                label.set_label(&size);
            }
            if let Some(label) = view.error.upgrade() {
                label.set_label(&summary.errors.join("\n"));
                label.set_visible(!summary.errors.is_empty());
            }
        });
    }

    fn set_busy(&self, busy: bool) {
        for button in &self.reset_buttons {
            if let Some(button) = button.upgrade() {
                button.set_sensitive(!busy);
            }
        }
        if let Some(spinner) = self.spinner.upgrade() {
            spinner.set_spinning(busy);
            spinner.set_visible(busy);
        }
        for row in &self.rows {
            if let Some(button) = row.clear.upgrade() {
                button.set_sensitive(!busy);
            }
        }
    }
}

impl CacheRow {
    fn update(&self, usage: Option<CacheUsage>, unavailable: &str) {
        if let Some(count) = self.count.upgrade() {
            let text = usage
                .map(|usage| {
                    localization::trn_with(
                        "{count} entry",
                        "{count} entries",
                        usage.entries,
                        &[("count", &usage.entries.to_string())],
                    )
                })
                .unwrap_or_else(|| unavailable.to_owned());
            count.set_label(&text);
        }
        if let Some(size) = self.size.upgrade() {
            let text = usage
                .map(|usage| gtk::glib::format_size(usage.bytes))
                .unwrap_or_else(|| unavailable.into());
            size.set_label(&text);
        }
        if let Some(button) = self.clear.upgrade() {
            button.set_sensitive(usage.is_none_or(|usage| usage.entries > 0));
        }
    }
}
