//! The M1 scenarios against a real GitHub scratch repo. Ignored by default.
//!
//! Run them only against a scratch repo, never the real data repo: each test
//! starts by deleting `state.json`, `config.json` and every asset of the
//! `saves` release. As a guard, the repo name must contain `scratch` or `test`.
//!
//! ```powershell
//! $env:VWS_TEST_REPO = "owner/valheim-world-scratch"
//! $env:VWS_TEST_TOKEN = "<fine-grained token for that repo only>"
//! cargo test -p vws-core --test live_github -- --ignored --test-threads=1
//! ```

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use vws_core::model::{CONFIG_PATH, Config, SAVES_TAG, STATE_PATH};
use vws_core::setup_code::Secret;
use vws_core::store::Store;
use vws_core::store::github::GitHubStore;

fn harness() -> Harness {
    let repo = std::env::var("VWS_TEST_REPO").expect("set VWS_TEST_REPO=owner/scratch-repo");
    let token = std::env::var("VWS_TEST_TOKEN").expect("set VWS_TEST_TOKEN");
    let (owner, name) = repo.split_once('/').expect("VWS_TEST_REPO must be owner/name");
    let lower = name.to_ascii_lowercase();
    assert!(
        lower.contains("scratch") || lower.contains("test"),
        "refusing to wipe {repo}: the scratch repo's name must contain 'scratch' or 'test'"
    );
    let client = || -> Arc<dyn Store> { Arc::new(GitHubStore::new(owner, name, Secret::new(token.clone())).unwrap()) };
    let a = client();
    reset(&*a);
    let cfg = Config {
        stale_after_seconds: 5,
        ..Config::default()
    };
    Harness {
        a: Friend::new("Ori", a),
        b: Friend::new("Tomer", client()),
        cfg,
        make_stale: Arc::new(|| std::thread::sleep(Duration::from_secs(9))),
        _remote: None,
    }
}

fn reset(store: &dyn Store) {
    for path in [STATE_PATH, CONFIG_PATH] {
        if let Some(f) = store.read_file(path).unwrap() {
            store.delete_file(path, &f.sha, "test reset").unwrap();
        }
    }
    if let Some(release) = store.find_release(SAVES_TAG).unwrap() {
        for asset in store.list_assets(&release).unwrap() {
            store.delete_asset(asset.id).unwrap();
        }
    }
}

#[test]
#[ignore = "needs VWS_TEST_REPO and VWS_TEST_TOKEN (a scratch repo)"]
fn live_m1_two_simultaneous_acquires_have_exactly_one_winner() {
    two_simultaneous_acquires(&harness(), 3);
}

#[test]
#[ignore = "needs VWS_TEST_REPO and VWS_TEST_TOKEN (a scratch repo)"]
fn live_m1_stale_sha_is_rejected() {
    stale_sha_is_rejected(&harness());
}

#[test]
#[ignore = "needs VWS_TEST_REPO and VWS_TEST_TOKEN (a scratch repo)"]
fn live_m1_save_is_never_referenced_before_its_upload_is_verified() {
    unverified_save_is_never_referenced(&harness());
}

#[test]
#[ignore = "needs VWS_TEST_REPO and VWS_TEST_TOKEN (a scratch repo)"]
fn live_m1_upload_after_lost_lock_becomes_side_copy() {
    upload_after_lost_lock_becomes_side_copy(&harness());
}

#[test]
#[ignore = "needs VWS_TEST_REPO and VWS_TEST_TOKEN (a scratch repo)"]
fn live_m1_lock_lost_between_upload_and_commit_becomes_side_copy() {
    lock_lost_between_upload_and_commit(&harness());
}

#[test]
#[ignore = "needs VWS_TEST_REPO and VWS_TEST_TOKEN (a scratch repo)"]
fn live_world_travels_between_friends() {
    full_handoff(&harness());
}

#[test]
#[ignore = "needs VWS_TEST_REPO and VWS_TEST_TOKEN (a scratch repo)"]
fn live_heartbeat_and_takeover_race_has_one_winner() {
    heartbeat_and_takeover_race(&harness());
}

#[test]
#[ignore = "needs VWS_TEST_REPO and VWS_TEST_TOKEN (a scratch repo)"]
fn live_lost_responses_are_harmless() {
    common::lost_responses_are_harmless(&harness());
}
