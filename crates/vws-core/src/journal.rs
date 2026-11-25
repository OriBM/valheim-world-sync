//! What this PC is in the middle of (PLAN.md 3, 5.7). Saved after every step
//! that matters, so a crash or reboot resumes instead of losing track.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{IoContext, Result};
use crate::model::{SCHEMA, SaveKind};
use crate::util;
use crate::world::FileEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalPhase {
    #[default]
    Idle,
    /// Lock taken, world being downloaded and put in place.
    Installing,
    /// World installed; the session runs until Finish, Cancel or Lost lock.
    Hosting,
}

/// This PC's hosting session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub base_generation: u64,
    /// GitHub's commit time of the acquiring write; the first heartbeat copies it to `lock.since`.
    #[serde(default)]
    pub acquired_at: Option<String>,
    #[serde(default)]
    pub since_written: bool,
    /// I7: once set, this PC writes neither `lock` nor `latest` again in this session.
    #[serde(default)]
    pub lost: bool,
}

/// A save that is uploaded and verified but not yet referenced by `state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingUpload {
    pub asset_id: u64,
    pub asset_name: String,
    pub sha256: String,
    pub size: u64,
    pub generation: u64,
    pub kind: SaveKind,
    pub saved_at: String,
    pub saved_by: String,
    pub game_build: String,
    pub game_version: String,
    pub mods_fingerprint: String,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Journal {
    pub schema: u32,
    pub phase: JournalPhase,
    pub world_name: Option<String>,
    pub session: Option<Session>,
    /// The world's files as installed at the start of the session (5.4 step 1, Cancel).
    pub installed: Option<Vec<FileEntry>>,
    /// The world's files in the last save this PC uploaded during the session.
    pub last_uploaded: Option<Vec<FileEntry>>,
    pub pending: Option<PendingUpload>,
    /// S6: Valheim was seen running without mods during this session.
    pub started_without_mods: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Journal {
    /// A missing journal means idle.
    pub fn load(path: &Path) -> Result<Journal> {
        match std::fs::read(path) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Journal::default()),
            Err(e) => Err(e).at(path),
        }
    }

    pub fn save(&mut self, path: &Path) -> Result<()> {
        self.schema = SCHEMA;
        util::write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }

    /// Back to idle, keeping nothing from the session.
    pub fn reset(&mut self) {
        *self = Journal::default();
    }

    pub fn is_idle(&self) -> bool {
        self.phase == JournalPhase::Idle
    }

    /// True when the world on disk is byte-identical to what was installed or
    /// to the last save this PC uploaded (5.4 step 1, 5.6 step 2).
    pub fn unchanged(&self, current: &[FileEntry]) -> bool {
        self.installed.as_deref() == Some(current) || self.last_uploaded.as_deref() == Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_journal_is_idle_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("journal.json");
        let mut j = Journal::load(&p).unwrap();
        assert!(j.is_idle());
        j.phase = JournalPhase::Hosting;
        j.session = Some(Session {
            session_id: "s".into(),
            base_generation: 4,
            acquired_at: None,
            since_written: false,
            lost: false,
        });
        j.save(&p).unwrap();
        assert_eq!(Journal::load(&p).unwrap(), j);
    }
}
