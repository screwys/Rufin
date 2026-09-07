//! Release-update results presented by the UI.
//!
//! Rufin owns acquisition and notification policy. UI owns only when to start
//! the launch check and how to present its complete result.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReleaseNote {
    pub version: String,
    pub date: String,
    pub url: String,
    pub body: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseHistory {
    pub notes: Arc<[ReleaseNote]>,
    pub installed_version: String,
    pub available_version: Option<String>,
    pub automatic_updates_supported: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReleaseUpdate {
    Refreshed {
        history: ReleaseHistory,
        notification_version: Option<String>,
    },
    Updating {
        version: String,
    },
    Updated {
        version: String,
        restart_required: bool,
    },
    Failed {
        version: String,
        error: String,
    },
    Restarting {
        version: String,
    },
}

pub type ReleaseUpdateHandle = Arc<crate::release_update::ReleaseUpdateOwner>;
