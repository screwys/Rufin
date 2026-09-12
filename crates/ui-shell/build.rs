#[path = "../ui-shared/build_support.rs"]
mod build_support;
fn main() {
    println!("cargo:rerun-if-changed=../../data/showcase.css");
    build_support::build(false, "rufin.gresource");
}
