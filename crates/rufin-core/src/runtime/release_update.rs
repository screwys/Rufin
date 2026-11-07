//! Release-update results presented by the UI.
//!
//! Rufin owns acquisition and notification policy. UI owns only when to start
//! the launch check and how to present its complete result.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ReleaseNote {
    pub version: String,
    pub date: String,
    pub url: String,
    pub body: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ReleaseHistory {
    pub notes: Vec<ReleaseNote>,
    pub installed_version: String,
    pub available_version: Option<String>,
    pub automatic_updates_supported: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ReleaseUpdate {
    Refreshed {
        history: ReleaseHistory,
        notification_version: Option<String>,
    },
    Updating {
        version: String,
    },
    Ready {
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

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ReleaseSubscription {
    initial: std::sync::Mutex<Option<ReleaseHistory>>,
    events: async_channel::Receiver<ReleaseUpdate>,
}
impl ReleaseSubscription {
    pub fn new(
        initial: ReleaseHistory,
        events: async_channel::Receiver<ReleaseUpdate>,
    ) -> Arc<Self> {
        Arc::new(Self {
            initial: std::sync::Mutex::new(Some(initial)),
            events,
        })
    }
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl ReleaseSubscription {
    pub async fn next(&self) -> Result<ReleaseUpdate, crate::settings::SettingsError> {
        if let Some(history) = self
            .initial
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            return Ok(ReleaseUpdate::Refreshed {
                history,
                notification_version: None,
            });
        }
        self.events
            .recv()
            .await
            .map_err(|error| error.to_string().into())
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn release_note_html(body: String) -> String {
    let mut html = String::new();
    pulldown_cmark::html::push_html(
        &mut html,
        pulldown_cmark::Parser::new_ext(
            &body,
            pulldown_cmark::Options::ENABLE_TABLES | pulldown_cmark::Options::ENABLE_STRIKETHROUGH,
        ),
    );
    html
}
