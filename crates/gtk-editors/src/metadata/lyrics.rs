use super::{Editor, refresh_save_state};
use adw::prelude::*;
use lyrics::LyricsSearchResult;
use sources::LyricsStorage;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub(super) struct LyricsPane {
    content: gtk::Paned,
    artist: gtk::Entry,
    title: gtk::Entry,
    status: gtk::Label,
    results: gtk::ListBox,
    storage: gtk::Box,
    embed: gtk::ToggleButton,
    sidecar: gtk::ToggleButton,
    reveal: gtk::Revealer,
    editor: gtk_widgets::lyrics_editor::LyricsEditor,
    settings: RefCell<Option<rufin_core::metadata::MetadataLyricsSettings>>,
    candidates: RefCell<Vec<LyricsSearchResult>>,
    active: Cell<bool>,
    updating: Cell<bool>,
    sequence: Cell<u64>,
    task: RefCell<Option<gtk::glib::JoinHandle<()>>>,
}

impl Drop for LyricsPane {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}

impl LyricsPane {
    pub(super) fn new(host: &gtk::Box) -> Self {
        let resource = crate::ui_resource::METADATA_LYRICS_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            lyrics_content: gtk::Paned,
            artist: gtk::Entry, title: gtk::Entry, status: gtk::Label,
            results: gtk::ListBox, storage: gtk::Box, embed: gtk::ToggleButton, sidecar: gtk::ToggleButton,
            editor_revealer: gtk::Revealer, editor_host: gtk::Box,
        });
        let editor = gtk_widgets::lyrics_editor::LyricsEditor::new("", 0, Rc::new(|_| {}));
        editor_host.append(&editor.content);
        host.append(&lyrics_content);
        Self {
            content: lyrics_content,
            artist,
            title,
            status,
            results,
            storage,
            embed,
            sidecar,
            reveal: editor_revealer,
            editor,
            settings: RefCell::new(None),
            candidates: RefCell::new(Vec::new()),
            active: Cell::new(false),
            updating: Cell::new(false),
            sequence: Cell::new(0),
            task: RefCell::new(None),
        }
    }

    pub(super) fn cancel(&self) {
        if let Some(task) = self.task.borrow_mut().take() {
            task.abort();
        }
        self.sequence.set(self.sequence.get().wrapping_add(1));
    }

    pub(super) fn set_busy(&self, busy: bool) {
        self.content.set_sensitive(!busy);
    }

    pub(super) fn reset(&self) {
        self.active.set(false);
        self.updating.set(true);
        self.reveal.set_reveal_child(false);
        self.editor.set_content("", 0);
        self.results.remove_all();
        self.status.set_visible(false);
        self.candidates.borrow_mut().clear();
        self.settings.borrow_mut().take();
        self.updating.set(false);
    }

    fn mode(&self) -> LyricsStorage {
        if self.storage.is_visible() {
            if self.sidecar.is_active() {
                LyricsStorage::Sidecar
            } else {
                LyricsStorage::Embedded
            }
        } else {
            self.settings
                .borrow()
                .as_ref()
                .map_or(LyricsStorage::Server, |settings| settings.storage)
        }
    }

    fn message(&self, message: &str) {
        self.status.set_label(message);
        self.status.set_visible(!message.is_empty());
    }
}

pub(super) fn connect(editor: &Rc<Editor>) {
    let weak = Rc::downgrade(editor);
    editor
        .pages
        .connect_visible_child_name_notify(move |pages| {
            if pages.visible_child_name().as_deref() == Some("lyrics")
                && let Some(editor) = weak.upgrade()
            {
                activate(&editor);
            }
        });
    for entry in [&editor.lyrics.artist, &editor.lyrics.title] {
        let weak = Rc::downgrade(editor);
        entry.connect_activate(move |_| {
            if let Some(editor) = weak.upgrade() {
                search(&editor, false);
            }
        });
        let weak = Rc::downgrade(editor);
        entry.connect_changed(move |_| {
            if let Some(editor) = weak.upgrade()
                && !editor.lyrics.updating.get()
            {
                search(&editor, true);
            }
        });
        entry.connect_icon_release(|entry, position| {
            if position == gtk::EntryIconPosition::Secondary {
                entry.set_text("");
            }
        });
    }
    let weak = Rc::downgrade(editor);
    editor
        .lyrics
        .editor
        .view
        .buffer()
        .connect_changed(move |buffer| {
            if let Some(editor) = weak.upgrade()
                && !editor.lyrics.updating.get()
                && editor.lyrics.reveal.reveals_child()
                && buffer_is_writable(&editor)
            {
                let (start, end) = buffer.bounds();
                editor.model().set_lyrics(
                    buffer.text(&start, &end, false).to_string(),
                    editor.lyrics.mode(),
                );
                refresh_save_state(&editor);
            }
        });
    for button in [&editor.lyrics.embed, &editor.lyrics.sidecar] {
        let weak = Rc::downgrade(editor);
        button.connect_toggled(move |button| {
            if button.is_active()
                && let Some(editor) = weak.upgrade()
                && !editor.lyrics.updating.get()
                && editor.lyrics.reveal.reveals_child()
            {
                editor
                    .model()
                    .set_lyrics(editor.lyrics.editor.text(), editor.lyrics.mode());
                refresh_save_state(&editor);
            }
        });
    }
    let weak = Rc::downgrade(editor);
    editor.lyrics.results.connect_row_selected(move |_, row| {
        let Some(editor) = weak.upgrade() else { return };
        let candidate = row.and_then(|row| {
            editor
                .lyrics
                .candidates
                .borrow()
                .get(row.index() as usize)
                .cloned()
        });
        let Some(candidate) = candidate else { return };
        if !candidate.content.can_save() {
            return;
        }
        editor.lyrics.cancel();
        let sequence = editor.lyrics.sequence.get();
        let model = editor.model();
        let weak = Rc::downgrade(&editor);
        let task = gtk::glib::spawn_future_local(async move {
            let result = model.resolve_lyrics(candidate).await;
            let Some(editor) = weak.upgrade() else { return };
            if sequence != editor.lyrics.sequence.get() {
                return;
            }
            match result {
                Ok(Some(text)) => {
                    editor.lyrics.updating.set(true);
                    editor.lyrics.editor.set_content(&text, 0);
                    editor.lyrics.updating.set(false);
                    let writable = editor
                        .lyrics
                        .settings
                        .borrow()
                        .as_ref()
                        .is_some_and(|settings| !settings.storages.is_empty());
                    editor.lyrics.editor.set_editable(writable);
                    editor.lyrics.reveal.set_reveal_child(true);
                    if writable {
                        model.set_lyrics(text, editor.lyrics.mode());
                        refresh_save_state(&editor);
                    }
                }
                Ok(None) => editor.lyrics.message(&localization::tr("No lyrics")),
                Err(error) => editor.lyrics.message(&error.to_string()),
            }
        });
        editor.lyrics.task.replace(Some(task));
    });
}

pub(super) fn activate(editor: &Rc<Editor>) {
    if editor.lyrics.active.replace(true) {
        return;
    }
    let model = editor.model();
    let weak = Rc::downgrade(editor);
    let sequence = editor.lyrics.sequence.get();
    let task = gtk::glib::spawn_future_local(async move {
        let result = model.lyrics_settings().await;
        let Some(editor) = weak.upgrade() else { return };
        if sequence != editor.lyrics.sequence.get() {
            return;
        }
        match result {
            Ok(settings) => {
                editor.lyrics.updating.set(true);
                editor.lyrics.artist.set_text(&settings.artist);
                editor.lyrics.title.set_text(&settings.title);
                editor
                    .lyrics
                    .storage
                    .set_visible(settings.storages.len() > 1);
                editor
                    .lyrics
                    .sidecar
                    .set_active(settings.storage == LyricsStorage::Sidecar);
                editor
                    .lyrics
                    .embed
                    .set_active(settings.storage == LyricsStorage::Embedded);
                editor.lyrics.artist.set_sensitive(settings.can_search);
                editor.lyrics.title.set_sensitive(settings.can_search);
                editor
                    .lyrics
                    .editor
                    .set_editable(!settings.storages.is_empty());
                if let Some(text) = &settings.text {
                    editor.lyrics.editor.set_content(text, 0);
                    editor.lyrics.reveal.set_reveal_child(true);
                }
                let can_search = settings.can_search;
                editor.lyrics.settings.replace(Some(settings));
                editor.lyrics.updating.set(false);
                if can_search {
                    search(&editor, false);
                }
            }
            Err(error) => editor.lyrics.message(&error.to_string()),
        }
    });
    editor.lyrics.task.replace(Some(task));
}

fn search(editor: &Rc<Editor>, delayed: bool) {
    editor.lyrics.cancel();
    if !editor
        .lyrics
        .settings
        .borrow()
        .as_ref()
        .is_some_and(|settings| settings.can_search)
    {
        return;
    }
    let sequence = editor.lyrics.sequence.get();
    let artist = editor.lyrics.artist.text().trim().to_string();
    let title = editor.lyrics.title.text().trim().to_string();
    editor.lyrics.results.remove_all();
    editor.lyrics.candidates.borrow_mut().clear();
    if artist.is_empty() && title.is_empty() {
        editor.lyrics.message(&localization::tr("Type to search"));
        return;
    }
    editor.lyrics.message(&localization::tr("Searching..."));
    let model = editor.model();
    let weak = Rc::downgrade(editor);
    let task = gtk::glib::spawn_future_local(async move {
        if delayed {
            gtk::glib::timeout_future(std::time::Duration::from_millis(600)).await;
        }
        let result = model.search_lyrics(artist, title).await;
        let Some(editor) = weak.upgrade() else { return };
        if sequence != editor.lyrics.sequence.get() {
            return;
        }
        editor.lyrics.results.remove_all();
        match result {
            Ok(results) => {
                for result in &results {
                    let row = adw::ActionRow::builder()
                        .title(gtk_widgets::lyrics_editor::lyrics_result_title_markup(
                            result,
                        ))
                        .subtitle(gtk_widgets::lyrics_editor::lyrics_result_subtitle_markup(
                            result,
                        ))
                        .build();
                    row.set_activatable(true);
                    row.set_selectable(result.content.can_save());
                    editor.lyrics.results.append(&row);
                }
                let message = if results.is_empty() {
                    localization::tr("No results")
                } else {
                    String::new()
                };
                editor.lyrics.message(&message);
                editor.lyrics.candidates.replace(results);
            }
            Err(error) => editor.lyrics.message(&error.to_string()),
        }
    });
    editor.lyrics.task.replace(Some(task));
}

pub(super) fn commit(editor: &Editor) {
    if editor.lyrics.reveal.reveals_child() && buffer_is_writable(editor) {
        editor
            .model()
            .set_lyrics(editor.lyrics.editor.text(), editor.lyrics.mode());
    }
}

fn buffer_is_writable(editor: &Editor) -> bool {
    editor
        .lyrics
        .settings
        .borrow()
        .as_ref()
        .is_some_and(|settings| !settings.storages.is_empty())
}
