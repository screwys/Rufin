use super::Shell;
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk_widgets::artwork::{ArtworkTile, cover_fetch_size_for_display};
use gtk_widgets::interactions::add_widget_click;
use std::rc::Rc;
impl Shell {
    pub(crate) fn present_full_artwork(self: &Rc<Self>, candidates: ArtworkBinding) {
        let size = full_artwork_size(self.chrome.window.width(), self.chrome.window.height());
        let fetch_size = cover_fetch_size_for_display(size);
        let tile = ArtworkTile::new_sized(size, size);
        let cover = tile.widget();
        self.artwork
            .bind_playback_artwork_tile(&tile, candidates, size, fetch_size);
        let tile = tile.downgrade();
        let artwork = Rc::downgrade(&self.artwork);
        self.present_artwork_overlay(&cover, move || {
            if let (Some(artwork), Some(tile)) = (artwork.upgrade(), tile.upgrade()) {
                artwork.clear_artwork_tile(&tile);
            }
        });
    }

    pub(crate) fn present_full_artwork_group(self: &Rc<Self>, bindings: Vec<ArtworkBinding>) {
        if bindings.len() <= 1 {
            self.present_full_artwork(bindings.into_iter().next().unwrap_or_default());
            return;
        }
        let size = full_artwork_size(self.chrome.window.width(), self.chrome.window.height());
        let group = self
            .artwork
            .cover_group_projection_for_artwork(&bindings, size, size);
        let cover = group.widget();
        let artwork = Rc::downgrade(&self.artwork);
        self.present_artwork_overlay(&cover, move || {
            if let Some(artwork) = artwork.upgrade() {
                group.clear(&artwork);
            }
        });
    }

    fn present_artwork_overlay(&self, cover: &gtk::Widget, clear: impl Fn() + 'static) {
        cover.add_css_class("full-artwork-cover");
        cover.set_halign(gtk::Align::Center);
        cover.set_valign(gtk::Align::Center);

        let root = gtk::Overlay::new();
        root.add_css_class("full-artwork-window");
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_child(Some(cover));

        self.chrome.app_root_overlay.add_overlay(&root);
        self.chrome
            .app_root_overlay
            .set_measure_overlay(&root, false);

        let overlay = self.chrome.app_root_overlay.downgrade();
        let root_for_close = root.downgrade();
        add_widget_click(root.upcast_ref(), move || {
            clear();
            if let (Some(overlay), Some(root)) = (overlay.upgrade(), root_for_close.upgrade()) {
                overlay.remove_overlay(&root);
            }
        });
    }
}
fn full_artwork_size(width: i32, height: i32) -> i32 {
    (width.min(height) - 80).clamp(240, 720)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_artwork_size_fits_window() {
        assert_eq!(full_artwork_size(300, 300), 240);
        assert_eq!(full_artwork_size(4_000, 3_000), 720);
        assert!(full_artwork_size(900, 700) <= 700);
    }
}
