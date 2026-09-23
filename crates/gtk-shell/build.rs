#[path = "../gtk-widgets/build_support.rs"]
mod build_support;
fn main() {
    println!("cargo:rerun-if-changed=../../resources/showcase.css");
    build_support::build(false, "rufin.gresource");
}
