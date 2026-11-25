//! Where things live on disk (PLAN.md 2.2, 3, 10).

use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::model::validate_world_name;

/// Stands in for `IronGate\Valheim` (worlds_local, sidecars, adminlist.txt, Player.log).
pub const ENV_GAME_DATA_DIR: &str = "VWS_GAME_DATA_DIR";
/// Stands in for `%LOCALAPPDATA%\ValheimWorldSync` (settings, journal, backups, cache).
pub const ENV_DATA_DIR: &str = "VWS_DATA_DIR";

#[derive(Debug, Clone)]
pub struct Paths {
    /// `IronGate\Valheim`
    pub game_data: PathBuf,
    /// `%LOCALAPPDATA%\ValheimWorldSync`
    pub app_data: PathBuf,
}

impl Paths {
    pub fn new(game_data: impl Into<PathBuf>, app_data: impl Into<PathBuf>) -> Self {
        Paths {
            game_data: game_data.into(),
            app_data: app_data.into(),
        }
    }

    /// The real locations, unless overridden by `VWS_GAME_DATA_DIR` / `VWS_DATA_DIR`.
    /// Debug builds never default to the real game folder: they use a sandbox (section 10).
    pub fn from_env() -> Result<Self> {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let app_data = match var(ENV_DATA_DIR) {
            Some(p) => p,
            None => {
                let base = var("LOCALAPPDATA").ok_or_else(|| Error::Invalid("LOCALAPPDATA is not set".into()))?;
                if cfg!(debug_assertions) {
                    base.join("ValheimWorldSync-dev")
                } else {
                    base.join("ValheimWorldSync")
                }
            }
        };
        let game_data = match var(ENV_GAME_DATA_DIR) {
            Some(p) => p,
            None if cfg!(debug_assertions) => app_data.join("sandbox").join("IronGate").join("Valheim"),
            None => {
                let home = var("USERPROFILE").ok_or_else(|| Error::Invalid("USERPROFILE is not set".into()))?;
                home.join("AppData").join("LocalLow").join("IronGate").join("Valheim")
            }
        };
        Ok(Paths { game_data, app_data })
    }

    pub fn worlds_local(&self) -> PathBuf {
        self.game_data.join("worlds_local")
    }
    pub fn world_dir(&self, world: &str) -> PathBuf {
        self.worlds_local().join(world)
    }
    pub fn backups(&self) -> PathBuf {
        self.app_data.join("backups")
    }
    pub fn cache(&self) -> PathBuf {
        self.app_data.join("cache")
    }
    pub fn journal_file(&self) -> PathBuf {
        self.app_data.join("journal.json")
    }
    pub fn settings_file(&self) -> PathBuf {
        self.app_data.join("settings.json")
    }

    /// The sidecar files of `world` named by `config.world_sidecars`.
    pub fn sidecars(&self, world: &str, patterns: &[String]) -> Result<Vec<Sidecar>> {
        validate_world_name(world)?;
        patterns
            .iter()
            .map(|p| {
                let rel = resolve_sidecar(p, world)?;
                let path = rel.split('/').fold(self.game_data.clone(), |acc, c| acc.join(c));
                Ok(Sidecar { rel, path })
            })
            .collect()
    }
}

/// A file outside the world folder that belongs to the world (PLAN.md 2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidecar {
    /// Relative to `IronGate\Valheim`, forward slashes. Also its place under `sidecars/` in a save zip.
    pub rel: String,
    pub path: PathBuf,
}

/// Check a `world_sidecars` pattern and fill in the world name.
///
/// `config.json` can be edited by anyone holding the shared token, so a
/// pattern may only point at a world-specific file inside `IronGate\Valheim`,
/// and never into the character or world folders.
pub fn resolve_sidecar(pattern: &str, world: &str) -> Result<String> {
    let bad = |why: &str| Error::Invalid(format!("world_sidecars entry {pattern:?} is not allowed: {why}"));
    if pattern.contains('\\') || pattern.contains(':') || pattern.starts_with('/') {
        return Err(bad("use a relative path with forward slashes"));
    }
    let parts: Vec<&str> = pattern.split('/').collect();
    if parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return Err(bad("empty, '.' or '..' path parts"));
    }
    if !parts.last().is_some_and(|f| f.contains("{world}")) {
        return Err(bad("the file name must contain {world}"));
    }
    let first = parts[0].to_ascii_lowercase();
    if parts.len() > 1 && matches!(first.as_str(), "characters" | "characters_local" | "worlds_local") {
        return Err(bad("that folder is off limits"));
    }
    let rel = pattern.replace("{world}", world);
    if rel
        .chars()
        .any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*' | '{' | '}'))
    {
        return Err(bad("unsupported characters"));
    }
    Ok(rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_patterns() {
        assert_eq!(
            resolve_sidecar("worlds/{world}.one_map_to_rule_them_all.explored", "HomeServer").unwrap(),
            "worlds/HomeServer.one_map_to_rule_them_all.explored"
        );
        for bad in [
            "../{world}.x",
            "worlds/x.explored",
            "C:/{world}",
            "/abs/{world}",
            "worlds\\{world}",
            "characters/{world}.fch",
            "worlds_local/{world}/evil",
            "a//{world}",
        ] {
            assert!(resolve_sidecar(bad, "HomeServer").is_err(), "{bad}");
        }
    }
}
