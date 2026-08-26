use std::{cell::Cell, rc::Rc};

use adw::prelude::*;
use artwork::ArtworkBinding;

use super::{ArtworkTile, LARGE_COVER_SIZE, MEDIUM_COVER_SIZE, cover_fetch_size_for_display};
use crate::shell::Shell;

fn elastic_cover_fetch_size(artwork_count: usize, mosaic_fetch_size: u32) -> u32 {
    if artwork_count == 1 {
        LARGE_COVER_SIZE
    } else {
        mosaic_fetch_size
    }
}

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

    pub(crate) fn elastic_cover_tile_for_candidates(
        self: &Rc<Self>,
        candidates: ArtworkBinding,
        fetch_size: u32,
    ) -> (gtk::Widget, ArtworkTile) {
        let tile = ArtworkTile::new_elastic_square();
        let widget = tile.widget();
        self.bind_artwork_tile(&tile, candidates, MEDIUM_COVER_SIZE as i32, fetch_size);
        (widget, tile)
    }

    pub(crate) fn elastic_cover_group_tile_for_artwork(
        self: &Rc<Self>,
        artwork: &[ArtworkBinding],
        mosaic_fetch_size: u32,
    ) -> gtk::Widget {
        let fetch_size = elastic_cover_fetch_size(artwork.len(), mosaic_fetch_size);
        match artwork.len() {
            0 => {
                self.elastic_cover_tile_for_candidates(ArtworkBinding::new(), fetch_size)
                    .0
            }
            1 => {
                self.elastic_cover_tile_for_candidates(artwork[0].clone(), fetch_size)
                    .0
            }
            _ => {
                let grid = gtk::Grid::new();
                grid.add_css_class("cover-tile");
                grid.add_css_class("card");
                grid.set_overflow(gtk::Overflow::Hidden);
                grid.set_row_homogeneous(true);
                grid.set_column_homogeneous(true);
                grid.set_hexpand(true);
                grid.set_vexpand(true);
                grid.set_halign(gtk::Align::Fill);
                grid.set_valign(gtk::Align::Fill);

                for index in 0..4 {
                    let child = self
                        .elastic_cover_tile_for_candidates(
                            artwork[index % artwork.len()].clone(),
                            fetch_size,
                        )
                        .0;
                    grid.attach(&child, (index % 2) as i32, (index / 2) as i32, 1, 1);
                }
                grid.upcast()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::elastic_cover_fetch_size;
    use crate::shell::cover::{LARGE_COVER_SIZE, THUMB_COVER_SIZE};

    #[test]
    fn a_single_elastic_grid_cover_uses_the_large_fetch_size() {
        assert_eq!(
            elastic_cover_fetch_size(1, THUMB_COVER_SIZE),
            LARGE_COVER_SIZE
        );
        assert_eq!(
            elastic_cover_fetch_size(4, THUMB_COVER_SIZE),
            THUMB_COVER_SIZE
        );
    }
}
