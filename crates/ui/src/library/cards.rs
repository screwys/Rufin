use crate::library::CatalogUi;
use crate::menus::media_menus::present_row_menu;
use crate::widgets::artwork::ArtworkTile;
use crate::widgets::cover_controls::{
    CoverFavorite, CoverHoverControls, CoverParts, CoverRating, constrain_cover_widget,
    cover_overlay, set_cover_tier,
};
use crate::widgets::interactions::ContextMenuOpen;
use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use library::FavoriteTarget;
use rufin_core::artwork::ArtworkBinding;
use std::cell::RefCell;
use std::rc::Rc;

use crate::widgets::library_fields::{COLLECTION_GRID_CARD_MARGIN, COLLECTION_GRID_MIN_CARD_WIDTH};
use rufin_core::route::Route;

/// An album's cover beside its tracks, with the grid's controls on hover.
pub struct AlbumCover {
    pub widget: gtk::Overlay,
    button: gtk::Button,
    tile: ArtworkTile,
    controls: CoverHoverControls,
    current: Rc<RefCell<Option<library::AlbumRow>>>,
}

impl AlbumCover {
    pub fn new(shell: &Rc<CatalogUi>) -> Self {
        let current = Rc::new(RefCell::new(None::<library::AlbumRow>));
        let button = gtk::Button::new();
        button.add_css_class("album-cover-button");
        button.add_css_class("flat");
        button.set_overflow(gtk::Overflow::Hidden);
        let tile = ArtworkTile::new_sized(1, 1);
        button.set_child(Some(&tile.widget()));
        let open_shell = Rc::downgrade(shell);
        let row = Rc::clone(&current);
        button.connect_clicked(move |_| {
            if let Some(shell) = open_shell.upgrade()
                && let Some(album) = row.borrow().clone()
            {
                shell.open_route(Route::AlbumDetail(album.media_uri.clone()));
            }
        });
        let menu_shell = Rc::downgrade(shell);
        let row = Rc::clone(&current);
        let open_menu: ContextMenuOpen = Rc::new(move |target, position| {
            if let Some(shell) = menu_shell.upgrade()
                && let Some(album) = row.borrow().clone()
            {
                present_row_menu(
                    target,
                    shell.shell(),
                    library::Row::Album(album),
                    None,
                    position,
                );
            }
        });
        let controls = CoverHoverControls::new(localization::msgid("Play album"), CoverParts::ALL);
        super::grid_cells::install_grid_play(
            shell,
            &current,
            [
                controls.play.clone(),
                controls.play_next.clone(),
                controls.play_last.clone(),
            ],
            |shell, album: &library::AlbumRow, placement| {
                crate::menus::media_menus::play_target(
                    &shell.products,
                    &crate::widgets::selection::PlayTarget::of_album(album),
                    placement,
                    false,
                );
            },
        );
        install_cover_marks(shell, &current, &controls, |album| {
            FavoriteTarget::Album(album.media_uri.clone())
        });
        let widget = cover_overlay(&button.clone().upcast(), &controls, Some(open_menu));
        Self {
            widget,
            button,
            tile,
            controls,
            current,
        }
    }

    pub fn bind(&self, shell: &CatalogUi, album: Option<&library::AlbumRow>) {
        self.current.replace(album.cloned());
        if let Some(album) = album {
            shell.artwork.bind_artwork_tile(
                &self.tile,
                album
                    .artwork_binding
                    .as_deref()
                    .map(ArtworkBinding::opaque)
                    .unwrap_or_default(),
                crate::library::route_layout::detail_showcase_cover_size(i32::MAX),
            );
            bind_cover_marks(
                shell,
                &self.controls,
                Some(album.state.favorite),
                Some(album.state.rating),
                &album.media_uri,
                None,
            );
        } else {
            shell.artwork.clear_artwork_tile(&self.tile);
            clear_cover_marks(&self.controls);
        }
    }

    pub fn resize(&self, size: i32) {
        constrain_cover_widget(&self.button, size);
        constrain_cover_widget(&self.widget, size);
        set_cover_tier(&self.widget, size);
        self.tile.set_square_size(size);
    }
}

/// Favorites and rates `current`'s row, `target`, from the cover's corner and stars.
pub fn install_cover_marks<T: 'static>(
    shell: &Rc<CatalogUi>,
    current: &Rc<RefCell<Option<T>>>,
    controls: &CoverHoverControls,
    target: impl Fn(&T) -> FavoriteTarget + 'static,
) {
    let target = Rc::new(target);
    if let Some(favorite) = &controls.favorite {
        let (shell, current, target) =
            (Rc::downgrade(shell), Rc::clone(current), Rc::clone(&target));
        favorite.connect_toggle(move |favorite| {
            if let Some(shell) = shell.upgrade()
                && let Some(row) = current.borrow().as_ref()
            {
                shell.set_favorite(target(row), favorite);
            }
        });
    }
    if let Some(rating) = &controls.rating {
        let (shell, current) = (Rc::downgrade(shell), Rc::clone(current));
        rating.connect_commit(move |rating| {
            if let Some(shell) = shell.upgrade()
                && let Some(row) = current.borrow().as_ref()
            {
                shell.set_rating(target(row), rating);
            }
        });
    }
}

/// Shows a row's favorite and rating on its cover; `None` for a row that takes neither.
pub fn bind_cover_marks(
    shell: &CatalogUi,
    controls: &CoverHoverControls,
    favorite: Option<bool>,
    rating: Option<Option<i64>>,
    media_uri: &str,
    source_id: Option<&str>,
) {
    if let Some(corner) = &controls.favorite {
        corner.set(favorite);
    }
    if let Some(stars) = &controls.rating {
        let half_stars = rating.is_some() && shell.half_stars_enabled(media_uri, source_id);
        stars.set(rating, half_stars);
    }
}

pub fn clear_cover_marks(controls: &CoverHoverControls) {
    if let Some(corner) = &controls.favorite {
        corner.set(None);
    }
    if let Some(stars) = &controls.rating {
        stars.set(None, false);
    }
}

pub mod collection_grid_cover_view_imp {
    use gtk::{CompositeTemplate, TemplateChild, glib, prelude::*, subclass::prelude::*};

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/app/rufin/rufin/ui/routes/collection_grid_cover.ui")]
    pub struct CollectionGridCoverView {
        #[template_child]
        pub overlay: TemplateChild<gtk::Overlay>,
        #[template_child]
        pub cover_host: TemplateChild<gtk::Box>,
        pub controls: std::cell::OnceCell<crate::widgets::cover_controls::CoverHoverControls>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CollectionGridCoverView {
        const NAME: &'static str = "RufinCollectionGridCoverView";
        type Type = super::CollectionGridCoverView;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for CollectionGridCoverView {
        fn dispose(&self) {
            self.dispose_template();
        }
    }

    impl WidgetImpl for CollectionGridCoverView {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Vertical {
                super::square_cover_vertical_measure(for_size)
            } else {
                self.overlay.measure(orientation, for_size)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            crate::widgets::cover_controls::set_cover_tier(&*self.overlay, width);
            self.overlay.allocate(width, height, baseline, None);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().snapshot_child(&*self.overlay, snapshot);
        }
    }
}

gtk::glib::wrapper! {
    pub struct CollectionGridCoverView(ObjectSubclass<collection_grid_cover_view_imp::CollectionGridCoverView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CollectionGridCoverView {
    /// A track's, album's or artist's cover: Play and its queue modes, More, the favorite
    /// and the rating.
    pub fn new(play_label: &str) -> Self {
        Self::build(play_label, CoverParts::ALL)
    }

    /// A cover of a row that takes no favorite or rating, as a playlist's.
    pub fn without_marks(play_label: &str) -> Self {
        Self::build(
            play_label,
            CoverParts {
                favorite: false,
                rating: false,
                ..CoverParts::ALL
            },
        )
    }

    fn build(play_label: &str, parts: CoverParts) -> Self {
        let view: Self = gtk::glib::Object::new();
        let imp = view.imp();
        let controls = CoverHoverControls::new(play_label, parts);
        controls.attach(&imp.overlay);
        let _ = imp.controls.set(controls);
        view.set_hexpand(true);
        view.set_halign(gtk::Align::Fill);
        view.set_valign(gtk::Align::Start);
        view.set_accessible_role(gtk::AccessibleRole::Presentation);
        view
    }

    /// Calls `open` on one click of the cover outside its controls, as a page header's cover
    /// opens its page; a click with Ctrl or Shift stays with the grid's selection.
    pub fn connect_open(&self, open: impl Fn() + 'static) {
        let overlay = self.imp().overlay.get();
        overlay.set_cursor_from_name(Some("pointer"));
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        click.connect_released(move |gesture, presses, _, _| {
            let selecting =
                gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK;
            if presses == 1 && !gesture.current_event_state().intersects(selecting) {
                open();
            }
        });
        overlay.add_controller(click);
    }

    pub fn controls(&self) -> &CoverHoverControls {
        self.imp()
            .controls
            .get()
            .expect("a grid cover builds its controls")
    }

    pub fn menu(&self) -> gtk::Button {
        self.controls().menu.clone().expect("a grid cover has More")
    }

    pub fn favorite(&self) -> &CoverFavorite {
        self.controls()
            .favorite
            .as_ref()
            .expect("a grid cover of a favorite has its corner")
    }

    pub fn rating(&self) -> Option<&CoverRating> {
        self.controls().rating.as_ref()
    }
}

mod collection_grid_card_inset_imp {
    use std::{
        any::Any,
        cell::{Cell, RefCell},
    };

    use gtk::{glib, prelude::*, subclass::prelude::*};

    #[derive(Default)]
    pub struct CollectionGridCardInset {
        pub minimum_content_width: Cell<i32>,
        pub inset: Cell<i32>,
        pub cell: RefCell<Option<Box<dyn Any>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CollectionGridCardInset {
        const NAME: &'static str = "RufinCollectionGridCardInset";
        type Type = super::CollectionGridCardInset;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for CollectionGridCardInset {
        fn dispose(&self) {
            self.cell.take();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for CollectionGridCardInset {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.obj()
                .first_child()
                .map(|child| child.request_mode())
                .unwrap_or(gtk::SizeRequestMode::ConstantSize)
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.obj().first_child() else {
                return (0, 0, -1, -1);
            };
            let total_inset = self.inset.get() * 2;
            if orientation == gtk::Orientation::Horizontal {
                return super::collection_grid_card_horizontal_measure(
                    self.minimum_content_width.get(),
                    self.inset.get(),
                );
            }
            let child_for_size = if for_size < 0 {
                -1
            } else {
                for_size.saturating_sub(total_inset).max(0)
            };
            let (minimum, natural, minimum_baseline, natural_baseline) =
                child.measure(orientation, child_for_size);
            let add_inset = |size: i32| size.saturating_add(total_inset);
            let add_baseline = |baseline: i32| {
                if baseline < 0 {
                    -1
                } else {
                    baseline.saturating_add(self.inset.get())
                }
            };
            (
                add_inset(minimum),
                add_inset(natural),
                add_baseline(minimum_baseline),
                add_baseline(natural_baseline),
            )
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.obj().first_child() else {
                return;
            };
            let (x, child_width) =
                super::collection_grid_card_inner_extent(width, self.inset.get());
            let (y, child_height) =
                super::collection_grid_card_inner_extent(height, self.inset.get());
            let child_baseline = if baseline < 0 {
                -1
            } else {
                baseline.saturating_sub(y).max(0)
            };
            let transform = gtk::gsk::Transform::new()
                .translate(&gtk::graphene::Point::new(x as f32, y as f32));
            child.allocate(child_width, child_height, child_baseline, Some(transform));
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.obj().first_child() {
                self.obj().snapshot_child(&child, snapshot);
            }
        }
    }
}

fn collection_grid_card_horizontal_measure(
    minimum_content_width: i32,
    inset: i32,
) -> (i32, i32, i32, i32) {
    let slot_width = minimum_content_width
        .max(1)
        .saturating_add(inset.saturating_mul(2));
    (slot_width, slot_width, -1, -1)
}

gtk::glib::wrapper! {
    pub struct CollectionGridCardInset(ObjectSubclass<collection_grid_card_inset_imp::CollectionGridCardInset>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CollectionGridCardInset {
    pub fn apply_display(&self, settings: &rufin_core::settings::layout::DisplaySettings) {
        let (minimum, _, inset) = super::grid_cells::grid_dimensions(settings);
        self.imp().minimum_content_width.set(minimum);
        self.imp().inset.set(inset);
        self.queue_resize();
    }
    pub fn set_cell<T: 'static>(&self, cell: T) {
        self.imp().cell.replace(Some(Box::new(cell)));
    }

    pub fn with_cell<T: 'static, R>(&self, apply: impl FnOnce(&T) -> R) -> Option<R> {
        let cell = self.imp().cell.borrow();
        cell.as_ref()?.downcast_ref::<T>().map(apply)
    }

    pub fn clear_cell(&self) {
        self.imp().cell.take();
    }
}

fn collection_grid_card_inner_extent(allocation: i32, inset: i32) -> (i32, i32) {
    let allocation = allocation.max(0);
    let leading = inset.min(allocation / 2);
    (leading, allocation.saturating_sub(leading * 2))
}

#[cfg(test)]
mod collection_grid_card_inset_tests {
    use super::*;

    #[test]
    fn preliminary_allocations_never_produce_a_negative_card_extent() {
        for allocation in 0..=COLLECTION_GRID_CARD_MARGIN * 2 {
            let (leading, inner) =
                collection_grid_card_inner_extent(allocation, COLLECTION_GRID_CARD_MARGIN);
            assert!(leading >= 0);
            assert!(leading <= COLLECTION_GRID_CARD_MARGIN);
            assert!(inner >= 0);
            assert_eq!(leading * 2 + inner, allocation);
        }
    }

    #[test]
    fn grid_slot_width_uses_the_configured_card_width() {
        let expected = COLLECTION_GRID_MIN_CARD_WIDTH + COLLECTION_GRID_CARD_MARGIN * 2;
        assert_eq!(
            collection_grid_card_horizontal_measure(
                COLLECTION_GRID_MIN_CARD_WIDTH,
                COLLECTION_GRID_CARD_MARGIN
            ),
            (expected, expected, -1, -1)
        );
    }

    #[test]
    fn square_cover_height_follows_the_allocated_width() {
        assert_eq!(square_cover_vertical_measure(360), (360, 360, -1, -1));
        assert_eq!(square_cover_vertical_measure(180), (180, 180, -1, -1));
        assert_eq!(
            square_cover_vertical_measure(-1),
            (
                COLLECTION_GRID_MIN_CARD_WIDTH,
                COLLECTION_GRID_MIN_CARD_WIDTH,
                -1,
                -1
            )
        );
    }
}

pub fn collection_grid_card_inset(
    child: &impl IsA<gtk::Widget>,
    minimum_content_width: i32,
) -> CollectionGridCardInset {
    use gtk::subclass::prelude::ObjectSubclassIsExt;

    let minimum_content_width = minimum_content_width.max(1);
    let inset: CollectionGridCardInset = gtk::glib::Object::new();
    inset.imp().minimum_content_width.set(minimum_content_width);
    inset.imp().inset.set(COLLECTION_GRID_CARD_MARGIN);
    inset.set_hexpand(true);
    inset.set_halign(gtk::Align::Fill);
    inset.set_valign(gtk::Align::Start);
    inset.set_accessible_role(gtk::AccessibleRole::Presentation);
    child.set_width_request(minimum_content_width);
    child.set_parent(&inset);
    inset
}

mod square_cover_frame_imp {
    use gtk::{glib, prelude::*, subclass::prelude::*};

    #[derive(Default)]
    pub struct SquareCoverFrame;

    #[glib::object_subclass]
    impl ObjectSubclass for SquareCoverFrame {
        const NAME: &'static str = "RufinSquareCoverFrame";
        type Type = super::SquareCoverFrame;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SquareCoverFrame {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for SquareCoverFrame {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Vertical {
                super::square_cover_vertical_measure(for_size)
            } else {
                self.obj()
                    .first_child()
                    .map(|child| child.measure(orientation, for_size))
                    .unwrap_or((0, 0, -1, -1))
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.obj().first_child() {
                child.allocate(width, height, baseline, None);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.obj().first_child() {
                self.obj().snapshot_child(&child, snapshot);
            }
        }
    }
}

fn square_cover_vertical_measure(for_size: i32) -> (i32, i32, i32, i32) {
    let size = if for_size >= 0 {
        for_size
    } else {
        COLLECTION_GRID_MIN_CARD_WIDTH
    };
    (size, size, -1, -1)
}

gtk::glib::wrapper! {
    pub struct SquareCoverFrame(ObjectSubclass<square_cover_frame_imp::SquareCoverFrame>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

pub fn square_cover_frame(child: &impl IsA<gtk::Widget>) -> SquareCoverFrame {
    let frame: SquareCoverFrame = gtk::glib::Object::new();
    frame.set_hexpand(true);
    frame.set_halign(gtk::Align::Fill);
    frame.set_valign(gtk::Align::Start);
    frame.set_accessible_role(gtk::AccessibleRole::Presentation);
    child.set_parent(&frame);
    frame
}
