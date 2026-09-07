use adw::prelude::*;

const SHOWCASE_PALETTE_CLASSES: [&str; 16] = [
    "seeded-gradient-palette-0",
    "seeded-gradient-palette-1",
    "seeded-gradient-palette-2",
    "seeded-gradient-palette-3",
    "seeded-gradient-palette-4",
    "seeded-gradient-palette-5",
    "seeded-gradient-palette-6",
    "seeded-gradient-palette-7",
    "seeded-gradient-palette-8",
    "seeded-gradient-palette-9",
    "seeded-gradient-palette-10",
    "seeded-gradient-palette-11",
    "seeded-gradient-palette-12",
    "seeded-gradient-palette-13",
    "seeded-gradient-palette-14",
    "seeded-gradient-palette-15",
];

pub fn stable_seed(value: &str) -> u32 {
    value.bytes().fold(0x811c_9dc5, |hash, byte| {
        hash.wrapping_mul(16_777_619) ^ u32::from(byte)
    })
}

pub fn add_album_seed_gradient_class(widget: &impl IsA<gtk::Widget>, seed: u32) {
    widget.add_css_class("seeded-gradient-showcase");
    for class in SHOWCASE_PALETTE_CLASSES {
        widget.remove_css_class(class);
    }
    let palette = seed ^ seed.rotate_left(13) ^ seed.rotate_right(9);
    widget
        .add_css_class(SHOWCASE_PALETTE_CLASSES[palette as usize % SHOWCASE_PALETTE_CLASSES.len()]);
}
