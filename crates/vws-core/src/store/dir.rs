//! A stand-in for the GitHub data repo, kept in a local folder.
//!
//! Same rules as GitHub where the protocol depends on them: compare-and-swap
//! on file content (stale SHA = conflict, create-when-exists = conflict),
//! commit times on the server's clock, release assets with digests, name
//! clashes rejected. Safe across threads and across processes, so two CLI
//! instances can share one folder to play two friends offline.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::{Asset, CommitInfo, Release, RemoteFile, Store, WriteOutcome, check_asset_name, check_remote_path};
use crate::error::{Error, IoContext, Result};
use crate::util::{self, UnixTime};

/// The server's clock.
pub trait Clock: Send + Sync {
    fn now(&self) -> UnixTime;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UnixTime {
        util::now_unix()
    }
}

/// A clock tests can move by hand.
pub struct ManualClock(AtomicI64);

impl ManualClock {
    pub fn new(t: UnixTime) -> Arc<Self> {
        Arc::new(ManualClock(AtomicI64::new(t)))
    }
    pub fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> UnixTime {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct DirStore {
    root: PathBuf,
    clock: Arc<dyn Clock>,
    in_process: Mutex<()>,
}

#[derive(Serialize, Deserialize)]
struct FileMeta {
    sha: String,
    commit_time: UnixTime,
}

#[derive(Serialize, Deserialize, Clone)]
struct ReleaseRec {
    id: u64,
    tag: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct AssetRec {
    id: u64,
    release_id: u64,
    name: String,
    size: u64,
    state: String,
    created_at: String,
    digest: String,
}

impl AssetRec {
    fn to_asset(&self) -> Asset {
        Asset {
            id: self.id,
            name: self.name.clone(),
            size: self.size,
            state: self.state.clone(),
            created_at: self.created_at.clone(),
            digest: Some(self.digest.clone()),
        }
    }
}

impl DirStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<DirStore> {
        Self::with_clock(root, Arc::new(SystemClock))
    }

    pub fn with_clock(root: impl Into<PathBuf>, clock: Arc<dyn Clock>) -> Result<DirStore> {
        let root = root.into();
        for d in ["files", "meta", "assets"] {
            let p = root.join(d);
            fs::create_dir_all(&p).at(&p)?;
        }
        Ok(DirStore {
            root,
            clock,
            in_process: Mutex::new(()),
        })
    }

    /// Hold both locks for the duration of one operation.
    fn locked<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self.in_process.lock().unwrap_or_else(|p| p.into_inner());
        let lock_path = self.root.join(".lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .at(&lock_path)?;
        lock_file.lock().at(&lock_path)?;
        let out = f();
        let _ = lock_file.unlock();
        out
    }

    fn content_path(&self, path: &str) -> Result<PathBuf> {
        check_remote_path(path)?;
        Ok(path.split('/').fold(self.root.join("files"), |a, c| a.join(c)))
    }

    fn meta_path(&self, path: &str) -> PathBuf {
        self.root.join("meta").join(format!("{}.json", path.replace('/', "__")))
    }

    fn read_meta(&self, path: &str) -> Result<Option<FileMeta>> {
        read_json_opt(&self.meta_path(path))
    }

    fn releases(&self) -> Result<Vec<ReleaseRec>> {
        Ok(read_json_opt(&self.root.join("releases.json"))?.unwrap_or_default())
    }

    fn asset_rec(&self, id: u64) -> Result<AssetRec> {
        read_json_opt(&self.root.join("assets").join(format!("{id}.json")))?
            .ok_or_else(|| Error::NotFound(format!("asset {id}")))
    }

    fn all_assets(&self) -> Result<Vec<AssetRec>> {
        let dir = self.root.join("assets");
        let mut out = Vec::new();
        for e in fs::read_dir(&dir).at(&dir)? {
            let p = e.at(&dir)?.path();
            if p.extension().is_some_and(|x| x == "json")
                && let Some(rec) = read_json_opt::<AssetRec>(&p)?
            {
                out.push(rec);
            }
        }
        out.sort_by_key(|a| a.id);
        Ok(out)
    }

    fn save_asset_rec(&self, rec: &AssetRec) -> Result<()> {
        util::write_atomic(
            &self.root.join("assets").join(format!("{}.json", rec.id)),
            &serde_json::to_vec_pretty(rec)?,
        )
    }

    fn next_id(&self) -> Result<u64> {
        let p = self.root.join("next_id");
        let id = match fs::read_to_string(&p) {
            Ok(s) => s.trim().parse::<u64>().unwrap_or(1),
            Err(_) => 1,
        };
        util::write_atomic(&p, (id + 1).to_string().as_bytes())?;
        Ok(id)
    }
}

fn read_json_opt<T: for<'de> Deserialize<'de>>(p: &Path) -> Result<Option<T>> {
    match fs::read(p) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(p, e)),
    }
}

fn http(status: u16, message: &str) -> Error {
    Error::Http {
        status,
        message: message.into(),
    }
}

impl Store for DirStore {
    fn read_file(&self, path: &str) -> Result<Option<RemoteFile>> {
        let cp = self.content_path(path)?;
        self.locked(|| {
            let Some(meta) = self.read_meta(path)? else {
                return Ok(None);
            };
            let content = fs::read(&cp).at(&cp)?;
            Ok(Some(RemoteFile { content, sha: meta.sha }))
        })
    }

    fn write_file(
        &self,
        path: &str,
        content: &[u8],
        expected_sha: Option<&str>,
        _message: &str,
    ) -> Result<WriteOutcome> {
        let cp = self.content_path(path)?;
        self.locked(|| {
            let current = self.read_meta(path)?;
            match (expected_sha, &current) {
                (None, Some(_)) => return Err(Error::Conflict),
                (Some(_), None) => return Err(Error::Conflict),
                (Some(e), Some(c)) if e != c.sha => return Err(Error::Conflict),
                _ => {}
            }
            util::write_atomic(&cp, content)?;
            let meta = FileMeta {
                sha: util::sha256_hex(content),
                commit_time: self.clock.now(),
            };
            util::write_atomic(&self.meta_path(path), &serde_json::to_vec(&meta)?)?;
            Ok(WriteOutcome {
                sha: meta.sha,
                commit_time: Some(meta.commit_time),
            })
        })
    }

    fn delete_file(&self, path: &str, sha: &str, _message: &str) -> Result<()> {
        let cp = self.content_path(path)?;
        self.locked(|| {
            match self.read_meta(path)? {
                None => return Err(Error::NotFound(path.into())),
                Some(m) if m.sha != sha => return Err(Error::Conflict),
                Some(_) => {}
            }
            fs::remove_file(&cp).at(&cp)?;
            let mp = self.meta_path(path);
            fs::remove_file(&mp).at(&mp)
        })
    }

    fn last_commit(&self, path: &str) -> Result<Option<CommitInfo>> {
        check_remote_path(path)?;
        self.locked(|| {
            Ok(self.read_meta(path)?.map(|m| CommitInfo {
                commit_time: m.commit_time,
                server_time: self.clock.now(),
            }))
        })
    }

    fn find_release(&self, tag: &str) -> Result<Option<Release>> {
        self.locked(|| {
            Ok(self.releases()?.into_iter().find(|r| r.tag == tag).map(|r| Release {
                id: r.id,
                tag: r.tag,
                upload_url: String::new(),
            }))
        })
    }

    fn create_release(&self, tag: &str) -> Result<Release> {
        self.locked(|| {
            let mut all = self.releases()?;
            if all.iter().any(|r| r.tag == tag) {
                return Err(http(422, "Validation Failed: tag already_exists"));
            }
            let rec = ReleaseRec {
                id: self.next_id()?,
                tag: tag.into(),
            };
            all.push(rec.clone());
            util::write_atomic(&self.root.join("releases.json"), &serde_json::to_vec_pretty(&all)?)?;
            Ok(Release {
                id: rec.id,
                tag: rec.tag,
                upload_url: String::new(),
            })
        })
    }

    fn list_assets(&self, release: &Release) -> Result<Vec<Asset>> {
        self.locked(|| {
            Ok(self
                .all_assets()?
                .iter()
                .filter(|a| a.release_id == release.id)
                .map(AssetRec::to_asset)
                .collect())
        })
    }

    fn upload_asset(&self, release: &Release, name: &str, data: &[u8]) -> Result<Asset> {
        check_asset_name(name)?;
        self.locked(|| {
            if self
                .all_assets()?
                .iter()
                .any(|a| a.release_id == release.id && a.name == name)
            {
                return Err(http(422, "Validation Failed: asset already_exists"));
            }
            let id = self.next_id()?;
            let bin = self.root.join("assets").join(format!("{id}.bin"));
            util::write_atomic(&bin, data)?;
            let rec = AssetRec {
                id,
                release_id: release.id,
                name: name.into(),
                size: data.len() as u64,
                state: "uploaded".into(),
                created_at: util::format_rfc3339(self.clock.now()),
                digest: format!("sha256:{}", util::sha256_hex(data)),
            };
            self.save_asset_rec(&rec)?;
            Ok(rec.to_asset())
        })
    }

    fn download_asset(&self, asset_id: u64) -> Result<Vec<u8>> {
        self.locked(|| {
            self.asset_rec(asset_id)?;
            let bin = self.root.join("assets").join(format!("{asset_id}.bin"));
            fs::read(&bin).at(&bin)
        })
    }

    fn rename_asset(&self, asset_id: u64, new_name: &str) -> Result<Asset> {
        check_asset_name(new_name)?;
        self.locked(|| {
            let mut rec = self.asset_rec(asset_id)?;
            if self
                .all_assets()?
                .iter()
                .any(|a| a.release_id == rec.release_id && a.name == new_name && a.id != asset_id)
            {
                return Err(http(422, "Validation Failed: asset already_exists"));
            }
            rec.name = new_name.into();
            self.save_asset_rec(&rec)?;
            Ok(rec.to_asset())
        })
    }

    fn delete_asset(&self, asset_id: u64) -> Result<()> {
        self.locked(|| {
            self.asset_rec(asset_id)?;
            for ext in ["bin", "json"] {
                let p = self.root.join("assets").join(format!("{asset_id}.{ext}"));
                if p.exists() {
                    fs::remove_file(&p).at(&p)?;
                }
            }
            Ok(())
        })
    }
}

impl std::fmt::Debug for DirStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirStore").field("root", &self.root).finish()
    }
}
