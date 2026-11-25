//! The mods fingerprint (PLAN.md 6, S5): the sorted `mod@version` list of the
//! Gale profile's `BepInEx\plugins\`, hashed. Only `plugins` is read; the
//! profile's other folders (`snapshots`, `_state`, ...) are ignored.

use std::fs;
use std::path::Path;

use crate::error::{IoContext, Result};
use crate::util;

/// `Author-Name@1.2.3` for every mod folder under `<profile>\BepInEx\plugins\`
/// that has a Thunderstore `manifest.json`. Sorted, no duplicates.
pub fn list_mods(profile_dir: &Path) -> Result<Vec<String>> {
    let plugins = profile_dir.join("BepInEx").join("plugins");
    let mut out = Vec::new();
    for e in fs::read_dir(&plugins).at(&plugins)? {
        let e = e.at(&plugins)?;
        if !e.file_type().at(e.path())?.is_dir() {
            continue;
        }
        let manifest = e.path().join("manifest.json");
        let Ok(bytes) = fs::read(&manifest) else { continue };
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            continue;
        };
        let version = v.get("version_number").and_then(|x| x.as_str()).unwrap_or("?");
        let folder = e.file_name().to_string_lossy().into_owned();
        out.push(format!("{folder}@{version}"));
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// `sha256:<hex>` of the list, one entry per line.
pub fn fingerprint(mods: &[String]) -> String {
    format!("sha256:{}", util::sha256_hex(mods.join("\n").as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_plugins_only_and_tolerates_bom() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("BepInEx").join("plugins");
        fs::create_dir_all(plugins.join("Advize-PlantEverything")).unwrap();
        fs::write(
            plugins.join("Advize-PlantEverything").join("manifest.json"),
            b"\xEF\xBB\xBF{\"name\":\"PlantEverything\",\"version_number\":\"1.19.2\"}",
        )
        .unwrap();
        fs::create_dir_all(plugins.join("NoManifest")).unwrap();
        fs::create_dir_all(dir.path().join("snapshots").join("x")).unwrap();
        let mods = list_mods(dir.path()).unwrap();
        assert_eq!(mods, vec!["Advize-PlantEverything@1.19.2".to_string()]);
        assert!(fingerprint(&mods).starts_with("sha256:"));
        assert_ne!(fingerprint(&mods), fingerprint(&[]));
    }
}
