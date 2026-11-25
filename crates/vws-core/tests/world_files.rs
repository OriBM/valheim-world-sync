//! World folder and save zip behaviour: completeness (S7), presence (I5),
//! moving out (I4), install, and the zip checks (S8).

mod common;

use std::fs;
use std::io::Write;

use common::{WORLD, bytes, play, write_world};
use vws_core::Error;
use vws_core::archive::{self, PackKind, SaveMeta};
use vws_core::model::Config;
use vws_core::paths::Paths;
use vws_core::world::{self, FileEntry};

fn paths() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let p = Paths::new(dir.path().join("game"), dir.path().join("app"));
    fs::create_dir_all(p.worlds_local()).unwrap();
    (dir, p)
}

fn sidecars(p: &Paths) -> Vec<vws_core::paths::Sidecar> {
    p.sidecars(WORLD, &Config::default().world_sidecars).unwrap()
}

const SIDE: PackKind<'static> = PackKind {
    kind: "side",
    generation: None,
    base_generation: Some(1),
};

#[test]
fn completeness_uses_the_newest_main_set() {
    let (_d, p) = paths();
    write_world(&p, 8, 1);
    let dir = p.world_dir(WORLD);
    let c = world::check_complete(&dir).unwrap();
    assert_eq!(c.save_number, 8);
    assert!(c.data_bytes > 65_000);
    // A newer, half-written set (no .ok yet) is incomplete.
    fs::write(dir.join("_main.9.chunks"), b"x").unwrap();
    fs::write(dir.join("_main.9.db2"), b"x").unwrap();
    let e = world::check_complete(&dir).unwrap_err();
    assert!(e.to_string().contains("_main.9.ok"), "{e}");
    fs::remove_file(dir.join("_main.9.chunks")).unwrap();
    fs::remove_file(dir.join("_main.9.db2")).unwrap();
    play(&p, 3);
    assert_eq!(world::check_complete(&dir).unwrap().save_number, 9);
}

#[test]
fn a_cloud_worlds_cache_folder_is_not_a_world() {
    let (_d, p) = paths();
    let dir = p.world_dir(WORLD);
    fs::create_dir_all(&dir).unwrap();
    for f in [
        "cacheMinimapBiome",
        "cacheMinimapHeight",
        "cacheMinimapMask",
        "cacheMinimapMeta",
    ] {
        fs::write(dir.join(f), b"cache").unwrap();
    }
    assert!(!world::world_present(&dir));
    write_world(&p, 1, 1);
    assert!(world::world_present(&dir));
}

#[test]
fn moving_out_takes_folder_siblings_and_sidecars_and_deletes_nothing() {
    let (_d, p) = paths();
    write_world(&p, 8, 1);
    let wl = p.worlds_local();
    fs::create_dir_all(wl.join("HomeServer_backup_auto-20260911-230456")).unwrap();
    fs::write(
        wl.join("HomeServer_backup_auto-20260911-230456").join("_main.4.ok"),
        b"ok",
    )
    .unwrap();
    fs::write(wl.join("HomeServer_mapTexCache"), b"other world cache").unwrap();
    fs::create_dir_all(wl.join("oritomer")).unwrap();
    let before = world::snapshot(&p, WORLD, &sidecars(&p)).unwrap();

    let moved = world::move_world_out(&p, WORLD, &sidecars(&p), "test")
        .unwrap()
        .unwrap();
    assert_eq!(moved.items.len(), 3, "{:?}", moved.items);
    assert!(!p.world_dir(WORLD).exists());
    assert!(!wl.join("HomeServer_backup_auto-20260911-230456").exists());
    assert!(wl.join("HomeServer_mapTexCache").exists(), "unrelated files stay");
    assert!(wl.join("oritomer").exists(), "other worlds stay");
    let b = &moved.backup_dir;
    assert!(b.join("worlds_local").join(WORLD).join("_main.8.ok").exists());
    assert!(
        b.join("worlds_local")
            .join("HomeServer_backup_auto-20260911-230456")
            .join("_main.4.ok")
            .exists()
    );
    assert!(
        b.join("sidecars")
            .join("worlds")
            .join("HomeServer.one_map_to_rule_them_all.explored")
            .exists()
    );
    assert!(b.join("info.json").exists());

    // Nothing to move the second time.
    assert!(
        world::move_world_out(&p, WORLD, &sidecars(&p), "test")
            .unwrap()
            .is_none()
    );
    assert!(!before.is_empty());
}

#[test]
fn move_path_never_overwrites() {
    let (d, _p) = paths();
    let a = d.path().join("a");
    let b = d.path().join("b");
    fs::write(&a, b"a").unwrap();
    fs::write(&b, b"b").unwrap();
    assert!(world::move_path(&a, &b).is_err());
    assert_eq!(fs::read(&b).unwrap(), b"b");
    assert!(a.exists());
}

#[test]
fn pack_extract_roundtrip_and_install_moves_the_old_world_out() {
    let (_d, p) = paths();
    write_world(&p, 8, 1);
    let original = world::snapshot(&p, WORLD, &sidecars(&p)).unwrap();
    assert!(original.iter().any(|f| f.path.starts_with("sidecars/worlds/")));
    let zip = p.cache().join("save.zip");
    let packed = archive::pack(&p, WORLD, &sidecars(&p), SIDE, &SaveMeta::default(), &zip).unwrap();
    assert_eq!(packed.manifest.files, original);
    assert_eq!(archive::read_manifest(&zip).unwrap(), packed.manifest);

    // The player keeps playing; then the save is installed over it.
    play(&p, 4);
    let staging = p.cache().join("staging");
    let ex = archive::extract_verified(&zip, &staging, Some(WORLD), &sidecars(&p)).unwrap();
    assert!(ex.skipped.is_empty());
    let moved = world::install_extracted(&p, WORLD, &sidecars(&p), &staging, "before-install").unwrap();
    assert!(
        moved.is_some(),
        "the played world must have been moved out, not overwritten"
    );
    assert_eq!(world::snapshot(&p, WORLD, &sidecars(&p)).unwrap(), original);
}

#[test]
fn extraction_rejects_the_wrong_world_and_unlisted_sidecars_are_skipped() {
    let (_d, p) = paths();
    write_world(&p, 8, 1);
    let zip = p.cache().join("save.zip");
    archive::pack(&p, WORLD, &sidecars(&p), SIDE, &SaveMeta::default(), &zip).unwrap();
    let e = archive::extract_verified(&zip, &p.cache().join("x1"), Some("OtherWorld"), &sidecars(&p)).unwrap_err();
    assert!(matches!(e, Error::Verify(_)), "{e}");
    let ex = archive::extract_verified(&zip, &p.cache().join("x2"), Some(WORLD), &[]).unwrap();
    assert_eq!(
        ex.skipped,
        vec!["sidecars/worlds/HomeServer.one_map_to_rule_them_all.explored".to_string()]
    );
    assert!(!p.cache().join("x2").join("sidecars").exists());
}

/// Build a zip by hand: (manifest file list, actual entries).
fn handmade_zip(path: &std::path::Path, listed: &[FileEntry], entries: &[(&str, Vec<u8>)]) {
    let manifest = serde_json::json!({
        "schema": 1, "world_name": WORLD, "kind": "session", "saved_by": "x", "files": listed,
    });
    let f = fs::File::create(path).unwrap();
    let mut zw = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default();
    zw.start_file("manifest.json", opts).unwrap();
    zw.write_all(manifest.to_string().as_bytes()).unwrap();
    for (name, data) in entries {
        zw.start_file(*name, opts).unwrap();
        zw.write_all(data).unwrap();
    }
    zw.finish().unwrap();
}

fn entry(path: &str, data: &[u8]) -> FileEntry {
    FileEntry {
        path: path.into(),
        size: data.len() as u64,
        sha256: vws_core::util::sha256_hex(data),
    }
}

#[test]
fn extraction_rejects_tampered_content() {
    let (d, p) = paths();
    let good = bytes(1, 1000);
    let mut bad = good.clone();
    bad[10] ^= 1;
    let zip = d.path().join("tampered.zip");
    handmade_zip(
        &zip,
        &[entry("world/HomeServer/_main.1.ok", &good)],
        &[("world/HomeServer/_main.1.ok", bad)],
    );
    let e = archive::extract_verified(&zip, &p.cache().join("t"), Some(WORLD), &[]).unwrap_err();
    assert!(matches!(e, Error::Verify(_)), "{e}");
}

#[test]
fn extraction_rejects_paths_outside_the_world() {
    let (d, p) = paths();
    let data = b"evil".to_vec();
    for evil in [
        "world/HomeServer/../../../escape.txt",
        "world/OtherWorld/_main.1.ok",
        "characters_local/ori.fch",
        "/abs.txt",
        "world/HomeServer/C:/x",
    ] {
        let zip = d.path().join("evil.zip");
        handmade_zip(&zip, &[entry(evil, &data)], &[(evil, data.clone())]);
        let out = p.cache().join("e");
        let e = archive::extract_verified(&zip, &out, Some(WORLD), &[]).unwrap_err();
        assert!(matches!(e, Error::Verify(_)), "{evil}: {e}");
        assert!(!d.path().join("escape.txt").exists());
        let _ = fs::remove_dir_all(&out);
    }
}

#[test]
fn extraction_rejects_missing_and_unlisted_files() {
    let (d, p) = paths();
    let a = bytes(1, 100);
    let zip = d.path().join("missing.zip");
    handmade_zip(
        &zip,
        &[
            entry("world/HomeServer/_main.1.ok", &a),
            entry("world/HomeServer/_main.1.db2", &a),
        ],
        &[("world/HomeServer/_main.1.ok", a.clone())],
    );
    assert!(matches!(
        archive::extract_verified(&zip, &p.cache().join("m"), Some(WORLD), &[]),
        Err(Error::Verify(_))
    ));
    let zip2 = d.path().join("extra.zip");
    handmade_zip(
        &zip2,
        &[entry("world/HomeServer/_main.1.ok", &a)],
        &[
            ("world/HomeServer/_main.1.ok", a.clone()),
            ("world/HomeServer/extra.bin", a.clone()),
        ],
    );
    assert!(matches!(
        archive::extract_verified(&zip2, &p.cache().join("n"), Some(WORLD), &[]),
        Err(Error::Verify(_))
    ));
}

#[test]
fn backups_are_pruned_oldest_first() {
    let (_d, p) = paths();
    for i in 0..13 {
        fs::create_dir_all(p.backups().join(format!("20261125-1200{i:02}Z-test"))).unwrap();
    }
    let removed = world::prune_backups(&p, 10).unwrap();
    assert_eq!(removed.len(), 3);
    assert!(!p.backups().join("20261125-120000Z-test").exists());
    assert!(p.backups().join("20261125-120012Z-test").exists());
}
