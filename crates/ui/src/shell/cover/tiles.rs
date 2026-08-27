use std::{cell::Cell, rc::Rc};

use adw::prelude::*;
use artwork::ArtworkBinding;

use super::{ArtworkTile, cover_fetch_size_for_display};
use crate::shell::Shell;

#[derive(Clone)]
pub(crate) struct CoverGroupProjection {
    root: gtk::Stack,
    single: ArtworkTile,
    grid: gtk::Grid,
    quadrants: Rc<Vec<ArtworkTile>>,
    size: Rc<Cell<i32>>,
    render_size: i32,
    fetch_size: u32,
}

impl CoverGroupProjection {
    pub(crate) fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub(crate) fn replace(&self, shell: &Rc<Shell>, artwork: &[ArtworkBinding]) {
        if artwork.len() <= 1 {
            for tile in self.quadrants.iter() {
                shell.clear_artwork_tile(tile);
            }
            shell.bind_artwork_tile(
                &self.single,
                artwork.first().cloned().unwrap_or_else(ArtworkBinding::new),
                self.render_size,
                self.fetch_size,
            );
            self.root.set_visible_child_name("single");
            return;
        }

        shell.clear_artwork_tile(&self.single);
        let cell_size = (self.render_size / 2).max(1);
        for (index, tile) in self.quadrants.iter().enumerate() {
            shell.bind_artwork_tile(
                tile,
                artwork[index % artwork.len()].clone(),
                cell_size,
                self.fetch_size,
            );
        }
        self.root.set_visible_child_name("grid");
    }

    pub(crate) fn resize(&self, size: i32) {
        let size = size.max(1);
        if self.size.replace(size) == size {
            return;
        }
        self.root.set_size_request(size, size);
        self.grid.set_size_request(size, size);
        self.single.set_square_size(size);
        let cell_size = (size / 2).max(1);
        for tile in self.quadrants.iter() {
            tile.set_square_size(cell_size);
        }
    }
}

impl Shell {
    pub(crate) fn cover_group_projection_for_artwork(
        self: &Rc<Self>,
        artwork: &[ArtworkBinding],
        size: i32,
        render_size: i32,
    ) -> CoverGroupProjection {
        let render_size = render_size.max(1);
        let fetch_size = cover_fetch_size_for_display(render_size);
        let root = gtk::Stack::new();
        root.set_size_request(size, size);
        root.set_hexpand(false);
        root.set_vexpand(false);
        root.set_halign(gtk::Align::Start);
        root.set_valign(gtk::Align::Start);

        let single = ArtworkTile::new_sized(size, size);
        root.add_named(&single.widget(), Some("single"));

        let grid = gtk::Grid::new();
        grid.add_css_class("cover-tile");
        grid.add_css_class("card");
        grid.set_size_request(size, size);
        grid.set_overflow(gtk::Overflow::Hidden);
        grid.set_row_homogeneous(true);
        grid.set_column_homogeneous(true);
        let cell_size = (size / 2).max(1);
        let quadrants = Rc::new(
            (0..4)
                .map(|index| {
                    let tile = ArtworkTile::new_sized(cell_size, cell_size);
                    grid.attach(&tile.widget(), (index % 2) as i32, (index / 2) as i32, 1, 1);
                    tile
                })
                .collect::<Vec<_>>(),
        );
        root.add_named(&grid, Some("grid"));

        let projection = CoverGroupProjection {
            root,
            single,
            grid,
            quadrants,
            size: Rc::new(Cell::new(size)),
            render_size,
            fetch_size,
        };
        projection.replace(self, artwork);
        projection
    }
}
