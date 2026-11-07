use std::{cell::Cell, rc::Rc};

use adw::prelude::*;
use artwork::ArtworkBinding;

use super::ArtworkState;
use super::ArtworkTile;

#[derive(Clone)]
pub struct CoverGroupProjection {
    root: gtk::Stack,
    single: ArtworkTile,
    grid: gtk::Grid,
    quadrants: Rc<Vec<ArtworkTile>>,
    size: Rc<Cell<i32>>,
    render_size: i32,
}

impl CoverGroupProjection {
    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub fn replace(&self, shell: &Rc<ArtworkState>, artwork: &[ArtworkBinding]) {
        if artwork.len() <= 1 {
            for tile in self.quadrants.iter() {
                shell.clear_artwork_tile(tile);
            }
            shell.bind_artwork_tile(
                &self.single,
                artwork.first().cloned().unwrap_or_else(ArtworkBinding::new),
                self.render_size,
            );
            self.root.set_visible_child_name("single");
            return;
        }

        shell.clear_artwork_tile(&self.single);
        let cell_size = (self.render_size / 2).max(1);
        for (index, tile) in self.quadrants.iter().enumerate() {
            shell.bind_artwork_tile(tile, artwork[index % artwork.len()].clone(), cell_size);
        }
        self.root.set_visible_child_name("grid");
    }

    pub fn clear(&self, shell: &Rc<ArtworkState>) {
        shell.clear_artwork_tile(&self.single);
        for tile in self.quadrants.iter() {
            shell.clear_artwork_tile(tile);
        }
    }

    pub fn resize(&self, size: i32) {
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

impl ArtworkState {
    pub fn cover_group_projection_for_artwork(
        self: &Rc<Self>,
        artwork: &[ArtworkBinding],
        size: i32,
        render_size: i32,
        mosaic: bool,
    ) -> CoverGroupProjection {
        let render_size = render_size.max(1);
        self.cover_group_projection(artwork, Some(size.max(1)), render_size, mosaic)
    }

    pub fn elastic_cover_group_projection_for_artwork(
        self: &Rc<Self>,
        artwork: &[ArtworkBinding],
        render_size: i32,
        mosaic: bool,
    ) -> CoverGroupProjection {
        self.cover_group_projection(artwork, None, render_size.max(1), mosaic)
    }

    fn cover_group_projection(
        self: &Rc<Self>,
        artwork: &[ArtworkBinding],
        size: Option<i32>,
        render_size: i32,
        mosaic: bool,
    ) -> CoverGroupProjection {
        let root = gtk::Stack::new();
        if let Some(size) = size {
            root.set_size_request(size, size);
            root.set_hexpand(false);
            root.set_vexpand(false);
            root.set_halign(gtk::Align::Start);
            root.set_valign(gtk::Align::Start);
        } else {
            root.set_hexpand(true);
            root.set_vexpand(true);
            root.set_halign(gtk::Align::Fill);
            root.set_valign(gtk::Align::Fill);
        }

        let single = size.map_or_else(ArtworkTile::new_elastic_square, ArtworkTile::new);
        root.add_named(&single.widget(), Some("single"));

        let grid = gtk::Grid::new();
        grid.add_css_class("cover-tile");
        grid.add_css_class("cover-mosaic");
        grid.add_css_class("card");
        grid.set_overflow(gtk::Overflow::Hidden);
        grid.set_row_homogeneous(true);
        grid.set_column_homogeneous(true);
        if let Some(size) = size {
            grid.set_size_request(size, size);
        } else {
            grid.set_hexpand(true);
            grid.set_vexpand(true);
        }
        let cell_size = size.map(|size| (size / 2).max(1));
        let quadrants = Rc::new(
            (0..if mosaic { 4 } else { 0 })
                .map(|index| {
                    let tile = cell_size
                        .map(ArtworkTile::new)
                        .unwrap_or_else(ArtworkTile::new_elastic_square);
                    let widget = tile.widget();
                    widget.set_hexpand(true);
                    widget.set_vexpand(true);
                    widget.set_halign(gtk::Align::Fill);
                    widget.set_valign(gtk::Align::Fill);
                    grid.attach(&widget, (index % 2) as i32, (index / 2) as i32, 1, 1);
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
            size: Rc::new(Cell::new(size.unwrap_or(1))),
            render_size,
        };
        projection.replace(self, artwork);
        projection
    }
}
