use super::{Editor, refresh_save_state};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use adw::prelude::*;
use gtk_widgets::artwork::texture_from_image as texture;
use rufin_core::metadata::ArtworkResult;
use sources::{ArtworkStorage, ImageBytes, SourceMetadataError};

struct SearchResult {
    result: ArtworkResult,
    thumbnail: gtk::Picture,
    image: Option<Arc<ImageBytes>>,
}

pub(super) struct ArtworkEditor {
    controls: gtk::Box,
    preview: gtk::Picture,
    status: gtk::Label,
    use_current: gtk::Button,
    upload: gtk::Button,
    remove: gtk::Button,
    chooser: gtk::FileDialog,
    storage_options: gtk::Box,
    embed: gtk::ToggleButton,
    folder: gtk::ToggleButton,
    open_search: gtk::Button,
    search_dialog: adw::Dialog,
    search_artist: gtk::Entry,
    search_album: gtk::Entry,
    search_status: gtk::Label,
    results: gtk::ListBox,
    candidates: RefCell<Vec<SearchResult>>,
    current: RefCell<Option<gtk::gdk::Texture>>,
    ready: Cell<bool>,
    loading_selection: Cell<bool>,
    search_sequence: Cell<u64>,
    selection_sequence: Cell<u64>,
    current_task: RefCell<Option<gtk::glib::JoinHandle<()>>>,
    search_task: RefCell<Option<gtk::glib::JoinHandle<()>>>,
    selection_task: RefCell<Option<gtk::glib::JoinHandle<()>>>,
    unavailable: String,
    selected_text: String,
    searching: String,
    type_to_search: String,
    empty: String,
}

impl ArtworkEditor {
    pub(super) fn new(host: &gtk::Box, external_lookup_allowed: bool) -> Self {
        let resource = crate::ui_resource::METADATA_ARTWORK_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            artwork_content: gtk::Box, artwork_controls: gtk::Box, preview: gtk::Picture,
            artwork_status: gtk::Label, use_current: gtk::Button, upload: gtk::Button,
            remove: gtk::Button,
            chooser: gtk::FileDialog, storage_options: gtk::Box, embed: gtk::ToggleButton,
            folder: gtk::ToggleButton, open_search: gtk::Button, search_dialog: adw::Dialog,
            search_artist: gtk::Entry,
            search_album: gtk::Entry, results: gtk::ListBox,
            search_status: gtk::Label,
            unavailable_text: gtk::Label, selected_text: gtk::Label,
            searching_text: gtk::Label, empty_text: gtk::Label,
            type_to_search_text: gtk::Label,
        });
        host.append(&artwork_content);
        open_search.set_visible(external_lookup_allowed);
        Self {
            controls: artwork_controls,
            preview,
            status: artwork_status,
            use_current,
            upload,
            remove,
            chooser,
            storage_options,
            embed,
            folder,
            open_search,
            search_dialog,
            search_artist,
            search_album,
            search_status,
            results,
            candidates: RefCell::new(Vec::new()),
            current: RefCell::new(None),
            ready: Cell::new(false),
            loading_selection: Cell::new(false),
            search_sequence: Cell::new(0),
            selection_sequence: Cell::new(0),
            current_task: RefCell::new(None),
            search_task: RefCell::new(None),
            selection_task: RefCell::new(None),
            unavailable: unavailable_text.label().to_string(),
            selected_text: selected_text.label().to_string(),
            searching: searching_text.label().to_string(),
            type_to_search: type_to_search_text.label().to_string(),
            empty: empty_text.label().to_string(),
        }
    }

    pub(super) fn pending(&self) -> bool {
        self.loading_selection.get()
    }

    pub(super) fn set_busy(&self, busy: bool) {
        self.controls.set_sensitive(!busy && self.ready.get());
    }

    fn message(&self, text: &str, error: bool) {
        status_message(&self.status, text, error);
    }

    fn search_message(&self, text: &str, error: bool) {
        status_message(&self.search_status, text, error);
    }

    fn restore_current_preview(&self, changed: bool) {
        if !changed && let Some(texture) = self.current.borrow().as_ref() {
            self.preview.set_paintable(Some(texture));
        }
    }

    fn begin_selection(&self) -> u64 {
        if let Some(task) = self.selection_task.borrow_mut().take() {
            task.abort();
        }
        let sequence = self.selection_sequence.get().wrapping_add(1);
        self.selection_sequence.set(sequence);
        self.loading_selection.set(true);
        self.message("", false);
        sequence
    }
}

impl Drop for ArtworkEditor {
    fn drop(&mut self) {
        for task in [
            &mut self.current_task,
            &mut self.search_task,
            &mut self.selection_task,
        ] {
            if let Some(task) = task.get_mut().take() {
                task.abort();
            }
        }
    }
}

pub(super) fn connect(editor: &Rc<Editor>) {
    seed_search(editor);
    let fields: &[&str] = match editor.draft.kind {
        sources::SourceEntityKind::Track => &["artist", "album_artist", "album"],
        sources::SourceEntityKind::Album => &["title", "artist", "album_artist"],
        sources::SourceEntityKind::Artist => &["title"],
    };
    for entry in editor
        .entries
        .iter()
        .filter(|entry| fields.contains(&entry.field.as_str()))
    {
        let weak = Rc::downgrade(editor);
        entry.entry.connect_changed(move |_| {
            if let Some(editor) = weak.upgrade() {
                seed_search(&editor);
            }
        });
    }
    for (button, storage) in [
        (&editor.artwork.embed, ArtworkStorage::Embedded),
        (&editor.artwork.folder, ArtworkStorage::Folder),
    ] {
        let weak = Rc::downgrade(editor);
        button.connect_toggled(move |button| {
            if button.is_active()
                && let Some(editor) = weak.upgrade()
            {
                editor.model.set_artwork_storage(storage);
                refresh_save_state(&editor);
            }
        });
    }
    let weak = Rc::downgrade(editor);
    editor.artwork.use_current.connect_clicked(move |_| {
        let Some(editor) = weak.upgrade() else {
            return;
        };
        let current = editor.artwork.current.borrow().clone();
        if let Some(texture) = current {
            editor.artwork.begin_selection();
            editor.model.use_current_artwork();
            show_selected_image(&editor, &texture);
            editor.artwork.results.unselect_all();
        }
    });
    let weak = Rc::downgrade(editor);
    editor.artwork.remove.connect_clicked(move |_| {
        let Some(editor) = weak.upgrade() else { return };
        editor.artwork.begin_selection();
        editor.model.remove_artwork();
        editor.artwork.loading_selection.set(false);
        editor
            .artwork
            .preview
            .set_paintable(None::<&gtk::gdk::Texture>);
        editor.artwork.remove.set_sensitive(false);
        editor.artwork.results.unselect_all();
        editor.artwork.message("", false);
        refresh_save_state(&editor);
    });
    for entry in [&editor.artwork.search_artist, &editor.artwork.search_album] {
        entry.connect_icon_release(|entry, position| {
            if position == gtk::EntryIconPosition::Secondary {
                entry.set_text("");
            }
        });
        let weak = Rc::downgrade(editor);
        entry.connect_activate(move |_| {
            if let Some(editor) = weak.upgrade() {
                schedule_search(&editor, Duration::ZERO);
            }
        });
        let weak = Rc::downgrade(editor);
        entry.connect_changed(move |_| {
            if let Some(editor) = weak.upgrade()
                && editor.artwork.search_dialog.is_mapped()
            {
                schedule_search(&editor, Duration::from_millis(600));
            }
        });
    }
    connect_upload(editor);
    connect_results(editor);
    let weak = Rc::downgrade(editor);
    editor.artwork.open_search.connect_clicked(move |button| {
        if let Some(editor) = weak.upgrade() {
            gtk_widgets::popup::present_light_dismiss_dialog(&editor.artwork.search_dialog, button);
            schedule_search(&editor, Duration::from_millis(600));
        }
    });
    let weak = Rc::downgrade(editor);
    editor.artwork.search_dialog.connect_closed(move |_| {
        let Some(editor) = weak.upgrade() else { return };
        if let Some(task) = editor.artwork.search_task.borrow_mut().take() {
            task.abort();
        }
        refresh_save_state(&editor);
    });
    load_current(editor);
}

fn seed_search(editor: &Editor) {
    let (artist, album) = draft_search_fields(editor);
    editor.artwork.search_artist.set_text(&artist);
    editor.artwork.search_album.set_visible(album.is_some());
    editor
        .artwork
        .search_album
        .set_text(album.as_deref().unwrap_or_default());
}

fn draft_search_fields(editor: &Editor) -> (String, Option<String>) {
    let state = editor.model.snapshot();
    (state.search_artist, state.search_album)
}

fn load_current(editor: &Rc<Editor>) {
    let editing = editor.draft.artwork.clone();
    let weak = Rc::downgrade(editor);
    let task = gtk::glib::spawn_future_local(async move {
        let Some(editor) = weak.upgrade() else { return };
        editor.artwork.ready.set(true);
        editor
            .artwork
            .remove
            .set_sensitive(editing.binding.is_some());
        editor
            .artwork
            .storage_options
            .set_visible(editing.storage != ArtworkStorage::Server && editing.can_embed);
        match editing.storage {
            ArtworkStorage::Embedded => editor.artwork.embed.set_active(true),
            ArtworkStorage::Folder => editor.artwork.folder.set_active(true),
            ArtworkStorage::Server => {}
        }
        editor.artwork.set_busy(editor.busy.get());
        let Some(_) = editing.binding else {
            editor.artwork.message("", false);
            refresh_save_state(&editor);
            return;
        };
        let model = editor.model.clone();
        let unavailable = editor.artwork.unavailable.clone();
        drop(editor);
        let result = match model.load_current_artwork().await {
            Ok(Some(image)) => texture(image, 512).await.map(Some),
            Ok(None) => Ok(None),
            Err(error) => Err(artwork_error(error, &unavailable)),
        };
        let Some(editor) = weak.upgrade() else {
            return;
        };
        match result {
            Ok(Some(texture)) => {
                if !editor.model.artwork_changed() && !editor.artwork.pending() {
                    editor.artwork.preview.set_paintable(Some(&texture));
                    if !editor.artwork.status.has_css_class("error") {
                        editor.artwork.message("", false);
                    }
                }
                editor.artwork.current.replace(Some(texture));
                editor.artwork.use_current.set_sensitive(true);
            }
            Ok(None)
                if !editor.model.artwork_changed()
                    && !editor.artwork.pending()
                    && !editor.artwork.status.has_css_class("error") =>
            {
                editor.artwork.message("", false)
            }
            Err(error)
                if !editor.model.artwork_changed()
                    && !editor.artwork.pending()
                    && !editor.artwork.status.has_css_class("error") =>
            {
                editor.artwork.message(&error, true)
            }
            _ => {}
        }
        refresh_save_state(&editor);
    });
    editor.artwork.current_task.replace(Some(task));
}

fn connect_upload(editor: &Rc<Editor>) {
    let weak = Rc::downgrade(editor);
    editor.artwork.upload.connect_clicked(move |_| {
        let Some(editor) = weak.upgrade() else { return };
        let parent = editor.artwork.upload.root().and_downcast::<gtk::Window>();
        let chosen = editor.artwork.chooser.open_future(parent.as_ref());
        let sequence = editor.artwork.begin_selection();
        editor.artwork.results.unselect_all();
        let model = editor.model.clone();
        let unavailable = editor.artwork.unavailable.clone();
        let weak = Rc::downgrade(&editor);
        let task = gtk::glib::spawn_future_local(async move {
            let chosen = chosen.await;
            let result = match chosen {
                Ok(file) => {
                    let bytes = match file.load_contents_future().await {
                        Ok((bytes, _)) => bytes.to_vec(),
                        Err(error) => {
                            finish_selection(&weak, sequence, Err(error.to_string()));
                            return;
                        }
                    };
                    image_response(model.prepare_artwork(bytes).await, unavailable).await
                }
                Err(error)
                    if error.matches(gtk::DialogError::Dismissed)
                        || error.matches(gtk::DialogError::Cancelled) =>
                {
                    if let Some(editor) = weak.upgrade()
                        && sequence == editor.artwork.selection_sequence.get()
                    {
                        editor.artwork.loading_selection.set(false);
                        editor
                            .artwork
                            .restore_current_preview(editor.model.artwork_changed());
                        editor.artwork.message("", false);
                        refresh_save_state(&editor);
                    }
                    return;
                }
                Err(error) => Err(error.to_string()),
            };
            finish_selection(&weak, sequence, result);
        });
        editor.artwork.selection_task.replace(Some(task));
        refresh_save_state(&editor);
    });
}

fn schedule_search(editor: &Rc<Editor>, delay: Duration) {
    if let Some(task) = editor.artwork.search_task.borrow_mut().take() {
        task.abort();
    }
    let sequence = editor.artwork.search_sequence.get().wrapping_add(1);
    editor.artwork.search_sequence.set(sequence);
    editor.artwork.results.remove_all();
    editor.artwork.results.set_visible(false);
    editor.artwork.candidates.borrow_mut().clear();
    if editor.artwork.search_artist.text().trim().is_empty()
        && editor.artwork.search_album.text().trim().is_empty()
    {
        editor
            .artwork
            .search_message(&editor.artwork.type_to_search, false);
        return;
    }
    editor
        .artwork
        .search_message(&editor.artwork.searching, false);
    let artist = editor.artwork.search_artist.text().to_string();
    let album = editor.artwork.search_album.text().to_string();
    let model = editor.model.clone();
    let weak = Rc::downgrade(editor);
    let task = gtk::glib::spawn_future_local(async move {
        gtk::glib::timeout_future(delay).await;
        let response = model.search_artwork(artist, album).await;
        let Some(editor) = weak.upgrade() else {
            return;
        };
        if sequence != editor.artwork.search_sequence.get() {
            return;
        }
        let thumbnails = match response {
            Ok(results) => show_results(&editor, results),
            Err(error) => {
                editor
                    .artwork
                    .search_message(&artwork_error(error, &editor.artwork.unavailable), true);
                return;
            }
        };
        drop(editor);
        for (index, (picture, url)) in thumbnails.into_iter().enumerate() {
            if let Ok(image) = model.download_artwork(url).await {
                let result = texture(Arc::clone(&image), 128).await;
                let Some(editor) = weak.upgrade() else {
                    return;
                };
                if sequence != editor.artwork.search_sequence.get() {
                    return;
                }
                if let (Ok(texture), Some(picture)) = (result, picture.upgrade()) {
                    picture.set_paintable(Some(&texture));
                    if let Some(candidate) = editor.artwork.candidates.borrow_mut().get_mut(index)
                        && candidate.result.thumbnail_url == candidate.result.image_url
                    {
                        candidate.image = Some(image);
                    }
                }
            }
        }
    });
    editor.artwork.search_task.replace(Some(task));
}

fn show_results(
    editor: &Editor,
    results: Vec<ArtworkResult>,
) -> Vec<(gtk::glib::WeakRef<gtk::Picture>, String)> {
    editor.artwork.search_message(
        if results.is_empty() {
            &editor.artwork.empty
        } else {
            ""
        },
        false,
    );
    let mut thumbnails = Vec::with_capacity(results.len());
    let mut candidates = Vec::with_capacity(results.len());
    for result in results {
        let resource = crate::ui_resource::METADATA_ARTWORK_RESULT_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            result_row: gtk::ListBoxRow, thumbnail: gtk::Picture, title: gtk::Label, detail: gtk::Label,
        });
        title.set_label(&result.title);
        title.set_tooltip_text(Some(&result.title));
        detail.set_label(&result.detail);
        detail.set_visible(!result.detail.is_empty());
        result_row.set_tooltip_text(Some(&result.source_url));
        thumbnails.push((thumbnail.downgrade(), result.thumbnail_url.clone()));
        editor.artwork.results.append(&result_row);
        candidates.push(SearchResult {
            result,
            thumbnail,
            image: None,
        });
    }
    editor.artwork.results.set_visible(!candidates.is_empty());
    editor.artwork.candidates.replace(candidates);
    thumbnails
}

fn connect_results(editor: &Rc<Editor>) {
    let weak = Rc::downgrade(editor);
    editor.artwork.results.connect_row_activated(move |_, row| {
        let Some(editor) = weak.upgrade() else { return };
        let candidate = editor
            .artwork
            .candidates
            .borrow()
            .get(row.index() as usize)
            .map(|candidate| {
                (
                    candidate.result.image_url.clone(),
                    candidate.thumbnail.paintable(),
                    candidate.image.clone(),
                )
            });
        let Some((url, thumbnail, image)) = candidate else {
            return;
        };
        let previous = editor.artwork.preview.paintable();
        let sequence = editor.artwork.begin_selection();
        editor.artwork.preview.set_paintable(thumbnail.as_ref());
        editor.artwork.search_dialog.close();
        let model = editor.model.clone();
        let unavailable = editor.artwork.unavailable.clone();
        let weak = Rc::downgrade(&editor);
        let task = gtk::glib::spawn_future_local(async move {
            let result = if let Some(image) = image {
                texture(Arc::clone(&image), 512)
                    .await
                    .map(|texture| (image, texture))
            } else {
                image_response(model.download_artwork(url).await, unavailable).await
            };
            let Some(editor) = weak.upgrade() else { return };
            if sequence != editor.artwork.selection_sequence.get() {
                return;
            }
            if result.is_err() {
                editor.artwork.preview.set_paintable(previous.as_ref());
            }
            finish_selection(&weak, sequence, result);
        });
        editor.artwork.selection_task.replace(Some(task));
        refresh_save_state(&editor);
    });
}

async fn image_response(
    image: Result<Arc<ImageBytes>, SourceMetadataError>,
    unavailable: String,
) -> Result<(Arc<ImageBytes>, gtk::gdk::Texture), String> {
    let image = image.map_err(|error| artwork_error(error, &unavailable))?;
    let texture = texture(Arc::clone(&image), 512).await?;
    Ok((image, texture))
}

fn finish_selection(
    weak: &std::rc::Weak<Editor>,
    sequence: u64,
    result: Result<(Arc<ImageBytes>, gtk::gdk::Texture), String>,
) {
    let Some(editor) = weak.upgrade() else {
        return;
    };
    if sequence != editor.artwork.selection_sequence.get() {
        return;
    }
    editor.artwork.loading_selection.set(false);
    match result {
        Ok((image, texture)) => {
            editor.model.select_artwork(image);
            show_selected_image(&editor, &texture);
        }
        Err(error) => {
            editor
                .artwork
                .restore_current_preview(editor.model.artwork_changed());
            editor.artwork.message(&error, true);
        }
    }
    refresh_save_state(&editor);
}

fn show_selected_image(editor: &Editor, texture: &gtk::gdk::Texture) {
    editor.artwork.remove.set_sensitive(true);
    editor.artwork.loading_selection.set(false);
    editor.artwork.preview.set_paintable(Some(texture));
    editor
        .artwork
        .preview
        .set_alternative_text(Some(&editor.artwork.selected_text));
    editor.artwork.message("", false);
    refresh_save_state(editor);
}

fn status_message(status: &gtk::Label, text: &str, error: bool) {
    status.set_label(text);
    status.set_visible(!text.is_empty());
    if error {
        status.add_css_class("error");
    } else {
        status.remove_css_class("error");
    }
}

fn artwork_error(error: SourceMetadataError, unavailable: &str) -> String {
    match error {
        SourceMetadataError::Write(message) => message,
        SourceMetadataError::Unavailable => unavailable.to_owned(),
        error => error.to_string(),
    }
}
