//! The world on disk (PLAN.md 2.1, 5.1, S7): is it here, is it complete, what
//! exactly is in it, moving it out to `backups\` and putting a new one in place.
//!
//! The world folder is an opaque unit: it is listed, hashed and moved as a
//! whole. File names are only read to check completeness.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};
use crate::paths::{Paths, Sidecar};
use crate::util;

/// One file of a world, named by its place in a save zip:
/// `world/<World>/<file>` or `sidecars/<path relative to IronGate\Valheim>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

pub const SIDECAR_PREFIX: &str = "sidecars/";

pub fn world_prefix(world: &str) -> String {
    format!("world/{world}/")
}

/// True when `dir` holds a `_main.*` file: a real world, not a cloud world's
/// minimap cache (2.1). This is what "the world is on this PC" means (I5).
pub fn world_present(dir: &Path) -> bool {
    let Ok(rd) = fs::read_dir(dir) else { return false };
    rd.flatten().any(|e| {
        e.file_name().to_string_lossy().starts_with("_main.") && e.file_type().map(|t| t.is_file()).unwrap_or(false)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completeness {
    /// The `N` of the newest complete `_main.N` set.
    pub save_number: u64,
    /// Bytes in `_main.*` and `*.chunk` files: the size S7 compares.
    pub data_bytes: u64,
}

/// S7, first half: the highest `N` has all four of `_main.N.chunks`, `.db2`, `.fwl2`, `.ok`.
pub fn check_complete(dir: &Path) -> Result<Completeness> {
    let mut mains: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
    let mut data_bytes = 0u64;
    for e in fs::read_dir(dir).at(dir)? {
        let e = e.at(dir)?;
        if !e.file_type().at(e.path())?.is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
        let len = e.metadata().at(e.path())?.len();
        if let Some(rest) = name.strip_prefix("_main.") {
            data_bytes += len;
            if let Some((n, ext)) = rest.split_once('.')
                && let Ok(n) = n.parse::<u64>()
            {
                mains.entry(n).or_default().insert(ext.to_string());
            }
        } else if name.ends_with(".chunk") {
            data_bytes += len;
        }
    }
    let Some((&n, exts)) = mains.iter().next_back() else {
        return Err(Error::Refused("the world folder has no _main save files".into()));
    };
    let missing: Vec<String> = ["chunks", "db2", "fwl2", "ok"]
        .into_iter()
        .filter(|x| !exts.contains(*x))
        .map(|x| format!("_main.{n}.{x}"))
        .collect();
    if !missing.is_empty() {
        return Err(Error::Refused(format!(
            "the newest save (#{n}) is incomplete; missing {}",
            missing.join(", ")
        )));
    }
    Ok(Completeness {
        save_number: n,
        data_bytes,
    })
}

/// Everything that makes up the world on this PC: the folder plus the sidecars
/// that exist, each hashed. Sorted by path, so two snapshots compare directly.
pub fn snapshot(paths: &Paths, world: &str, sidecars: &[Sidecar]) -> Result<Vec<FileEntry>> {
    let mut out = Vec::new();
    let dir = paths.world_dir(world);
    if dir.is_dir() {
        collect(&dir, &world_prefix(world), &mut out)?;
    }
    for s in sidecars {
        if s.path.is_file() {
            let (sha256, size) = util::sha256_file(&s.path)?;
            out.push(FileEntry {
                path: format!("{SIDECAR_PREFIX}{}", s.rel),
                size,
                sha256,
            });
        }
    }
    out.sort();
    Ok(out)
}

fn collect(dir: &Path, prefix: &str, out: &mut Vec<FileEntry>) -> Result<()> {
    let mut entries = fs::read_dir(dir).at(dir)?.collect::<io::Result<Vec<_>>>().at(dir)?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e
            .file_name()
            .into_string()
            .map_err(|_| Error::Invalid(format!("a file name in {} is not valid Unicode", dir.display())))?;
        let ft = e.file_type().at(e.path())?;
        if ft.is_dir() {
            collect(&e.path(), &format!("{prefix}{name}/"), out)?;
        } else if ft.is_file() {
            let (sha256, size) = util::sha256_file(&e.path())?;
            out.push(FileEntry {
                path: format!("{prefix}{name}"),
                size,
                sha256,
            });
        }
        // Links and other special files are never followed or copied.
    }
    Ok(())
}

/// What [`move_world_out`] moved, and where to.
#[derive(Debug, Clone)]
pub struct MovedOut {
    pub backup_dir: PathBuf,
    pub items: Vec<String>,
}

/// "Moving the world out" (PLAN.md 5.1): the world folder, every
/// `<World>_backup*` sibling and the sidecars go into one new folder under
/// `backups\`. Nothing is overwritten or deleted (I4). Returns `None` when
/// there was nothing to move.
pub fn move_world_out(paths: &Paths, world: &str, sidecars: &[Sidecar], reason: &str) -> Result<Option<MovedOut>> {
    let mut sources: Vec<(PathBuf, PathBuf, String)> = Vec::new();
    let dir = paths.world_dir(world);
    if dir.exists() {
        sources.push((
            dir,
            Path::new("worlds_local").join(world),
            format!("worlds_local/{world}"),
        ));
    }
    let sibling_prefix = format!("{world}_backup");
    if let Ok(rd) = fs::read_dir(paths.worlds_local()) {
        let mut siblings: Vec<_> = rd
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().into_string().ok()?;
                n.starts_with(&sibling_prefix).then_some((e.path(), n))
            })
            .collect();
        siblings.sort();
        for (p, n) in siblings {
            sources.push((p, Path::new("worlds_local").join(&n), format!("worlds_local/{n}")));
        }
    }
    for s in sidecars {
        if s.path.exists() {
            let rel = s.rel.split('/').fold(PathBuf::from("sidecars"), |a, c| a.join(c));
            sources.push((s.path.clone(), rel, format!("sidecars/{}", s.rel)));
        }
    }
    if sources.is_empty() {
        return Ok(None);
    }
    let backup_dir = new_backup_dir(paths, reason)?;
    let mut items = Vec::new();
    for (src, rel, label) in sources {
        let dst = backup_dir.join(rel);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).at(parent)?;
        }
        move_path(&src, &dst)?;
        items.push(label);
    }
    let info = serde_json::json!({
        "reason": reason,
        "world": world,
        "moved_at": util::format_rfc3339(util::now_unix()),
        "items": items,
    });
    util::write_atomic(&backup_dir.join("info.json"), &serde_json::to_vec_pretty(&info)?)?;
    Ok(Some(MovedOut { backup_dir, items }))
}

fn new_backup_dir(paths: &Paths, reason: &str) -> Result<PathBuf> {
    let root = paths.backups();
    fs::create_dir_all(&root).at(&root)?;
    let reason: String = reason
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let base = format!("{}-{reason}", util::format_compact(util::now_unix()));
    for i in 1..1000 {
        let name = if i == 1 { base.clone() } else { format!("{base}-{i}") };
        let p = root.join(name);
        match fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::io(&p, e)),
        }
    }
    Err(Error::Refused("too many backups with the same time stamp".into()))
}

/// Keep the newest `keep` folders in `backups\` and delete the rest (PLAN.md 3).
pub fn prune_backups(paths: &Paths, keep: usize) -> Result<Vec<PathBuf>> {
    let root = paths.backups();
    let Ok(rd) = fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    dirs.sort(); // names start with a UTC time stamp
    let excess = dirs.len().saturating_sub(keep);
    let removed: Vec<PathBuf> = dirs.drain(..excess).collect();
    for d in &removed {
        fs::remove_dir_all(d).at(d)?;
    }
    Ok(removed)
}

/// Put a world extracted by [`crate::archive::extract_verified`] into place
/// (5.3 step 4). Whatever is there is moved out first (I4).
pub fn install_extracted(
    paths: &Paths,
    world: &str,
    sidecars: &[Sidecar],
    staging: &Path,
    reason: &str,
) -> Result<Option<MovedOut>> {
    let src_world = staging.join("world").join(world);
    if !src_world.is_dir() {
        return Err(Error::Verify(format!("the save does not contain the world {world}")));
    }
    let moved = move_world_out(paths, world, sidecars, reason)?;
    let wl = paths.worlds_local();
    fs::create_dir_all(&wl).at(&wl)?;
    move_path(&src_world, &paths.world_dir(world))?;
    for s in sidecars {
        let src = s.rel.split('/').fold(staging.join("sidecars"), |a, c| a.join(c));
        if src.is_file() {
            if let Some(parent) = s.path.parent() {
                fs::create_dir_all(parent).at(parent)?;
            }
            move_path(&src, &s.path)?;
        }
    }
    Ok(moved)
}

/// Rename, or copy-verify-delete when the two places are on different drives.
/// Refuses to overwrite anything.
pub fn move_path(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        return Err(Error::Refused(format!(
            "{} already exists; not overwriting it",
            dst.display()
        )));
    }
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices || e.raw_os_error() == Some(17) => {
            copy_tree(src, dst)?;
            if tree_listing(src)? != tree_listing(dst)? {
                return Err(Error::Verify(format!(
                    "copy of {} does not match; original kept",
                    src.display()
                )));
            }
            if src.is_dir() {
                fs::remove_dir_all(src).at(src)
            } else {
                fs::remove_file(src).at(src)
            }
        }
        Err(e) => Err(Error::io(src, e)),
    }
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst).at(dst)?;
        for e in fs::read_dir(src).at(src)? {
            let e = e.at(src)?;
            let ft = e.file_type().at(e.path())?;
            if ft.is_dir() || ft.is_file() {
                copy_tree(&e.path(), &dst.join(e.file_name()))?;
            }
        }
        Ok(())
    } else {
        fs::copy(src, dst).at(dst).map(|_| ())
    }
}

fn tree_listing(root: &Path) -> Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    if root.is_file() {
        out.push((PathBuf::new(), util::sha256_file(root)?.0));
        return Ok(out);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).at(&d)? {
            let e = e.at(&d)?;
            let ft = e.file_type().at(e.path())?;
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                let rel = e.path().strip_prefix(root).unwrap_or(&e.path()).to_path_buf();
                out.push((rel, util::sha256_file(&e.path())?.0));
            }
        }
    }
    out.sort();
    Ok(out)
}
