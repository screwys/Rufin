#[path = "../ui-shared/build_support.rs"]
mod build_support;
fn main() {
    build_support::build(false, "rufin.gresource");
}
