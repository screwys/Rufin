use crate::CatalogUi;
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use localization::msgid;
use playback::QueuePlacement;
use rufin_core::playback::PlaybackTarget;
use std::rc::Rc;
use ui_shared::artwork::{ArtworkTile, LARGE_COVER_SIZE};
use ui_shared::controls::{ActionButtonVariant, configure_action_button};
use ui_shared::cover_controls::{
    COVER_CORNER_HORIZONTAL_INSET, COVER_CORNER_VERTICAL_INSET, showcase_cover_overlay,
};
use ui_shared::favorites::{favorite_button_is_active, favorite_icon_button};
use ui_shared::interactions::ContextMenuOpen;
use ui_shared::media_menus::present_album_context_menu;

use ui_shared::library_fields::{COLLECTION_GRID_CARD_MARGIN, COLLECTION_GRID_MIN_CARD_WIDTH};
use ui_shared::localization::{bind_widget_accessible_label, bind_widget_tooltip};
use ui_shared::route::Route;

pub fn album_cover_overlay(
    shell: &Rc<CatalogUi>,
    album: &library::AlbumRow,
    size: i32,
) -> gtk::Widget {
    let album_button = gtk::Button::new();
    album_button.add_css_class("album-cover-button");
    album_button.add_css_class("flat");
    constrain_cover_widget(&album_button, size);
    album_button.set_overflow(gtk::Overflow::Hidden);
    let tile = ArtworkTile::new_sized(size, size);
    shell.artwork.bind_artwork_tile(
        &tile,
        album
            .artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default(),
        crate::route_layout::detail_showcase_cover_size(i32::MAX),
        LARGE_COVER_SIZE,
    );
    album_button.set_child(Some(&tile.widget()));
    let open_shell = Rc::clone(shell);
    let album_uri = album.media_uri.clone();
    album_button
        .connect_clicked(move |_| open_shell.navigate(Route::AlbumDetail(album_uri.clone())));

    let menu_shell = Rc::clone(shell);
    let menu_album = album.clone();
    let open_menu: ContextMenuOpen = Rc::new(move |target, position| {
        present_album_context_menu(
            target,
            &menu_shell.media_menus,
            menu_album.clone(),
            None,
            None,
            position,
        );
    });
    let (controls, favorite) = cover_hover_controls_with_favorite(0, "Play album", album.favorite);
    for (button, placement) in [
        (&controls.play, QueuePlacement::Now),
        (&controls.play_next, QueuePlacement::Next),
        (&controls.play_last, QueuePlacement::Last),
    ] {
        let play_shell = Rc::clone(shell);
        let target = PlaybackTarget::Album(album.media_uri.clone());
        button.connect_clicked(move |_| (play_shell.media_menus.play_target)(&target, placement));
    }
    let favorite_key = album.media_uri.clone();
    shell.register_dynamic_favorite_button(
        Rc::new(move || Some(ui_shared::favorites::album_favorite_key(&favorite_key))),
        &favorite,
    );
    let favorite_shell = Rc::clone(shell);
    let favorite_media_uri = album.media_uri.clone();
    favorite.connect_clicked(move |button| {
        favorite_shell.set_favorite_with_feedback(
            library::FavoriteTarget::Album(favorite_media_uri.clone()),
            !favorite_button_is_active(button),
            Some(button),
        );
    });
    let overlay = showcase_cover_overlay(&album_button.clone().upcast(), controls, Some(open_menu));
    constrain_cover_widget(&overlay, size);
    overlay.upcast()
}

pub mod collection_grid_cover_view_imp {
    use gtk::{CompositeTemplate, TemplateChild, glib, prelude::*, subclass::prelude::*};

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/screwys/Rufin/ui/routes/collection_grid_cover.ui")]
    pub struct CollectionGridCoverView {
        #[template_child]
        pub overlay: TemplateChild<gtk::Overlay>,
        #[template_child]
        pub cover_host: TemplateChild<gtk::Box>,
        #[template_child]
        pub shade: TemplateChild<gtk::Box>,
        #[template_child]
        pub transport: TemplateChild<gtk::Box>,
        #[template_child]
        pub play_next: TemplateChild<gtk::Button>,
        #[template_child]
        pub play: TemplateChild<gtk::Button>,
        #[template_child]
        pub play_last: TemplateChild<gtk::Button>,
        #[template_child]
        pub menu: TemplateChild<gtk::Button>,
        pub favorite: std::cell::RefCell<Option<gtk::Button>>,
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
            let spacing = super::cover_hover_transport_spacing(width);
            if self.transport.spacing() != spacing {
                self.transport.set_spacing(spacing);
            }
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
    pub fn new(play_label: &str) -> Self {
        Self::build(play_label, true)
    }

    pub fn without_favorite(play_label: &str) -> Self {
        Self::build(play_label, false)
    }

    fn build(play_label: &str, with_favorite: bool) -> Self {
        let view: Self = gtk::glib::Object::new();
        let imp = view.imp();
        for (button, label, variant) in [
            (
                &*imp.play_next,
                msgid("Play Next"),
                ActionButtonVariant::CoverSideTransport,
            ),
            (
                &*imp.play,
                play_label,
                ActionButtonVariant::CoverPrimaryTransport,
            ),
            (
                &*imp.play_last,
                msgid("Play Later"),
                ActionButtonVariant::CoverSideTransport,
            ),
        ] {
            bind_widget_tooltip(button, label);
            configure_action_button(button, variant);
        }
        bind_widget_accessible_label(&*imp.menu, msgid("More actions"));
        configure_action_button(&imp.menu, ActionButtonVariant::CoverCornerMenu);
        let favorite = with_favorite.then(|| {
            let favorite = favorite_icon_button(msgid("Favorite"));
            configure_action_button(&favorite, ActionButtonVariant::CoverCornerFavorite);
            favorite.set_margin_top(COVER_CORNER_VERTICAL_INSET);
            favorite.set_margin_end(COVER_CORNER_HORIZONTAL_INSET);
            favorite.set_visible(false);
            imp.overlay.add_overlay(&favorite);
            favorite
        });
        imp.favorite.replace(favorite.clone());
        let controls = CoverHoverControls {
            shade: imp.shade.get(),
            transport: imp.transport.get(),
            play_next: imp.play_next.get(),
            play: imp.play.get(),
            play_last: imp.play_last.get(),
            favorite,
            menu: Some(imp.menu.get()),
        };
        controls.connect_hover(&imp.overlay);
        view.set_hexpand(true);
        view.set_halign(gtk::Align::Fill);
        view.set_valign(gtk::Align::Start);
        view.set_accessible_role(gtk::AccessibleRole::Presentation);
        view
    }

    pub fn favorite(&self) -> gtk::Button {
        self.imp()
            .favorite
            .borrow()
            .clone()
            .expect("Collection grid cover has a favorite button")
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
            let total_inset = super::COLLECTION_GRID_CARD_MARGIN * 2;
            if orientation == gtk::Orientation::Horizontal {
                return super::collection_grid_card_horizontal_measure(
                    self.minimum_content_width.get(),
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
                    baseline.saturating_add(super::COLLECTION_GRID_CARD_MARGIN)
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
            let (x, child_width) = super::collection_grid_card_inner_extent(width);
            let (y, child_height) = super::collection_grid_card_inner_extent(height);
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

fn collection_grid_card_horizontal_measure(minimum_content_width: i32) -> (i32, i32, i32, i32) {
    let slot_width = minimum_content_width
        .max(1)
        .saturating_add(COLLECTION_GRID_CARD_MARGIN.saturating_mul(2));
    (slot_width, slot_width, -1, -1)
}

gtk::glib::wrapper! {
    pub struct CollectionGridCardInset(ObjectSubclass<collection_grid_card_inset_imp::CollectionGridCardInset>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CollectionGridCardInset {
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

fn collection_grid_card_inner_extent(allocation: i32) -> (i32, i32) {
    let allocation = allocation.max(0);
    let leading = COLLECTION_GRID_CARD_MARGIN.min(allocation / 2);
    (leading, allocation.saturating_sub(leading * 2))
}

#[cfg(test)]
mod collection_grid_card_inset_tests {
    use super::*;

    #[test]
    fn preliminary_allocations_never_produce_a_negative_card_extent() {
        for allocation in 0..=COLLECTION_GRID_CARD_MARGIN * 2 {
            let (leading, inner) = collection_grid_card_inner_extent(allocation);
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
            collection_grid_card_horizontal_measure(COLLECTION_GRID_MIN_CARD_WIDTH),
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
    pub struct SquareCoverFrame {
        pub transport: glib::WeakRef<gtk::Box>,
    }

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
            if let Some(transport) = self.transport.upgrade() {
                let spacing = super::cover_hover_transport_spacing(width);
                if transport.spacing() != spacing {
                    transport.set_spacing(spacing);
                }
            }
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

pub fn square_cover_frame(
    child: &impl IsA<gtk::Widget>,
    transport: Option<&gtk::Box>,
) -> SquareCoverFrame {
    use gtk::subclass::prelude::ObjectSubclassIsExt;

    let frame: SquareCoverFrame = gtk::glib::Object::new();
    if let Some(transport) = transport {
        frame.imp().transport.set(Some(transport));
    }
    frame.set_hexpand(true);
    frame.set_halign(gtk::Align::Fill);
    frame.set_valign(gtk::Align::Start);
    frame.set_accessible_role(gtk::AccessibleRole::Presentation);
    child.set_parent(&frame);
    frame
}

use ui_shared::cover_controls::{
    CoverHoverControls, constrain_cover_widget, cover_hover_controls_with_favorite,
    cover_hover_transport_spacing,
};
