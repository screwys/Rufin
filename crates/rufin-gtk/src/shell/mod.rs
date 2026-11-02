//! Desktop GTK composition, navigation admission and native actions.
//! Pages and the persistent player live in their own modules.

mod application;
pub(crate) mod player;
pub(crate) mod shell;

pub use application::{
    ApplicationExit, run_application, run_application_after_update, run_startup_error_application,
};
