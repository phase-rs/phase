//! The remote content channels the shell loads, and the one origin each names.
#![cfg_attr(mobile, allow(dead_code))]

use serde::{Deserialize, Serialize};

pub const RELEASE_ORIGIN: &str = "https://phase-rs.dev";
pub const PREVIEW_ORIGIN: &str = "https://preview.phase-rs.dev";

/// The remote content channel the shell loads.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    #[default]
    Release,
    Preview,
}

impl Channel {
    /// The first-party origin this channel is served from.
    pub const fn origin(self) -> &'static str {
        match self {
            Self::Release => RELEASE_ORIGIN,
            Self::Preview => PREVIEW_ORIGIN,
        }
    }
}
