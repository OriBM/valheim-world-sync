//! The hosting protocol (PLAN.md 5). Every function talks to a [`Store`] and
//! keeps the invariants:
//!
//! - I1: `state.json` changes only by compare-and-swap. A conflict means: read
//!   again and decide again, never resend the same body.
//! - I2: `latest` changes only in a write whose freshly read state shows this
//!   session holding the lock.
//! - I3: a save asset is uploaded and its hash verified before any state write
//!   points at it.
//! - I6: staleness is judged on GitHub's clock only.
//! - I7: once a session learns it lost the lock, it writes neither `lock` nor
//!   `latest` again. It can still store side copies.
//!
//! Functions here do not touch the journal; the caller saves it between steps
//! (see [`publish_save`] for the one place where that matters mid-call).

// Results carrying a whole `State` are returned once per call; boxing them
// would only make the API clumsier.
#![allow(clippy::large_enum_variant)]

use std::fs;

use serde_json::Map;

use crate::archive::{self, PackKind, SaveMeta};
use crate::error::{Error, IoContext, Result};
use crate::journal::{Journal, JournalPhase, PendingUpload, Session};
use crate::model::{
    CONFIG_PATH, Config, Identity, Lock, Phase, SAVES_TAG, SCHEMA, STATE_PATH, SaveKind, SaveRef, State,
    save_asset_name, side_asset_name, validate_world_name,
};
use crate::paths::Paths;
use crate::store::{Asset, Release, Store, WriteOutcome};
use crate::util;
use crate::world::{self, FileEntry, MovedOut};

/// How often a compare-and-swap is retried after conflicts before giving up.
const MAX_CAS_ATTEMPTS: usize = 8;

/// A remote file's parsed content with the SHA it was read at.
#[derive(Debug, Clone, PartialEq)]
pub struct Versioned<T> {
    pub value: T,
    pub sha: String,
}

pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// `None` when no world has been shared yet.
pub fn read_state(store: &dyn Store) -> Result<Option<Versioned<State>>> {
    Ok(match store.read_file(STATE_PATH)? {
        None => None,
        Some(f) => Some(Versioned {
            value: State::from_json(&f.content)?,
            sha: f.sha,
        }),
    })
}

/// `config.json`, or the defaults when it does not exist yet.
pub fn read_config(store: &dyn Store) -> Result<Config> {
    Ok(match store.read_file(CONFIG_PATH)? {
        None => Config::default(),
        Some(f) => Config::from_json(&f.content)?,
    })
}

fn write_state(store: &dyn Store, state: &State, sha: Option<&str>, message: &str) -> Result<WriteOutcome> {
    store.write_file(STATE_PATH, &state.to_json(), sha, message)
}

#[derive(Debug, Clone, PartialEq)]
pub enum LockView {
    Free,
    Held {
        lock: Lock,
        /// Seconds since the last write to `state.json`, on GitHub's clock.
        silent_for: i64,
        stale: bool,
    },
}

/// Who holds the lock and whether it is stale (5.2), judged on GitHub's clock (I6).
pub fn lock_view(store: &dyn Store, state: &State, cfg: &Config) -> Result<LockView> {
    let Some(lock) = state.lock.clone() else {
        return Ok(LockView::Free);
    };
    let info = store
        .last_commit(STATE_PATH)?
        .ok_or_else(|| Error::Invalid("state.json has no commits".into()))?;
    let silent_for = (info.server_time - info.commit_time).max(0);
    Ok(LockView::Held {
        lock,
        silent_for,
        stale: silent_for > cfg.stale_after_seconds as i64,
    })
}

fn new_lock(me: &Identity, session_id: &str, base_generation: u64) -> Lock {
    Lock {
        holder: me.name.clone(),
        client_id: me.client_id.clone(),
        session_id: session_id.to_string(),
        base_generation,
        since: None,
        phase: Phase::Preparing,
        beat: 0,
        extra: Map::new(),
    }
}

fn session_for(session_id: &str, base_generation: u64, acquired_at: Option<i64>) -> Session {
    Session {
        session_id: session_id.to_string(),
        base_generation,
        acquired_at: acquired_at.map(util::format_rfc3339),
        since_written: false,
        lost: false,
    }
}

#[derive(Debug)]
pub enum Acquired {
    Ok { state: Versioned<State>, session: Session },
    HeldBy(Lock),
    NoWorld,
}

/// Take the lock (5.3 step 2). `session_id` comes from [`new_session_id`];
/// keep it in the journal before calling, so a retry after a lost response
/// recognises its own lock.
pub fn acquire(store: &dyn Store, me: &Identity, session_id: &str) -> Result<Acquired> {
    for _ in 0..MAX_CAS_ATTEMPTS {
        let Some(cur) = read_state(store)? else {
            return Ok(Acquired::NoWorld);
        };
        match &cur.value.lock {
            Some(l) if l.session_id == session_id => {
                // An earlier attempt of ours landed; only its answer was lost.
                let at = store.last_commit(STATE_PATH)?.map(|c| c.commit_time);
                let session = session_for(session_id, l.base_generation, at);
                return Ok(Acquired::Ok { state: cur, session });
            }
            Some(l) => return Ok(Acquired::HeldBy(l.clone())),
            None => {}
        }
        let mut next = cur.value.clone();
        next.lock = Some(new_lock(me, session_id, next.generation));
        match write_state(store, &next, Some(&cur.sha), &format!("{} starts hosting", me.name)) {
            Ok(w) => {
                let session = session_for(session_id, next.generation, w.commit_time);
                return Ok(Acquired::Ok {
                    state: Versioned {
                        value: next,
                        sha: w.sha,
                    },
                    session,
                });
            }
            Err(Error::Conflict) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(Error::Conflict)
}

#[derive(Debug)]
pub enum Beat {
    Ok(Versioned<State>),
    /// Someone else holds the lock now, or nobody does (5.6).
    Lost(Option<Lock>),
}

/// Heartbeat and phase change (5.3 step 3). Also how a session learns that it lost the lock.
pub fn heartbeat(store: &dyn Store, session: &mut Session, phase: Phase) -> Result<Beat> {
    for _ in 0..MAX_CAS_ATTEMPTS {
        let Some(cur) = read_state(store)? else {
            session.lost = true;
            return Ok(Beat::Lost(None));
        };
        if session.lost || !cur.value.is_held_by(&session.session_id) {
            session.lost = true;
            return Ok(Beat::Lost(cur.value.lock.clone()));
        }
        let mut next = cur.value.clone();
        let lock = next.lock.as_mut().expect("held by this session");
        lock.beat += 1;
        lock.phase = phase;
        if lock.since.is_none() {
            lock.since = session.acquired_at.clone();
        }
        let message = format!("{} heartbeat {}", lock.holder, lock.beat);
        match write_state(store, &next, Some(&cur.sha), &message) {
            Ok(w) => {
                session.since_written = true;
                return Ok(Beat::Ok(Versioned {
                    value: next,
                    sha: w.sha,
                }));
            }
            Err(Error::Conflict) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(Error::Conflict)
}

#[derive(Debug)]
pub enum TakeOver {
    Ok {
        state: Versioned<State>,
        session: Session,
        previous: Lock,
    },
    /// The host wrote again: not stale any more.
    Alive(Lock),
    /// The lock changed hands or was released since the user looked.
    Changed(Option<Lock>),
    NoWorld,
}

/// Take over a dead session (5.5). `expected` is the lock exactly as the user
/// saw it when it looked stale and they confirmed. The old host's heartbeat
/// and this write contend for the same file, so exactly one of them wins:
/// - a heartbeat that lands first makes this compare-and-swap fail, and the
///   re-read shows a higher `beat`: the host is alive;
/// - the commit-time check (I6) covers the rest.
///
/// Comparing `beat` does not depend on GitHub's commit list being up to date.
pub fn take_over(
    store: &dyn Store,
    me: &Identity,
    cfg: &Config,
    expected: &Lock,
    session_id: &str,
) -> Result<TakeOver> {
    for _ in 0..MAX_CAS_ATTEMPTS {
        let Some(cur) = read_state(store)? else {
            return Ok(TakeOver::NoWorld);
        };
        let previous = match &cur.value.lock {
            Some(l) if l.session_id == session_id => {
                // Our earlier attempt landed; only its answer was lost.
                let at = store.last_commit(STATE_PATH)?.map(|c| c.commit_time);
                let session = session_for(session_id, l.base_generation, at);
                return Ok(TakeOver::Ok {
                    state: cur,
                    session,
                    previous: expected.clone(),
                });
            }
            Some(l) if l.session_id == expected.session_id && l.beat != expected.beat => {
                return Ok(TakeOver::Alive(l.clone()));
            }
            Some(l) if l.session_id == expected.session_id => l.clone(),
            other => return Ok(TakeOver::Changed(other.clone())),
        };
        match lock_view(store, &cur.value, cfg)? {
            LockView::Held { stale: true, .. } => {}
            _ => return Ok(TakeOver::Alive(previous)),
        }
        let mut next = cur.value.clone();
        next.lock = Some(new_lock(me, session_id, next.generation));
        let message = format!("{} takes over from {}", me.name, previous.holder);
        match write_state(store, &next, Some(&cur.sha), &message) {
            Ok(w) => {
                let session = session_for(session_id, next.generation, w.commit_time);
                return Ok(TakeOver::Ok {
                    state: Versioned {
                        value: next,
                        sha: w.sha,
                    },
                    session,
                    previous,
                });
            }
            Err(Error::Conflict) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(Error::Conflict)
}

#[derive(Debug, PartialEq)]
pub enum Released {
    Ok,
    /// The lock was not ours (any more). Nothing was written (I7).
    NotMine(Option<Lock>),
}

/// Release the lock (5.4 step 6, Cancel hosting).
pub fn release(store: &dyn Store, session: &mut Session) -> Result<Released> {
    for _ in 0..MAX_CAS_ATTEMPTS {
        let Some(cur) = read_state(store)? else {
            return Ok(Released::NotMine(None));
        };
        if session.lost || !cur.value.is_held_by(&session.session_id) {
            session.lost = true;
            return Ok(Released::NotMine(cur.value.lock.clone()));
        }
        let mut next = cur.value.clone();
        let holder = next.lock.take().map(|l| l.holder).unwrap_or_default();
        match write_state(store, &next, Some(&cur.sha), &format!("{holder} stops hosting")) {
            Ok(_) => return Ok(Released::Ok),
            Err(Error::Conflict) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(Error::Conflict)
}

fn saves_release(store: &dyn Store) -> Result<Release> {
    store
        .find_release(SAVES_TAG)?
        .ok_or_else(|| Error::Refused("the saves release is missing on GitHub".into()))
}

fn referenced_assets(state: &State) -> Vec<u64> {
    let mut ids: Vec<u64> = state.history.iter().map(|s| s.asset_id).collect();
    ids.push(state.latest.asset_id);
    ids
}

/// I3: the asset is complete and holds exactly the bytes we hashed. Uses
/// GitHub's digest when it reports one, otherwise downloads the asset back.
pub fn verify_asset(store: &dyn Store, asset: &Asset, sha256: &str, size: u64) -> Result<()> {
    if asset.state != "uploaded" {
        return Err(Error::Verify(format!(
            "{} is not fully uploaded ({})",
            asset.name, asset.state
        )));
    }
    if asset.size != size {
        return Err(Error::Verify(format!(
            "{} has {} bytes, expected {size}",
            asset.name, asset.size
        )));
    }
    match asset.digest.as_deref().filter(|d| !d.is_empty()) {
        Some(d) if d.eq_ignore_ascii_case(&format!("sha256:{sha256}")) => Ok(()),
        Some(d) => Err(Error::Verify(format!(
            "{} has digest {d}, expected sha256:{sha256}",
            asset.name
        ))),
        None => {
            let bytes = store.download_asset(asset.id)?;
            if util::sha256_hex(&bytes) == sha256 {
                Ok(())
            } else {
                Err(Error::Verify(format!(
                    "{} does not hash to what was uploaded",
                    asset.name
                )))
            }
        }
    }
}

/// Upload and verify. A leftover asset with the same name (an earlier attempt)
/// is reused when it is complete and identical, otherwise replaced, unless the
/// state references it.
fn upload_verified(
    store: &dyn Store,
    release: &Release,
    name: &str,
    data: &[u8],
    sha256: &str,
    protected: &[u64],
) -> Result<Asset> {
    let size = data.len() as u64;
    if let Some(old) = store.list_assets(release)?.into_iter().find(|a| a.name == name) {
        if verify_asset(store, &old, sha256, size).is_ok() {
            return Ok(old);
        }
        if protected.contains(&old.id) {
            return Err(Error::Refused(format!("{name} already exists and is in use")));
        }
        store.delete_asset(old.id)?;
    }
    let asset = store.upload_asset(release, name, data)?;
    if let Err(e) = verify_asset(store, &asset, sha256, size) {
        // A bad upload is never referenced; remove it so a retry can reuse the name.
        let _ = store.delete_asset(asset.id);
        return Err(e);
    }
    Ok(asset)
}

fn pack_and_read(
    paths: &Paths,
    cfg: &Config,
    world: &str,
    what: PackKind<'_>,
    meta: &SaveMeta,
    file_name: &str,
) -> Result<(archive::Packed, Vec<u8>)> {
    let sidecars = paths.sidecars(world, &cfg.world_sidecars)?;
    let out = paths.cache().join(format!("out-{file_name}"));
    let packed = archive::pack(paths, world, &sidecars, what, meta, &out)?;
    let data = fs::read(&packed.path).at(&packed.path)?;
    let _ = fs::remove_file(&packed.path);
    Ok((packed, data))
}

/// Store the world as a side copy (5.6): an asset that `state.json` never
/// references. Allowed with or without the lock.
pub fn store_side_copy(
    store: &dyn Store,
    paths: &Paths,
    cfg: &Config,
    me: &Identity,
    world: &str,
    base_generation: u64,
    meta: &SaveMeta,
) -> Result<Asset> {
    let release = saves_release(store)?;
    let name = side_asset_name(base_generation, &me.name, util::now_unix());
    let what = PackKind {
        kind: "side",
        generation: None,
        base_generation: Some(base_generation),
    };
    let (packed, data) = pack_and_read(paths, cfg, world, what, meta, &name)?;
    upload_verified(store, &release, &name, &data, &packed.sha256, &[])
}

#[derive(Debug)]
pub enum Upload {
    /// Uploaded and verified; not referenced yet. Record it, then [`commit_save`].
    Ready(PendingUpload),
    /// The lock was no longer ours, so the world went up as a side copy (5.6).
    SideCopy(Asset),
}

/// 5.4 step 3, first half: pack, upload, verify (I3). Checks the lock first;
/// after a lost lock (I7) the upload becomes a side copy instead.
#[allow(clippy::too_many_arguments)]
pub fn upload_save(
    store: &dyn Store,
    paths: &Paths,
    cfg: &Config,
    session: &mut Session,
    me: &Identity,
    world: &str,
    kind: SaveKind,
    meta: &SaveMeta,
) -> Result<Upload> {
    let Some(cur) = read_state(store)? else {
        return Err(Error::Refused("the shared world is gone from GitHub".into()));
    };
    if cur.value.world_name != world {
        return Err(Error::Refused(format!(
            "the shared world is {}, not {world}",
            cur.value.world_name
        )));
    }
    if session.lost || !cur.value.is_held_by(&session.session_id) {
        session.lost = true;
        let asset = store_side_copy(store, paths, cfg, me, world, session.base_generation, meta)?;
        return Ok(Upload::SideCopy(asset));
    }
    let release = saves_release(store)?;
    let generation = cur.value.generation + 1;
    let name = save_asset_name(generation, &session.session_id);
    let what = PackKind {
        kind: kind.as_str(),
        generation: Some(generation),
        base_generation: Some(session.base_generation),
    };
    let (packed, data) = pack_and_read(paths, cfg, world, what, meta, &name)?;
    let asset = upload_verified(
        store,
        &release,
        &name,
        &data,
        &packed.sha256,
        &referenced_assets(&cur.value),
    )?;
    Ok(Upload::Ready(PendingUpload {
        asset_id: asset.id,
        asset_name: asset.name,
        sha256: packed.sha256,
        size: packed.size,
        generation,
        kind,
        saved_at: asset.created_at,
        saved_by: meta.saved_by.clone(),
        game_build: meta.game_build.clone(),
        game_version: meta.game_version.clone(),
        mods_fingerprint: meta.mods_fingerprint.clone(),
        files: packed.manifest.files,
    }))
}

#[derive(Debug)]
pub enum Commit {
    Published(SaveRef),
    /// The lock was lost after the upload; the asset was renamed to a side copy.
    SideCopy(Asset),
}

/// 5.4 step 3, second half: make a verified upload `latest` (I2). If the lock
/// was lost in between, the asset becomes a side copy instead.
pub fn commit_save(
    store: &dyn Store,
    session: &mut Session,
    me: &Identity,
    pending: &PendingUpload,
    phase_after: Phase,
) -> Result<Commit> {
    for _ in 0..MAX_CAS_ATTEMPTS {
        let Some(cur) = read_state(store)? else {
            return Err(Error::Refused("the shared world is gone from GitHub".into()));
        };
        let st = &cur.value;
        if st.latest.asset_id == pending.asset_id {
            // An earlier attempt landed; only its answer was lost.
            return Ok(Commit::Published(st.latest.clone()));
        }
        if session.lost || !st.is_held_by(&session.session_id) {
            session.lost = true;
            let side = side_asset_name(session.base_generation, &me.name, util::now_unix());
            let asset = store.rename_asset(pending.asset_id, &side)?;
            return Ok(Commit::SideCopy(asset));
        }
        if st.generation + 1 != pending.generation {
            return Err(Error::Refused(format!(
                "save #{} was due next, but the shared state is at #{}",
                pending.generation, st.generation
            )));
        }
        let entry = SaveRef {
            generation: pending.generation,
            asset_id: pending.asset_id,
            asset_name: pending.asset_name.clone(),
            sha256: pending.sha256.clone(),
            size: pending.size,
            kind: pending.kind,
            saved_by: pending.saved_by.clone(),
            saved_at: pending.saved_at.clone(),
            game_build: pending.game_build.clone(),
            game_version: pending.game_version.clone(),
            mods_fingerprint: pending.mods_fingerprint.clone(),
            extra: Map::new(),
        };
        let mut next = st.clone();
        next.generation = pending.generation;
        next.latest = entry.clone();
        next.history.insert(0, entry.clone());
        if let Some(lock) = next.lock.as_mut() {
            lock.phase = phase_after;
        }
        let message = format!("save #{} by {}", pending.generation, me.name);
        match write_state(store, &next, Some(&cur.sha), &message) {
            Ok(_) => return Ok(Commit::Published(entry)),
            Err(Error::Conflict) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(Error::Conflict)
}

/// Upload, let the caller record the pending upload (so a crash can finish
/// the job from the journal), then commit.
#[allow(clippy::too_many_arguments)]
pub fn publish_save(
    store: &dyn Store,
    paths: &Paths,
    cfg: &Config,
    session: &mut Session,
    me: &Identity,
    world: &str,
    kind: SaveKind,
    meta: &SaveMeta,
    phase_after: Phase,
    record_pending: &mut dyn FnMut(Option<&PendingUpload>) -> Result<()>,
) -> Result<Commit> {
    match upload_save(store, paths, cfg, session, me, world, kind, meta)? {
        Upload::SideCopy(asset) => Ok(Commit::SideCopy(asset)),
        Upload::Ready(pending) => {
            record_pending(Some(&pending))?;
            let commit = commit_save(store, session, me, &pending, phase_after)?;
            record_pending(None)?;
            Ok(commit)
        }
    }
}

#[derive(Debug)]
pub enum Import {
    Shared(Versioned<State>),
    /// A world is shared already. If our upload had gone through, it is kept as a side copy.
    AlreadyShared {
        state: Option<State>,
        kept_as_side_copy: Option<Asset>,
    },
}

/// First import (5.8): share a world from this PC as save #1. The caller moves
/// the world out afterwards (I5).
pub fn first_import(store: &dyn Store, paths: &Paths, me: &Identity, world: &str, meta: &SaveMeta) -> Result<Import> {
    validate_world_name(world)?;
    world::check_complete(&paths.world_dir(world))?;
    if let Some(cur) = read_state(store)? {
        return Ok(Import::AlreadyShared {
            state: Some(cur.value),
            kept_as_side_copy: None,
        });
    }
    let config_file = store.read_file(CONFIG_PATH)?;
    let cfg = match &config_file {
        Some(f) => Config::from_json(&f.content)?,
        None => Config::default(),
    };
    let release = match store.find_release(SAVES_TAG)? {
        Some(r) => r,
        None => match store.create_release(SAVES_TAG) {
            Ok(r) => r,
            Err(Error::Http { status: 422, .. }) => saves_release(store)?,
            Err(e) => return Err(e),
        },
    };
    let session_id = new_session_id();
    let name = save_asset_name(1, &session_id);
    let what = PackKind {
        kind: SaveKind::Import.as_str(),
        generation: Some(1),
        base_generation: None,
    };
    let (packed, data) = pack_and_read(paths, &cfg, world, what, meta, &name)?;
    let asset = upload_verified(store, &release, &name, &data, &packed.sha256, &[])?;
    if config_file.is_none() {
        match store.write_file(CONFIG_PATH, &cfg.to_json(), None, "Create config.json") {
            Ok(_) | Err(Error::Conflict) => {}
            Err(e) => return Err(e),
        }
    }
    let entry = SaveRef {
        generation: 1,
        asset_id: asset.id,
        asset_name: asset.name.clone(),
        sha256: packed.sha256,
        size: packed.size,
        kind: SaveKind::Import,
        saved_by: meta.saved_by.clone(),
        saved_at: asset.created_at.clone(),
        game_build: meta.game_build.clone(),
        game_version: meta.game_version.clone(),
        mods_fingerprint: meta.mods_fingerprint.clone(),
        extra: Map::new(),
    };
    let state = State {
        schema: SCHEMA,
        world_name: world.to_string(),
        generation: 1,
        latest: entry.clone(),
        history: vec![entry],
        lock: None,
        extra: Map::new(),
    };
    // Create-only: if someone else created state.json first, this fails.
    match write_state(store, &state, None, &format!("{} shares the world {world}", me.name)) {
        Ok(w) => Ok(Import::Shared(Versioned {
            value: state,
            sha: w.sha,
        })),
        Err(Error::Conflict) => {
            let side = side_asset_name(0, &me.name, util::now_unix());
            let kept = store.rename_asset(asset.id, &side)?;
            Ok(Import::AlreadyShared {
                state: read_state(store)?.map(|v| v.value),
                kept_as_side_copy: Some(kept),
            })
        }
        Err(e) => Err(e),
    }
}

#[derive(Debug)]
pub struct Installed {
    /// The world's files now on disk (record them in the journal).
    pub files: Vec<FileEntry>,
    pub moved_out: Option<MovedOut>,
    pub skipped_sidecars: Vec<String>,
}

/// 5.3 step 4: download `latest`, check its SHA-256, extract, check every file
/// (S8), move whatever is there out (I4), put the world in place.
pub fn install_latest(
    store: &dyn Store,
    paths: &Paths,
    cfg: &Config,
    state: &State,
    session_id: &str,
) -> Result<Installed> {
    let latest = &state.latest;
    let world = state.world_name.as_str();
    let bytes = store.download_asset(latest.asset_id)?;
    if bytes.len() as u64 != latest.size || util::sha256_hex(&bytes) != latest.sha256 {
        return Err(Error::Verify(format!(
            "the download of save #{} does not match its hash",
            latest.generation
        )));
    }
    let cache = paths.cache();
    fs::create_dir_all(&cache).at(&cache)?;
    let zip_path = cache.join(format!("in-{}", latest.asset_name));
    util::write_atomic(&zip_path, &bytes)?;
    let short: String = session_id.chars().take(8).collect();
    let staging = cache.join(format!("install-{short}"));
    if staging.exists() {
        fs::remove_dir_all(&staging).at(&staging)?; // our own leftovers
    }
    let sidecars = paths.sidecars(world, &cfg.world_sidecars)?;
    let extracted = archive::extract_verified(&zip_path, &staging, Some(world), &sidecars)?;
    let moved_out = world::install_extracted(paths, world, &sidecars, &staging, "before-install")?;
    let _ = fs::remove_dir_all(&staging);
    let _ = fs::remove_file(&zip_path);
    let files = world::snapshot(paths, world, &sidecars)?;
    let mut expected: Vec<FileEntry> = extracted
        .manifest
        .files
        .iter()
        .filter(|f| !extracted.skipped.contains(&f.path))
        .cloned()
        .collect();
    expected.sort();
    if files != expected {
        return Err(Error::Verify("the installed world does not match the save".into()));
    }
    Ok(Installed {
        files,
        moved_out,
        skipped_sidecars: extracted.skipped,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Journal idle: if the world is on this PC, move it out and tell the user (I5).
    MoveOutIfPresent,
    /// Install never finished: delete temp files, release the lock, go idle.
    AbortInstall,
    ResumeHeartbeat,
    /// Run Finish (5.4).
    Finish,
    /// Run Lost lock (5.6). Commit a pending upload first: that turns it into a side copy.
    LostLock,
}

/// The recovery table (5.7).
pub fn recovery_action(journal: &Journal, state: Option<&State>, game_running: bool) -> Recovery {
    let mine = match (&journal.session, state) {
        (Some(s), Some(st)) => !s.lost && st.is_held_by(&s.session_id),
        _ => false,
    };
    match journal.phase {
        JournalPhase::Idle => Recovery::MoveOutIfPresent,
        JournalPhase::Installing => Recovery::AbortInstall,
        JournalPhase::Hosting if mine && game_running => Recovery::ResumeHeartbeat,
        JournalPhase::Hosting if mine => Recovery::Finish,
        JournalPhase::Hosting => Recovery::LostLock,
    }
}
