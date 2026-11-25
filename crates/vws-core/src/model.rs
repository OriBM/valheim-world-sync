//! Shapes of `config.json` and `state.json` in the data repo (PLAN.md 4.2, 4.3)
//! and the names of save assets (4.4).
//!
//! Unknown fields are kept (`extra`) so an older app never drops data a newer
//! one wrote.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::util::{self, UnixTime};

pub const SCHEMA: u32 = 1;
pub const STATE_PATH: &str = "state.json";
pub const CONFIG_PATH: &str = "config.json";
pub const SAVES_TAG: &str = "saves";
pub const DEFAULT_SIDECAR: &str = "worlds/{world}.one_map_to_rule_them_all.explored";

/// `config.json`: settings that change rarely. Missing fields take the defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema: u32,
    pub min_app_version: String,
    pub min_game_build: String,
    pub discord_webhook_url: String,
    pub admins: Vec<String>,
    pub gale_profile_name: String,
    pub world_sidecars: Vec<String>,
    pub keep_saves: u32,
    pub keep_checkpoints: u32,
    pub heartbeat_seconds: u64,
    pub stale_after_seconds: u64,
    pub release_grace_seconds: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            schema: SCHEMA,
            min_app_version: "0.1.0".into(),
            min_game_build: "0".into(),
            discord_webhook_url: String::new(),
            admins: Vec::new(),
            gale_profile_name: "CoopWorld".into(),
            world_sidecars: vec![DEFAULT_SIDECAR.into()],
            keep_saves: 20,
            keep_checkpoints: 3,
            heartbeat_seconds: 300,
            stale_after_seconds: 900,
            release_grace_seconds: 60,
            extra: Map::new(),
        }
    }
}

impl Config {
    pub fn from_json(bytes: &[u8]) -> Result<Config> {
        Ok(serde_json::from_slice(strip_bom(bytes))?)
    }
    pub fn to_json(&self) -> Vec<u8> {
        to_pretty(self)
    }
}

/// `state.json`: the single source of truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub schema: u32,
    /// Folder name of the shared world, fixed by the first import.
    pub world_name: String,
    pub generation: u64,
    pub latest: SaveRef,
    /// Newest first; `history[0]` is `latest`.
    #[serde(default)]
    pub history: Vec<SaveRef>,
    /// `None` (JSON `null`) when nobody is hosting.
    pub lock: Option<Lock>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl State {
    pub fn from_json(bytes: &[u8]) -> Result<State> {
        let s: State = serde_json::from_slice(strip_bom(bytes))?;
        validate_world_name(&s.world_name)?;
        Ok(s)
    }
    pub fn to_json(&self) -> Vec<u8> {
        to_pretty(self)
    }
    /// True when the lock belongs to this session (the test behind I2).
    pub fn is_held_by(&self, session_id: &str) -> bool {
        self.lock.as_ref().is_some_and(|l| l.session_id == session_id)
    }
}

/// One save: `latest` and each `history` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveRef {
    pub generation: u64,
    pub asset_id: u64,
    pub asset_name: String,
    pub sha256: String,
    pub size: u64,
    pub kind: SaveKind,
    pub saved_by: String,
    /// GitHub's `created_at` for the asset. Display only.
    pub saved_at: String,
    #[serde(default)]
    pub game_build: String,
    #[serde(default)]
    pub game_version: String,
    #[serde(default)]
    pub mods_fingerprint: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveKind {
    Import,
    Session,
    Checkpoint,
    Restore,
    /// Written by a newer app version.
    #[serde(other)]
    Other,
}

impl SaveKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SaveKind::Import => "import",
            SaveKind::Session => "session",
            SaveKind::Checkpoint => "checkpoint",
            SaveKind::Restore => "restore",
            SaveKind::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lock {
    pub holder: String,
    pub client_id: String,
    pub session_id: String,
    pub base_generation: u64,
    /// GitHub's commit time of the acquiring write, filled in by the first heartbeat.
    #[serde(default)]
    pub since: Option<String>,
    pub phase: Phase,
    #[serde(default)]
    pub beat: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Ready,
    InGame,
    Finishing,
    #[serde(other)]
    Other,
}

/// Who this PC is: the display name and the client id from `settings.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub client_id: String,
}

fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b)
}

fn to_pretty<T: Serialize>(v: &T) -> Vec<u8> {
    let mut out = serde_json::to_vec_pretty(v).expect("serializing plain data cannot fail");
    out.push(b'\n');
    out
}

/// A world name must be a plain folder name (it becomes a path on every PC).
pub fn validate_world_name(name: &str) -> Result<()> {
    let bad_char = |c: char| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*');
    let upper = name.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or("");
    let reserved = matches!(stem, "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if name.is_empty()
        || name.len() > 64
        || name.starts_with(' ')
        || name.ends_with(' ')
        || name.ends_with('.')
        || name.chars().any(bad_char)
        || reserved
    {
        return Err(Error::Invalid(format!("not a usable world name: {name:?}")));
    }
    Ok(())
}

/// A display name reduced to characters GitHub keeps in asset names.
pub fn asset_safe_name(display: &str) -> String {
    let s: String = display
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s: String = s.trim_matches('_').chars().take(24).collect();
    if s.is_empty() { "player".into() } else { s }
}

/// `save-000043-9f2c.zip`
pub fn save_asset_name(generation: u64, session_id: &str) -> String {
    let short: String = session_id.chars().filter(|c| c.is_ascii_hexdigit()).take(4).collect();
    format!("save-{generation:06}-{short}.zip")
}

/// `side-000043-Tomer-20261125T194012Z.zip`
pub fn side_asset_name(base_generation: u64, display_name: &str, at: UnixTime) -> String {
    format!(
        "side-{base_generation:06}-{}-{}.zip",
        asset_safe_name(display_name),
        util::format_asset_stamp(at)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> State {
        let latest = SaveRef {
            generation: 43,
            asset_id: 123456789,
            asset_name: "save-000043-9f2c.zip".into(),
            sha256: "0f3a".into(),
            size: 1830211,
            kind: SaveKind::Session,
            saved_by: "Tomer".into(),
            saved_at: "2026-11-25T19:40:12Z".into(),
            game_build: "26870846".into(),
            game_version: "1.0.30".into(),
            mods_fingerprint: "sha256:77ab".into(),
            extra: Map::new(),
        };
        State {
            schema: 1,
            world_name: "HomeServer".into(),
            generation: 43,
            latest: latest.clone(),
            history: vec![latest],
            lock: None,
            extra: Map::new(),
        }
    }

    #[test]
    fn state_roundtrip_keeps_unknown_fields_and_writes_null_lock() {
        let mut s = sample_state();
        s.extra.insert("future_field".into(), Value::from(7));
        let json = String::from_utf8(s.to_json()).unwrap();
        assert!(json.contains("\"lock\": null"));
        let back = State::from_json(json.as_bytes()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn config_defaults_fill_missing_fields() {
        let c = Config::from_json(br#"{"discord_webhook_url":"x","world_name":"Old"}"#).unwrap();
        assert_eq!(c.keep_saves, 20);
        assert_eq!(c.world_sidecars, vec![DEFAULT_SIDECAR.to_string()]);
        assert_eq!(c.extra.get("world_name"), Some(&Value::from("Old")));
    }

    #[test]
    fn unknown_kind_and_phase_do_not_break_reading() {
        let mut v = serde_json::to_value(sample_state()).unwrap();
        v["latest"]["kind"] = Value::from("teleport");
        let s: State = serde_json::from_value(v).unwrap();
        assert_eq!(s.latest.kind, SaveKind::Other);
    }

    #[test]
    fn names() {
        assert_eq!(save_asset_name(43, "9f2c55aa-0000"), "save-000043-9f2c.zip");
        let t = util::parse_rfc3339("2026-11-25T19:40:12Z").unwrap();
        assert_eq!(
            side_asset_name(43, "Tomer", t),
            "side-000043-Tomer-20261125T194012Z.zip"
        );
        assert_eq!(asset_safe_name("◕ ·Tomer· ◕"), "Tomer");
        assert_eq!(asset_safe_name("עומר"), "player");
        assert!(validate_world_name("HomeServer").is_ok());
        for bad in ["", "..", "a/b", "a\\b", "CON", "x.", " x", "a:b"] {
            assert!(validate_world_name(bad).is_err(), "{bad}");
        }
    }
}
