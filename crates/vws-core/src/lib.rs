//! Core of Valheim World Sync: everything that is not UI.
//!
//! - [`protocol`]: the hosting protocol from PLAN.md section 5 (lock, heartbeat,
//!   publishing saves, side copies, first import, install).
//! - [`store`]: the remote. [`store::github::GitHubStore`] talks to GitHub;
//!   [`store::dir::DirStore`] is a local stand-in with the same semantics, used
//!   by tests and for offline runs of the CLI harness.
//! - [`world`] and [`archive`]: the world on disk and the save zips.
//! - [`journal`]: what this PC is in the middle of, so a crash resumes.

pub mod archive;
pub mod error;
pub mod journal;
pub mod model;
pub mod mods;
pub mod paths;
pub mod protocol;
pub mod settings;
pub mod setup_code;
pub mod store;
pub mod util;
pub mod world;

pub use error::{Error, Result};
