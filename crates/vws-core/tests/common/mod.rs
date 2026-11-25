//! Shared test harness: two friends (Ori and Tomer), each with their own game
//! and app folders, talking to one remote. The scenarios run unchanged
//! against the local stand-in (`DirStore`) and, when configured, against the
//! GitHub scratch repo.

#![allow(dead_code)]

use std::fs;
use std::sync::{Arc, Barrier};

use tempfile::TempDir;
use vws_core::archive::{self, SaveMeta};
use vws_core::journal::Session;
use vws_core::model::{Config, Identity, Lock, Phase, SaveKind, State};
use vws_core::paths::{Paths, Sidecar};
use vws_core::protocol::{self, Acquired, Beat, Commit, Import, Released, TakeOver, Upload};
use vws_core::store::Store;
use vws_core::store::faulty::FaultyStore;
use vws_core::world::{self, FileEntry};
use vws_core::{Error, util};

pub const WORLD: &str = "HomeServer";

pub type Remote = Arc<FaultyStore<Arc<dyn Store>>>;

pub struct Friend {
    pub me: Identity,
    pub paths: Paths,
    pub store: Remote,
    _dir: TempDir,
}

impl Friend {
    pub fn new(name: &str, store: Arc<dyn Store>) -> Friend {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(
            dir.path().join("game").join("IronGate").join("Valheim"),
            dir.path().join("app"),
        );
        fs::create_dir_all(paths.worlds_local()).unwrap();
        Friend {
            me: Identity {
                name: name.into(),
                client_id: uuid_like(name),
            },
            paths,
            store: Arc::new(FaultyStore::new(store)),
            _dir: dir,
        }
    }

    pub fn meta(&self) -> SaveMeta {
        SaveMeta {
            saved_by: self.me.name.clone(),
            game_build: "26870846".into(),
            game_version: "1.0.30".into(),
            mods_fingerprint: "sha256:test".into(),
        }
    }

    pub fn sidecars(&self, cfg: &Config) -> Vec<Sidecar> {
        self.paths.sidecars(WORLD, &cfg.world_sidecars).unwrap()
    }

    pub fn snapshot(&self, cfg: &Config) -> Vec<FileEntry> {
        world::snapshot(&self.paths, WORLD, &self.sidecars(cfg)).unwrap()
    }

    pub fn state(&self) -> protocol::Versioned<State> {
        protocol::read_state(&*self.store).unwrap().expect("a shared world")
    }
}

fn uuid_like(name: &str) -> String {
    format!(
        "{:0>8}-0000-4000-8000-000000000000",
        util::sha256_hex(name.as_bytes())[..8].to_string()
    )
}

pub struct Harness {
    pub a: Friend,
    pub b: Friend,
    pub cfg: Config,
    /// Make every current lock stale on the remote's clock.
    pub make_stale: Arc<dyn Fn() + Send + Sync>,
    pub _remote: Option<TempDir>,
}

/// Deterministic file content.
pub fn bytes(seed: u8, n: usize) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

fn sidecar_path(paths: &Paths) -> std::path::PathBuf {
    paths
        .game_data
        .join("worlds")
        .join(format!("{WORLD}.one_map_to_rule_them_all.explored"))
}

/// A 1.0-style world folder: one `_main.N` set, chunk files, a minimap cache,
/// and the One Map To Rule Them All sidecar.
pub fn write_world(paths: &Paths, save_no: u64, seed: u8) {
    let dir = paths.world_dir(WORLD);
    fs::create_dir_all(&dir).unwrap();
    for (i, ext) in ["chunks", "db2", "fwl2", "ok"].iter().enumerate() {
        fs::write(
            dir.join(format!("_main.{save_no}.{ext}")),
            bytes(seed + i as u8, 64 + i * 900),
        )
        .unwrap();
    }
    fs::write(dir.join("1c_20__2_7.chunk"), bytes(seed, 40_000)).unwrap();
    fs::write(dir.join("1e_1e__1_1.chunk"), bytes(seed.wrapping_add(9), 25_000)).unwrap();
    fs::write(dir.join("cacheMinimapMeta"), [1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
    let sc = sidecar_path(paths);
    fs::create_dir_all(sc.parent().unwrap()).unwrap();
    fs::write(sc, bytes(seed, 3_000)).unwrap();
}

/// What a play session does on disk: a new `_main.N` set replaces the old
/// one, a chunk is rewritten under a new name, the map sidecar changes.
pub fn play(paths: &Paths, seed: u8) {
    let dir = paths.world_dir(WORLD);
    let n = world::check_complete(&dir).unwrap().save_number;
    for (i, ext) in ["chunks", "db2", "fwl2", "ok"].iter().enumerate() {
        fs::remove_file(dir.join(format!("_main.{n}.{ext}"))).unwrap();
        fs::write(
            dir.join(format!("_main.{}.{ext}", n + 1)),
            bytes(seed + i as u8, 70 + i * 950),
        )
        .unwrap();
    }
    let old = dir.join("1c_20__2_7.chunk");
    if old.exists() {
        fs::remove_file(old).unwrap();
    }
    fs::write(
        dir.join(format!("1c_20__2_{}.chunk", 8 + seed as u32)),
        bytes(seed, 41_000),
    )
    .unwrap();
    fs::write(sidecar_path(paths), bytes(seed.wrapping_add(1), 3_100)).unwrap();
}

/// Ori shares a world (save #1) and moves his copy out. Returns its files.
pub fn setup_shared_world(h: &Harness) -> Vec<FileEntry> {
    write_world(&h.a.paths, 8, 1);
    let files = h.a.snapshot(&h.cfg);
    match protocol::first_import(&*h.a.store, &h.a.paths, &h.a.me, WORLD, &h.a.meta()).unwrap() {
        Import::Shared(st) => assert_eq!(st.value.generation, 1),
        other => panic!("import failed: {other:?}"),
    }
    let moved = world::move_world_out(&h.a.paths, WORLD, &h.a.sidecars(&h.cfg), "after-import").unwrap();
    assert!(moved.is_some());
    assert!(!world::world_present(&h.a.paths.world_dir(WORLD)));
    files
}

/// Take the lock and install the latest save.
pub fn host(f: &Friend, cfg: &Config) -> (Session, Vec<FileEntry>) {
    let sid = protocol::new_session_id();
    match protocol::acquire(&*f.store, &f.me, &sid).unwrap() {
        Acquired::Ok { state, session } => {
            assert_eq!(session.session_id, sid);
            let inst = protocol::install_latest(&*f.store, &f.paths, cfg, &state.value, &sid).unwrap();
            (session, inst.files)
        }
        other => panic!("{} could not host: {other:?}", f.me.name),
    }
}

pub fn publish(f: &Friend, cfg: &Config, session: &mut Session) -> vws_core::Result<Commit> {
    protocol::publish_save(
        &*f.store,
        &f.paths,
        cfg,
        session,
        &f.me,
        WORLD,
        SaveKind::Session,
        &f.meta(),
        Phase::Finishing,
        &mut |_| Ok(()),
    )
}

/// Finish (5.4): upload, release, move the world out.
pub fn finish(f: &Friend, cfg: &Config, session: &mut Session) -> Commit {
    let c = publish(f, cfg, session).unwrap();
    if matches!(c, Commit::Published(_)) {
        assert_eq!(protocol::release(&*f.store, session).unwrap(), Released::Ok);
    }
    world::move_world_out(&f.paths, WORLD, &f.sidecars(cfg), "after-finish").unwrap();
    c
}

fn assert_state_unchanged(f: &Friend, before: &protocol::Versioned<State>) {
    let now = f.state();
    assert_eq!(now.sha, before.sha, "state.json must not have been written");
    assert_eq!(now.value, before.value);
}

fn current_lock(f: &Friend) -> Lock {
    f.state().value.lock.expect("someone holds the lock")
}

// ---------------------------------------------------------------------------
// M1 acceptance tests (PLAN.md section 11)
// ---------------------------------------------------------------------------

/// Two simultaneous acquires give exactly one winner.
pub fn two_simultaneous_acquires(h: &Harness, rounds: usize) {
    setup_shared_world(h);
    for round in 0..rounds {
        let barrier = Barrier::new(2);
        let (ra, rb) = std::thread::scope(|s| {
            let ta = s.spawn(|| {
                barrier.wait();
                protocol::acquire(&*h.a.store, &h.a.me, &protocol::new_session_id())
            });
            let tb = s.spawn(|| {
                barrier.wait();
                protocol::acquire(&*h.b.store, &h.b.me, &protocol::new_session_id())
            });
            (ta.join().unwrap().unwrap(), tb.join().unwrap().unwrap())
        });
        let (winner, mut session, loser_saw) = match (ra, rb) {
            (Acquired::Ok { session, .. }, Acquired::HeldBy(l)) => (&h.a, session, l),
            (Acquired::HeldBy(l), Acquired::Ok { session, .. }) => (&h.b, session, l),
            other => panic!("round {round}: expected exactly one winner, got {other:?}"),
        };
        assert_eq!(
            loser_saw.session_id, session.session_id,
            "the loser must see the winner's lock"
        );
        assert!(winner.state().value.is_held_by(&session.session_id));
        assert_eq!(protocol::release(&*winner.store, &mut session).unwrap(), Released::Ok);
    }
}

/// A write with a stale SHA is rejected (I1).
pub fn stale_sha_is_rejected(h: &Harness) {
    setup_shared_world(h);
    let store = &*h.a.store;
    let first = store.read_file("state.json").unwrap().unwrap();
    let mut st = State::from_json(&first.content).unwrap();
    st.extra
        .insert("test_marker".into(), serde_json::json!(util::now_unix()));
    let w = store
        .write_file("state.json", &st.to_json(), Some(&first.sha), "test: change")
        .unwrap();
    assert_ne!(w.sha, first.sha);
    let stale = store.write_file("state.json", &first.content, Some(&first.sha), "test: stale write");
    assert!(
        matches!(stale, Err(Error::Conflict)),
        "a stale sha must be rejected, got {stale:?}"
    );
    let create = store.write_file("state.json", &first.content, None, "test: create over");
    assert!(
        matches!(create, Err(Error::Conflict)),
        "creating over an existing file must conflict, got {create:?}"
    );
    assert_eq!(store.read_file("state.json").unwrap().unwrap().sha, w.sha);
}

/// A save is never referenced before its upload is verified (I3).
pub fn unverified_save_is_never_referenced(h: &Harness) {
    setup_shared_world(h);
    let (mut session, _) = host(&h.a, &h.cfg);
    play(&h.a.paths, 2);
    let before = h.a.state();
    let release = h.a.store.find_release("saves").unwrap().unwrap();

    h.a.store.corrupt_uploads(true);
    let r = publish(&h.a, &h.cfg, &mut session);
    h.a.store.corrupt_uploads(false);
    assert!(
        matches!(r, Err(Error::Verify(_))),
        "a corrupted upload must fail verification, got {r:?}"
    );
    assert_state_unchanged(&h.a, &before);
    let names: Vec<String> =
        h.a.store
            .list_assets(&release)
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
    assert!(
        !names.iter().any(|n| n.starts_with("save-000002-")),
        "the bad upload must be gone: {names:?}"
    );

    h.a.store.fail_uploads(true);
    let r = publish(&h.a, &h.cfg, &mut session);
    h.a.store.fail_uploads(false);
    assert!(matches!(r, Err(Error::Network(_))), "got {r:?}");
    assert_state_unchanged(&h.a, &before);

    let Commit::Published(entry) = publish(&h.a, &h.cfg, &mut session).unwrap() else {
        panic!("expected the third attempt to publish");
    };
    assert_eq!(entry.generation, 2);
    let downloaded = h.a.store.download_asset(entry.asset_id).unwrap();
    assert_eq!(util::sha256_hex(&downloaded), entry.sha256);
    assert_eq!(h.a.state().value.latest, entry);
    assert_eq!(protocol::release(&*h.a.store, &mut session).unwrap(), Released::Ok);
}

/// An upload after a lost lock becomes a side copy (5.6, I7).
pub fn upload_after_lost_lock_becomes_side_copy(h: &Harness) {
    setup_shared_world(h);
    let (mut sa, _) = host(&h.a, &h.cfg);
    play(&h.a.paths, 3);
    let played = h.a.snapshot(&h.cfg);
    (h.make_stale)();
    let seen = current_lock(&h.a);
    let TakeOver::Ok { session: mut sb, .. } =
        protocol::take_over(&*h.b.store, &h.b.me, &h.cfg, &seen, &protocol::new_session_id()).unwrap()
    else {
        panic!("takeover of a stale lock must succeed");
    };
    let before = h.a.state();

    let c = publish(&h.a, &h.cfg, &mut sa).unwrap();
    let Commit::SideCopy(side) = c else {
        panic!("expected a side copy, got {c:?}")
    };
    assert!(side.name.starts_with("side-000001-Ori-"), "{}", side.name);
    assert!(sa.lost);
    assert_state_unchanged(&h.a, &before);

    // I7: the old host writes neither lock nor latest any more.
    let beat = protocol::heartbeat(&*h.a.store, &mut sa, Phase::InGame).unwrap();
    assert!(
        matches!(&beat, Beat::Lost(Some(l)) if l.session_id == sb.session_id),
        "got {beat:?}"
    );
    assert!(matches!(
        protocol::release(&*h.a.store, &mut sa).unwrap(),
        Released::NotMine(Some(_))
    ));
    let u = protocol::upload_save(
        &*h.a.store,
        &h.a.paths,
        &h.cfg,
        &mut sa,
        &h.a.me,
        WORLD,
        SaveKind::Session,
        &h.a.meta(),
    )
    .unwrap();
    assert!(matches!(u, Upload::SideCopy(_)), "got {u:?}");
    assert_state_unchanged(&h.a, &before);

    // The side copy holds exactly what Ori played.
    let zip = h.a.paths.cache().join("check-side.zip");
    fs::create_dir_all(zip.parent().unwrap()).unwrap();
    fs::write(&zip, h.a.store.download_asset(side.id).unwrap()).unwrap();
    let ex = archive::extract_verified(
        &zip,
        &h.a.paths.cache().join("check-side"),
        Some(WORLD),
        &h.a.sidecars(&h.cfg),
    )
    .unwrap();
    assert_eq!(ex.manifest.files, played);
    assert_eq!(ex.manifest.base_generation, Some(1));

    assert_eq!(protocol::release(&*h.b.store, &mut sb).unwrap(), Released::Ok);
}

/// Variant of the above: the lock is lost after the upload, before the state write.
pub fn lock_lost_between_upload_and_commit(h: &Harness) {
    setup_shared_world(h);
    let (mut sa, _) = host(&h.a, &h.cfg);
    play(&h.a.paths, 4);
    let store_b = h.b.store.clone();
    let me_b = h.b.me.clone();
    let cfg = h.cfg.clone();
    let make_stale = h.make_stale.clone();
    let observer = h.a.store.clone();
    h.a.store.after_next_upload(move || {
        make_stale();
        let seen = protocol::read_state(&*observer).unwrap().unwrap().value.lock.unwrap();
        let r = protocol::take_over(&*store_b, &me_b, &cfg, &seen, &protocol::new_session_id()).unwrap();
        assert!(matches!(r, TakeOver::Ok { .. }), "takeover in the hook failed: {r:?}");
    });
    let c = publish(&h.a, &h.cfg, &mut sa).unwrap();
    let Commit::SideCopy(side) = c else {
        panic!("expected a side copy, got {c:?}")
    };
    assert!(side.name.starts_with("side-"), "{}", side.name);
    let st = h.a.state().value;
    assert_eq!(st.generation, 1, "the lost session must not have published");
    assert!(st.history.iter().all(|s| s.asset_id != side.id));
    assert_eq!(st.lock.unwrap().holder, "Tomer");
}

// ---------------------------------------------------------------------------
// More protocol behaviour
// ---------------------------------------------------------------------------

/// The world travels: Ori shares, Tomer hosts and plays, Ori gets Tomer's progress (sidecar included).
pub fn full_handoff(h: &Harness) {
    let imported = setup_shared_world(h);
    let (mut sb, files_b) = host(&h.b, &h.cfg);
    assert_eq!(files_b, imported, "Tomer must get exactly what Ori shared");
    play(&h.b.paths, 5);
    let played = h.b.snapshot(&h.cfg);
    let Commit::Published(s2) = finish(&h.b, &h.cfg, &mut sb) else {
        panic!("Tomer's save must publish")
    };
    assert_eq!(s2.generation, 2);
    assert_eq!(s2.saved_by, "Tomer");
    assert!(
        !world::world_present(&h.b.paths.world_dir(WORLD)),
        "I5: Tomer's copy must be moved out"
    );

    let (mut sa, files_a) = host(&h.a, &h.cfg);
    assert_eq!(
        files_a, played,
        "Ori must get exactly what Tomer played, sidecar included"
    );
    let st = h.a.state().value;
    assert_eq!(st.latest.generation, 2);
    assert_eq!(st.history.iter().map(|s| s.generation).collect::<Vec<_>>(), vec![2, 1]);
    assert_eq!(protocol::release(&*h.a.store, &mut sa).unwrap(), Released::Ok);
}

/// Heartbeat and takeover race on the same file: exactly one wins (5.5).
pub fn heartbeat_and_takeover_race(h: &Harness) {
    setup_shared_world(h);
    let (mut sa, _) = host(&h.a, &h.cfg);
    (h.make_stale)();
    let seen = current_lock(&h.a);
    let barrier = Barrier::new(2);
    let (beat, take) = std::thread::scope(|s| {
        let t1 = s.spawn(|| {
            barrier.wait();
            protocol::heartbeat(&*h.a.store, &mut sa, Phase::InGame).unwrap()
        });
        let t2 = s.spawn(|| {
            barrier.wait();
            protocol::take_over(&*h.b.store, &h.b.me, &h.cfg, &seen, &protocol::new_session_id()).unwrap()
        });
        (t1.join().unwrap(), t2.join().unwrap())
    });
    match (&beat, &take) {
        (Beat::Ok(_), TakeOver::Alive(_)) => assert!(h.a.state().value.is_held_by(&sa.session_id)),
        (Beat::Lost(Some(l)), TakeOver::Ok { session, .. }) => assert_eq!(l.session_id, session.session_id),
        other => panic!("expected exactly one winner, got {other:?}"),
    }
}

/// A takeover is refused while the host is alive.
pub fn takeover_refused_while_host_alive(h: &Harness) {
    setup_shared_world(h);
    let (mut sa, _) = host(&h.a, &h.cfg);
    let seen = current_lock(&h.a);
    let r = protocol::take_over(&*h.b.store, &h.b.me, &h.cfg, &seen, &protocol::new_session_id()).unwrap();
    assert!(matches!(r, TakeOver::Alive(_)), "got {r:?}");
    // A heartbeat after the user looked also means alive, even if the lock looks stale.
    (h.make_stale)();
    let seen = current_lock(&h.a);
    assert!(matches!(
        protocol::heartbeat(&*h.a.store, &mut sa, Phase::InGame).unwrap(),
        Beat::Ok(_)
    ));
    (h.make_stale)();
    let r = protocol::take_over(&*h.b.store, &h.b.me, &h.cfg, &seen, &protocol::new_session_id()).unwrap();
    assert!(
        matches!(r, TakeOver::Alive(_)),
        "a newer beat must count as alive, got {r:?}"
    );
    assert_eq!(protocol::release(&*h.a.store, &mut sa).unwrap(), Released::Ok);
}

/// Lost answers to writes that did land are harmless: retries recognise them.
pub fn lost_responses_are_harmless(h: &Harness) {
    setup_shared_world(h);
    let sid = protocol::new_session_id();
    h.a.store.lose_write_responses(1);
    let r = protocol::acquire(&*h.a.store, &h.a.me, &sid);
    assert!(matches!(r, Err(Error::Network(_))), "got {r:?}");
    let Acquired::Ok { state, mut session } = protocol::acquire(&*h.a.store, &h.a.me, &sid).unwrap() else {
        panic!("the retry must recognise its own lock");
    };
    protocol::install_latest(&*h.a.store, &h.a.paths, &h.cfg, &state.value, &sid).unwrap();
    play(&h.a.paths, 6);
    let Upload::Ready(pending) = protocol::upload_save(
        &*h.a.store,
        &h.a.paths,
        &h.cfg,
        &mut session,
        &h.a.me,
        WORLD,
        SaveKind::Session,
        &h.a.meta(),
    )
    .unwrap() else {
        panic!("expected an upload");
    };
    h.a.store.lose_write_responses(1);
    let r = protocol::commit_save(&*h.a.store, &mut session, &h.a.me, &pending, Phase::Finishing);
    assert!(matches!(r, Err(Error::Network(_))), "got {r:?}");
    let Commit::Published(entry) =
        protocol::commit_save(&*h.a.store, &mut session, &h.a.me, &pending, Phase::Finishing).unwrap()
    else {
        panic!("the retry must see its own save");
    };
    assert_eq!(entry.asset_id, pending.asset_id);
    let st = h.a.state().value;
    assert_eq!(st.generation, 2);
    assert_eq!(
        st.history.iter().filter(|s| s.generation == 2).count(),
        1,
        "no double entry"
    );
    assert_eq!(protocol::release(&*h.a.store, &mut session).unwrap(), Released::Ok);
}

/// A second first-import is refused and touches nothing.
pub fn second_import_is_refused(h: &Harness) {
    setup_shared_world(h);
    let before = h.a.state();
    write_world(&h.b.paths, 3, 7);
    let r = protocol::first_import(&*h.b.store, &h.b.paths, &h.b.me, WORLD, &h.b.meta()).unwrap();
    assert!(
        matches!(
            r,
            Import::AlreadyShared {
                kept_as_side_copy: None,
                ..
            }
        ),
        "got {r:?}"
    );
    assert_state_unchanged(&h.a, &before);
    assert!(
        world::world_present(&h.b.paths.world_dir(WORLD)),
        "Tomer's own world must stay where it is"
    );
}
