use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::Arc,
};

use adw::prelude::*;
use rufin_core::source::SourceOwner;
use sources::{ArtworkChange, ImageBytes};

pub struct ArtworkEditor {
    pub frame: gtk::AspectFrame,
    pub overlay: gtk::Overlay,
    pub status: gtk::Label,
    preview: gtk::Picture,
    display: gtk::Stack,
    current_host: gtk::Box,
    current: gtk_widgets::artwork::CoverGroupProjection,
    artwork: Rc<gtk_widgets::artwork::ArtworkState>,
    actions: gtk::Box,
    content: gtk::Box,
    remove: gtk::Button,
    apply: gtk::Button,
    writable: bool,
    has_image: Cell<bool>,
    change: RefCell<Option<ArtworkChange>>,
    task: RefCell<Option<gtk::glib::JoinHandle<()>>>,
}

impl ArtworkEditor {
    pub fn new(
        source: &Arc<SourceOwner>,
        artwork: &Rc<gtk_widgets::artwork::ArtworkState>,
        editing: rufin_core::playlists::PlaylistArtworkEditing,
        content: &gtk::Box,
        apply: &gtk::Button,
        regenerate: impl Fn(
            &SourceOwner,
        ) -> async_channel::Receiver<Result<Option<Arc<ImageBytes>>, String>>
        + 'static,
    ) -> Rc<Self> {
        let resource = crate::ui_resource::PLAYLIST_ARTWORK_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            artwork_preview: gtk::Picture, artwork_status: gtk::Label,
            artwork_display: gtk::Stack, artwork_current: gtk::Box,
            artwork_actions: gtk::Box, add_image: gtk::Button, delete_image: gtk::Button,
            regenerate_image: gtk::Button, image_chooser: gtk::FileDialog,
            artwork_frame: gtk::AspectFrame, artwork_overlay: gtk::Overlay,
        });
        let fallback = editing
            .representative_artwork
            .iter()
            .map(|binding| artwork::ArtworkBinding::opaque(binding))
            .collect::<Vec<_>>();
        let bindings = editing
            .binding
            .as_ref()
            .map(|binding| vec![artwork::ArtworkBinding::opaque(binding)])
            .unwrap_or_else(|| fallback.clone());
        let current = artwork.elastic_cover_group_projection_for_artwork(&bindings, 512);
        artwork_current.append(&current.widget());
        let editor = Rc::new(Self {
            frame: artwork_frame,
            overlay: artwork_overlay,
            preview: artwork_preview,
            display: artwork_display,
            current_host: artwork_current,
            current,
            artwork: Rc::clone(artwork),
            status: artwork_status,
            actions: artwork_actions,
            content: content.clone(),
            remove: delete_image,
            apply: apply.clone(),
            writable: editing.writable,
            has_image: Cell::new(editing.binding.is_some()),
            change: RefCell::new(None),
            task: RefCell::new(None),
        });
        editor.set_busy(false);
        editor.message("", false);

        let weak = Rc::downgrade(&editor);
        let owner = Arc::downgrade(source);
        add_image.connect_clicked(move |button| {
            let Some(editor) = weak.upgrade() else { return };
            editor.begin();
            let parent = button.root().and_downcast::<gtk::Window>();
            let chosen = image_chooser.open_future(parent.as_ref());
            let weak = weak.clone();
            let owner = owner.clone();
            editor
                .task
                .replace(Some(gtk::glib::spawn_future_local(async move {
                    let result = async {
                        let file = match chosen.await {
                            Ok(file) => file,
                            Err(error)
                                if error.matches(gtk::DialogError::Dismissed)
                                    || error.matches(gtk::DialogError::Cancelled) =>
                            {
                                return Ok(None);
                            }
                            Err(error) => return Err(error.to_string()),
                        };
                        let (bytes, _) = file
                            .load_contents_future()
                            .await
                            .map_err(|error| error.to_string())?;
                        let Some(owner) = owner.upgrade() else {
                            return Ok(None);
                        };
                        let result = rufin_core::metadata::prepare_artwork(&owner, bytes.to_vec());
                        drop(owner);
                        result
                            .recv()
                            .await
                            .map_err(|error| error.to_string())?
                            .map(Some)
                    }
                    .await;
                    finish(&weak, result).await;
                })));
        });
        let weak = Rc::downgrade(&editor);
        let owner = Arc::downgrade(source);
        regenerate_image.connect_clicked(move |_| {
            let (Some(editor), Some(owner)) = (weak.upgrade(), owner.upgrade()) else {
                return;
            };
            editor.begin();
            let result = regenerate(&owner);
            let weak = weak.clone();
            editor
                .task
                .replace(Some(gtk::glib::spawn_future_local(async move {
                    let result = result
                        .recv()
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result);
                    finish(&weak, result).await;
                })));
        });
        let weak = Rc::downgrade(&editor);
        editor.remove.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.cancel_task();
                editor.change.replace(Some(ArtworkChange::Remove));
                editor.has_image.set(false);
                editor.preview.set_paintable(None::<&gtk::gdk::Texture>);
                editor.set_busy(false);
                editor.current.replace(&editor.artwork, &fallback);
                editor.display.set_visible_child(&editor.current_host);
                editor.message("", false);
            }
        });
        editor
    }

    pub fn change(&self) -> Option<ArtworkChange> {
        self.change.borrow().clone()
    }

    pub fn saved(&self) {
        self.change.borrow_mut().take();
    }

    pub fn message(&self, text: &str, error: bool) {
        self.status.set_label(text);
        self.status.set_visible(!text.is_empty());
        if error {
            self.status.add_css_class("error");
        } else {
            self.status.remove_css_class("error");
        }
    }

    pub fn set_busy(&self, busy: bool) {
        self.content.set_sensitive(!busy);
        self.actions.set_sensitive(self.writable && !busy);
        self.remove.set_sensitive(self.has_image.get());
        self.apply.set_sensitive(!busy);
    }

    fn begin(&self) {
        self.cancel_task();
        self.set_busy(true);
        self.message("", false);
    }

    fn cancel_task(&self) {
        if let Some(task) = self.task.borrow_mut().take() {
            task.abort();
        }
    }
}

impl Drop for ArtworkEditor {
    fn drop(&mut self) {
        self.current.clear(&self.artwork);
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}

async fn finish(editor: &Weak<ArtworkEditor>, result: Result<Option<Arc<ImageBytes>>, String>) {
    let result = match result {
        Ok(Some(image)) => gtk_widgets::artwork::texture_from_image(Arc::clone(&image), 512)
            .await
            .map(|texture| Some((image, texture))),
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    };
    let Some(editor) = editor.upgrade() else {
        return;
    };
    match result {
        Ok(Some((image, texture))) => {
            editor.change.replace(Some(ArtworkChange::Replace(image)));
            editor.has_image.set(true);
            editor.preview.set_paintable(Some(&texture));
            editor.display.set_visible_child(&editor.preview);
            editor.message("", false);
        }
        Ok(None) => editor.message("", false),
        Err(error) => editor.message(&error, true),
    }
    editor.set_busy(false);
}
