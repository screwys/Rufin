#[path = "../gtk-widgets/build_support.rs"]
mod build_support;
fn main() {
    build_support::build(false, "gtk-preferences.gresource");
}
