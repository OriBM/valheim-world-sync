//! Protocol tests against the local stand-in for GitHub (`DirStore`).
//! Each friend gets their own `DirStore` instance on the shared folder, so the
//! cross-process file lock is exercised too.

mod common;

use std::sync::Arc;

use common::*;
use vws_core::model::Config;
use vws_core::store::Store;
use vws_core::store::dir::{DirStore, ManualClock};

fn harness() -> Harness {
    let remote = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(1_800_000_000);
    let a: Arc<dyn Store> = Arc::new(DirStore::with_clock(remote.path(), clock.clone()).unwrap());
    let b: Arc<dyn Store> = Arc::new(DirStore::with_clock(remote.path(), clock.clone()).unwrap());
    let cfg = Config::default();
    let stale_by = cfg.stale_after_seconds as i64 + 60;
    Harness {
        a: Friend::new("Ori", a),
        b: Friend::new("Tomer", b),
        cfg,
        make_stale: Arc::new(move || clock.advance(stale_by)),
        _remote: Some(remote),
    }
}

#[test]
fn m1_two_simultaneous_acquires_have_exactly_one_winner() {
    two_simultaneous_acquires(&harness(), 25);
}

#[test]
fn m1_stale_sha_is_rejected() {
    stale_sha_is_rejected(&harness());
}

#[test]
fn m1_save_is_never_referenced_before_its_upload_is_verified() {
    unverified_save_is_never_referenced(&harness());
}

#[test]
fn m1_upload_after_lost_lock_becomes_side_copy() {
    upload_after_lost_lock_becomes_side_copy(&harness());
}

#[test]
fn m1_lock_lost_between_upload_and_commit_becomes_side_copy() {
    lock_lost_between_upload_and_commit(&harness());
}

#[test]
fn world_travels_between_friends() {
    full_handoff(&harness());
}

#[test]
fn heartbeat_and_takeover_race_has_one_winner() {
    for _ in 0..10 {
        heartbeat_and_takeover_race(&harness());
    }
}

#[test]
fn takeover_is_refused_while_host_is_alive() {
    takeover_refused_while_host_alive(&harness());
}

#[test]
fn lost_responses_are_harmless() {
    common::lost_responses_are_harmless(&harness());
}

#[test]
fn second_import_is_refused() {
    common::second_import_is_refused(&harness());
}
