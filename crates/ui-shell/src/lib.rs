//! Desktop GTK composition, navigation admission and native actions.
//! Catalog and persistent player presentation live in their own UI crates.

mod application;
mod downloads;
mod favorites;
pub(crate) mod player;
mod preferences;
mod ratings;
mod shell;
mod ui_resource;

pub use application::{
    run_application, run_application_after_update, run_startup_error_application,
};

use rufin_core::settings::*;
