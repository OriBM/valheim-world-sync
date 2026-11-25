//! `settings.json` on this PC (PLAN.md 3). The token is not stored here.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Error, IoContext, Result};
use crate::model::Identity;
use crate::util;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub display_name: String,
    pub client_id: String,
    #[serde(default)]
    pub gale_profile_dir: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Settings {
    pub fn new(display_name: &str) -> Result<Settings> {
        let name = display_name.trim();
        if name.is_empty() || name.chars().count() > 32 || name.chars().any(|c| c.is_control()) {
            return Err(Error::Invalid("pick a display name of 1 to 32 characters".into()));
        }
        Ok(Settings {
            display_name: name.to_string(),
            client_id: uuid::Uuid::new_v4().to_string(),
            gale_profile_dir: None,
            extra: Map::new(),
        })
    }

    pub fn load(path: &Path) -> Result<Option<Settings>> {
        match std::fs::read(path) {
            Ok(b) => Ok(Some(serde_json::from_slice(&b)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).at(path),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        util::write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn identity(&self) -> Identity {
        Identity {
            name: self.display_name.clone(),
            client_id: self.client_id.clone(),
        }
    }
}
