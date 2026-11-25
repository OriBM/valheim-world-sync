//! Save zips (PLAN.md 4.4): `manifest.json` at the root, the world under
//! `world/<World>/`, sidecars under `sidecars/<path>`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::error::{Error, IoContext, Result};
use crate::model::{SCHEMA, validate_world_name};
use crate::paths::{Paths, Sidecar};
use crate::util;
use crate::world::{self, FileEntry, SIDECAR_PREFIX, world_prefix};

const MANIFEST: &str = "manifest.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZipManifest {
    pub schema: u32,
    pub world_name: String,
    /// Set for `save-*` assets.
    #[serde(default)]
    pub generation: Option<u64>,
    /// Set for side copies: the save the session started from.
    #[serde(default)]
    pub base_generation: Option<u64>,
    /// `import`, `session`, `checkpoint`, `restore` or `side`.
    pub kind: String,
    pub saved_by: String,
    #[serde(default)]
    pub game_build: String,
    #[serde(default)]
    pub game_version: String,
    #[serde(default)]
    pub mods_fingerprint: String,
    /// This PC's clock; information only.
    #[serde(default)]
    pub created_at: String,
    pub files: Vec<FileEntry>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Facts about the PC and game that go into every save.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveMeta {
    pub saved_by: String,
    pub game_build: String,
    pub game_version: String,
    pub mods_fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct Packed {
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
    pub manifest: ZipManifest,
}

#[derive(Debug, Clone, Copy)]
pub struct PackKind<'a> {
    pub kind: &'a str,
    pub generation: Option<u64>,
    pub base_generation: Option<u64>,
}

fn zip_err(e: zip::result::ZipError) -> Error {
    Error::Invalid(format!("zip: {e}"))
}

/// Zip the world (folder plus sidecars) into `out`. Fails if any file changes
/// while it is being zipped.
pub fn pack(
    paths: &Paths,
    world: &str,
    sidecars: &[Sidecar],
    what: PackKind<'_>,
    meta: &SaveMeta,
    out: &Path,
) -> Result<Packed> {
    validate_world_name(world)?;
    let files = world::snapshot(paths, world, sidecars)?;
    let wp = world_prefix(world);
    if !files.iter().any(|f| f.path.starts_with(&wp)) {
        return Err(Error::Refused(format!("there is no world folder for {world} to save")));
    }
    let manifest = ZipManifest {
        schema: SCHEMA,
        world_name: world.to_string(),
        generation: what.generation,
        base_generation: what.base_generation,
        kind: what.kind.to_string(),
        saved_by: meta.saved_by.clone(),
        game_build: meta.game_build.clone(),
        game_version: meta.game_version.clone(),
        mods_fingerprint: meta.mods_fingerprint.clone(),
        created_at: util::format_rfc3339(util::now_unix()),
        files,
        extra: Map::new(),
    };
    let dir = out
        .parent()
        .ok_or_else(|| Error::Invalid("no folder for the zip".into()))?;
    fs::create_dir_all(dir).at(dir)?;
    let mut tmp_name = out.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".partial");
    let tmp = dir.join(tmp_name);
    {
        let f = File::create(&tmp).at(&tmp)?;
        let mut zw = ZipWriter::new(BufWriter::new(f));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zw.start_file(MANIFEST, opts).map_err(zip_err)?;
        zw.write_all(&serde_json::to_vec_pretty(&manifest)?).at(&tmp)?;
        let mut buf = vec![0u8; 256 * 1024];
        for fe in &manifest.files {
            let src = source_path(paths, world, sidecars, &fe.path)?;
            zw.start_file(fe.path.as_str(), opts).map_err(zip_err)?;
            let mut f = File::open(&src).at(&src)?;
            let mut h = Sha256::new();
            let mut n = 0u64;
            loop {
                let k = f.read(&mut buf).at(&src)?;
                if k == 0 {
                    break;
                }
                h.update(&buf[..k]);
                zw.write_all(&buf[..k]).at(&tmp)?;
                n += k as u64;
            }
            if n != fe.size || util::hex(&h.finalize()) != fe.sha256 {
                drop(zw);
                let _ = fs::remove_file(&tmp);
                return Err(Error::Refused(format!(
                    "{} changed while it was being saved; try again",
                    fe.path
                )));
            }
        }
        let w = zw.finish().map_err(zip_err)?;
        let f = w.into_inner().map_err(|e| Error::io(&tmp, e.into_error()))?;
        f.sync_all().at(&tmp)?;
    }
    if out.exists() {
        fs::remove_file(out).at(out)?; // an old file of ours in cache\
    }
    fs::rename(&tmp, out).at(out)?;
    let (sha256, size) = util::sha256_file(out)?;
    Ok(Packed {
        path: out.to_path_buf(),
        sha256,
        size,
        manifest,
    })
}

fn source_path(paths: &Paths, world: &str, sidecars: &[Sidecar], zip_path: &str) -> Result<PathBuf> {
    if let Some(rest) = zip_path.strip_prefix(&world_prefix(world)) {
        return Ok(rest.split('/').fold(paths.world_dir(world), |a, c| a.join(c)));
    }
    if let Some(rest) = zip_path.strip_prefix(SIDECAR_PREFIX)
        && let Some(s) = sidecars.iter().find(|s| s.rel == rest)
    {
        return Ok(s.path.clone());
    }
    Err(Error::Invalid(format!("unexpected file in snapshot: {zip_path}")))
}

/// Read only the manifest of a save zip.
pub fn read_manifest(zip_path: &Path) -> Result<ZipManifest> {
    let f = File::open(zip_path).at(zip_path)?;
    let mut za = ZipArchive::new(BufReader::new(f)).map_err(zip_err)?;
    manifest_of(&mut za)
}

fn manifest_of<R: Read + io::Seek>(za: &mut ZipArchive<R>) -> Result<ZipManifest> {
    let mut mf = za
        .by_name(MANIFEST)
        .map_err(|_| Error::Verify("the save has no manifest.json".into()))?;
    let mut bytes = Vec::new();
    (&mut mf)
        .take(4 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::Verify(format!("manifest: {e}")))?;
    let m: ZipManifest = serde_json::from_slice(&bytes).map_err(|e| Error::Verify(format!("manifest: {e}")))?;
    validate_world_name(&m.world_name)?;
    Ok(m)
}

#[derive(Debug, Clone)]
pub struct Extracted {
    pub manifest: ZipManifest,
    /// Sidecars in the save that the current config does not list; checked but not extracted.
    pub skipped: Vec<String>,
}

/// Unpack a save into `dest` (which must not exist yet), checking every file
/// against the manifest (S8). Only `world/<World>/...` and the configured
/// sidecars are written; anything else in the zip fails the check.
pub fn extract_verified(
    zip_path: &Path,
    dest: &Path,
    expected_world: Option<&str>,
    sidecars: &[Sidecar],
) -> Result<Extracted> {
    let f = File::open(zip_path).at(zip_path)?;
    let mut za = ZipArchive::new(BufReader::new(f)).map_err(zip_err)?;
    let manifest = manifest_of(&mut za)?;
    if let Some(w) = expected_world
        && w != manifest.world_name
    {
        return Err(Error::Verify(format!(
            "the save holds the world {:?}, expected {w:?}",
            manifest.world_name
        )));
    }
    let wp = world_prefix(&manifest.world_name);
    let mut expected: BTreeMap<&str, &FileEntry> = BTreeMap::new();
    for fe in &manifest.files {
        check_entry_path(&fe.path, &wp)?;
        if expected.insert(fe.path.as_str(), fe).is_some() {
            return Err(Error::Verify(format!("the manifest lists {} twice", fe.path)));
        }
    }
    if !expected.keys().any(|p| p.starts_with(&wp)) {
        return Err(Error::Verify("the save contains no world files".into()));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).at(parent)?;
    }
    fs::create_dir(dest).at(dest)?;

    let mut seen = BTreeSet::new();
    let mut skipped = Vec::new();
    let mut buf = vec![0u8; 256 * 1024];
    for i in 0..za.len() {
        let mut zf = za.by_index(i).map_err(zip_err)?;
        let name = zf.name().map_err(zip_err)?.to_string();
        if zf.is_dir() || name == MANIFEST {
            continue;
        }
        let Some(fe) = expected.get(name.as_str()) else {
            return Err(Error::Verify(format!(
                "the save contains a file its manifest does not list: {name}"
            )));
        };
        if !seen.insert(name.clone()) {
            return Err(Error::Verify(format!("the save contains {name} twice")));
        }
        let write_it = match name.strip_prefix(SIDECAR_PREFIX) {
            Some(rest) => sidecars.iter().any(|s| s.rel == rest),
            None => true,
        };
        let mut out = if write_it {
            let out_path = name.split('/').fold(dest.to_path_buf(), |a, c| a.join(c));
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent).at(parent)?;
            }
            Some((File::create_new(&out_path).at(&out_path)?, out_path))
        } else {
            skipped.push(name.clone());
            None
        };
        let mut h = Sha256::new();
        let mut n = 0u64;
        let mut limited = (&mut zf).take(fe.size + 1);
        loop {
            let k = limited
                .read(&mut buf)
                .map_err(|e| Error::Verify(format!("{name}: {e}")))?;
            if k == 0 {
                break;
            }
            h.update(&buf[..k]);
            n += k as u64;
            if let Some((f, p)) = out.as_mut() {
                f.write_all(&buf[..k]).at(&*p)?;
            }
        }
        if n != fe.size || util::hex(&h.finalize()) != fe.sha256 {
            return Err(Error::Verify(format!("{name} does not match the manifest")));
        }
        if let Some((f, p)) = out {
            f.sync_all().at(&p)?;
        }
    }
    if seen.len() != expected.len() {
        let missing: Vec<&str> = expected.keys().filter(|k| !seen.contains(**k)).copied().collect();
        return Err(Error::Verify(format!("the save is missing {}", missing.join(", "))));
    }
    Ok(Extracted { manifest, skipped })
}

/// A manifest path must stay under `world/<World>/` or `sidecars/`, with no
/// tricks (absolute paths, `..`, drive letters, backslashes).
fn check_entry_path(p: &str, world_prefix: &str) -> Result<()> {
    let bad = || Error::Verify(format!("the save names a file in a place it may not write: {p:?}"));
    if p.is_empty() || p.starts_with('/') || p.contains('\\') || p.contains(':') || p.chars().any(|c| c.is_control()) {
        return Err(bad());
    }
    if p.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return Err(bad());
    }
    let inside = |prefix: &str| p.strip_prefix(prefix).is_some_and(|rest| !rest.is_empty());
    if inside(world_prefix) || inside(SIDECAR_PREFIX) {
        Ok(())
    } else {
        Err(bad())
    }
}
