//! Rufin's application identity shared by runtime and platform integrations.

pub const STABLE_APP_ID: &str = "io.github.screwys.Rufin";
pub const STABLE_PROJECT_NAME: &str = "Rufin";

#[cfg(feature = "development")]
pub const APP_ID: &str = "io.github.screwys.Rufin.Devel";
#[cfg(not(feature = "development"))]
pub const APP_ID: &str = STABLE_APP_ID;

#[cfg(feature = "development")]
pub const PROJECT_NAME: &str = "Rufin.Devel";
#[cfg(not(feature = "development"))]
pub const PROJECT_NAME: &str = STABLE_PROJECT_NAME;
