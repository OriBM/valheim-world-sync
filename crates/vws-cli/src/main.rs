//! `vws`: developer harness for vws-core (M1). Drives the protocol by hand,
//! one step per command, with the journal saved between steps exactly as the
//! app will do it.
//!
//! Remote selection:
//!   VWS_REMOTE=dir:<folder>         local stand-in for GitHub (offline; two
//!                                   friends can share the folder)
//!   VWS_SETUP_CODE=VWS1....         GitHub, from a setup code
//!   VWS_REPO=owner/name + VWS_TOKEN GitHub, directly
//! Folders: VWS_GAME_DATA_DIR, VWS_DATA_DIR (debug builds default to a sandbox).

use std::env;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use vws_core::archive::{self, PackKind, SaveMeta};
use vws_core::journal::{Journal, JournalPhase, Session};
use vws_core::model::{Config, Identity, Lock, Phase, SaveKind, State};
use vws_core::paths::Paths;
use vws_core::protocol::{self, Acquired, Beat, Commit, Import, LockView, Recovery, Released, TakeOver};
use vws_core::settings::Settings;
use vws_core::setup_code::{self, Secret};
use vws_core::store::Store;
use vws_core::store::dir::DirStore;
use vws_core::store::github::GitHubStore;
use vws_core::{Error, Result, mods, world};

const HELP: &str = "\
vws: Valheim World Sync developer harness

Setup and local:
  vws paths                     show the folders in use
  vws init <display name>       create settings.json for this PC
  vws check <World>             S7: is worlds_local\\<World> complete?
  vws pack <World> <out.zip>    zip a world with its sidecars
  vws verify <save.zip>         check a save zip against its manifest
  vws mods <Gale profile dir>   mods list and fingerprint
  vws make-setup-code <owner/repo>
                                print a setup code built from VWS_TOKEN (a secret!)

Hosting (needs a remote, see below):
  vws status                    shared state, who hosts, staleness
  vws import <World>            share a world from this PC (first import)
  vws host                      take the lock and install the latest save
  vws beat [ready|in_game]      send one heartbeat
  vws finish                    upload the world, release the lock, move the world out
  vws cancel                    release without saving (world must be unchanged)
  vws takeover                  take over from a host that looks dead
  vws recover                   run the recovery table once (Valheim assumed closed)

Remote: VWS_REMOTE=dir:<folder> (offline stand-in), or VWS_SETUP_CODE,
or VWS_REPO=owner/name with VWS_TOKEN. Folders: VWS_GAME_DATA_DIR, VWS_DATA_DIR.
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Ctx {
    paths: Paths,
    settings: Settings,
    store: Arc<dyn Store>,
    remote: String,
}

impl Ctx {
    fn me(&self) -> Identity {
        self.settings.identity()
    }
    fn journal(&self) -> Result<Journal> {
        Journal::load(&self.paths.journal_file())
    }
    fn save(&self, j: &mut Journal) -> Result<()> {
        j.save(&self.paths.journal_file())
    }
    fn config(&self) -> Result<Config> {
        protocol::read_config(&*self.store)
    }
    fn meta(&self) -> Result<SaveMeta> {
        let mods_fingerprint = match &self.settings.gale_profile_dir {
            Some(d) => mods::fingerprint(&mods::list_mods(Path::new(d))?),
            None => String::new(),
        };
        Ok(SaveMeta {
            saved_by: self.settings.display_name.clone(),
            game_build: env::var("VWS_GAME_BUILD").unwrap_or_default(),
            game_version: env::var("VWS_GAME_VERSION").unwrap_or_default(),
            mods_fingerprint,
        })
    }
}

fn arg<'a>(rest: &'a [String], i: usize, what: &str) -> Result<&'a str> {
    rest.get(i)
        .map(String::as_str)
        .ok_or_else(|| Error::Invalid(format!("missing {what}; see `vws help`")))
}

fn run(args: &[String]) -> Result<()> {
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let rest = args.get(1..).unwrap_or_default();
    let paths = Paths::from_env()?;
    match cmd {
        "help" | "-h" | "--help" => {
            print!("{HELP}");
            Ok(())
        }
        "paths" => {
            println!("game data: {}", paths.game_data.display());
            println!("app data:  {}", paths.app_data.display());
            Ok(())
        }
        "init" => init(&paths, rest),
        "check" => check(&paths, arg(rest, 0, "a world name")?),
        "pack" => pack(&paths, arg(rest, 0, "a world name")?, arg(rest, 1, "an output file")?),
        "verify" => verify(&paths, arg(rest, 0, "a zip file")?),
        "mods" => {
            let list = mods::list_mods(Path::new(arg(rest, 0, "a Gale profile folder")?))?;
            for m in &list {
                println!("{m}");
            }
            println!("{} mods, fingerprint {}", list.len(), mods::fingerprint(&list));
            Ok(())
        }
        "make-setup-code" => {
            let repo = arg(rest, 0, "owner/repo")?;
            let (owner, name) = repo
                .split_once('/')
                .ok_or_else(|| Error::Invalid("use owner/repo".into()))?;
            let token = env::var("VWS_TOKEN").map_err(|_| Error::Refused("set VWS_TOKEN first".into()))?;
            println!("{}", setup_code::encode(owner, name, &token)?);
            eprintln!("This code is a secret: send it to each friend privately, once.");
            Ok(())
        }
        _ => {
            let ctx = connect(paths)?;
            match cmd {
                "status" => status(&ctx),
                "import" => import(&ctx, arg(rest, 0, "a world name")?),
                "host" => host(&ctx),
                "beat" => beat(&ctx, rest.first().map(String::as_str)),
                "finish" => finish(&ctx),
                "cancel" => cancel(&ctx),
                "takeover" => takeover(&ctx),
                "recover" => recover(&ctx),
                other => Err(Error::Invalid(format!("unknown command {other:?}; see `vws help`"))),
            }
        }
    }
}

fn connect(paths: Paths) -> Result<Ctx> {
    let settings = Settings::load(&paths.settings_file())?
        .ok_or_else(|| Error::Refused("run `vws init <your name>` first".into()))?;
    let var = |k: &str| env::var(k).ok().filter(|v| !v.trim().is_empty());
    let (store, remote): (Arc<dyn Store>, String) = match var("VWS_REMOTE") {
        Some(r) if r.starts_with("dir:") => {
            let dir = &r[4..];
            (Arc::new(DirStore::open(dir)?), format!("local stand-in at {dir}"))
        }
        Some(r) if r != "github" => {
            return Err(Error::Invalid("VWS_REMOTE must be `github` or `dir:<folder>`".into()));
        }
        _ => {
            let gh = if let Some(code) = var("VWS_SETUP_CODE") {
                GitHubStore::from_setup_code(&setup_code::parse(&code)?)?
            } else if let (Some(repo), Some(token)) = (var("VWS_REPO"), var("VWS_TOKEN")) {
                let (owner, name) = repo
                    .split_once('/')
                    .ok_or_else(|| Error::Invalid("VWS_REPO must be owner/name".into()))?;
                GitHubStore::new(owner, name, Secret::new(token))?
            } else {
                return Err(Error::Refused(
                    "no remote: set VWS_REMOTE=dir:<folder>, or VWS_SETUP_CODE, or VWS_REPO and VWS_TOKEN".into(),
                ));
            };
            gh.check_access()?;
            let label = format!("GitHub {}", gh.repo_name());
            (Arc::new(gh), label)
        }
    };
    Ok(Ctx {
        paths,
        settings,
        store,
        remote,
    })
}

fn init(paths: &Paths, rest: &[String]) -> Result<()> {
    let name = rest.join(" ");
    if let Some(s) = Settings::load(&paths.settings_file())? {
        return Err(Error::Refused(format!(
            "this PC is already set up as {:?} ({})",
            s.display_name,
            paths.settings_file().display()
        )));
    }
    let s = Settings::new(&name)?;
    s.save(&paths.settings_file())?;
    println!("Set up as {:?} with client id {}.", s.display_name, s.client_id);
    Ok(())
}

fn check(paths: &Paths, world_name: &str) -> Result<()> {
    let dir = paths.world_dir(world_name);
    if !world::world_present(&dir) {
        println!("{} holds no world (no _main.* files).", dir.display());
        return Ok(());
    }
    let c = world::check_complete(&dir)?;
    println!(
        "Complete: newest save #{} with {} bytes of world data.",
        c.save_number, c.data_bytes
    );
    Ok(())
}

fn pack(paths: &Paths, world_name: &str, out: &str) -> Result<()> {
    let sidecars = paths.sidecars(world_name, &Config::default().world_sidecars)?;
    let what = PackKind {
        kind: "manual",
        generation: None,
        base_generation: None,
    };
    let p = archive::pack(paths, world_name, &sidecars, what, &SaveMeta::default(), Path::new(out))?;
    println!(
        "{} files, {} bytes, sha256 {}",
        p.manifest.files.len(),
        p.size,
        p.sha256
    );
    Ok(())
}

fn verify(paths: &Paths, zip: &str) -> Result<()> {
    let m = archive::read_manifest(Path::new(zip))?;
    let sidecars = paths.sidecars(&m.world_name, &Config::default().world_sidecars)?;
    let tmp = paths
        .cache()
        .join(format!("verify-{}", &protocol::new_session_id()[..8]));
    let result = archive::extract_verified(Path::new(zip), &tmp, None, &sidecars);
    let _ = std::fs::remove_dir_all(&tmp);
    let ex = result?;
    println!(
        "OK: world {}, {} files match the manifest ({} skipped).",
        ex.manifest.world_name,
        ex.manifest.files.len(),
        ex.skipped.len()
    );
    Ok(())
}

fn describe_save(st: &State) -> String {
    let l = &st.latest;
    format!(
        "#{} ({}) by {} at {}, {} bytes",
        l.generation,
        l.kind.as_str(),
        l.saved_by,
        l.saved_at,
        l.size
    )
}

fn status(ctx: &Ctx) -> Result<()> {
    println!("remote:  {}", ctx.remote);
    let journal = ctx.journal()?;
    println!("this PC: {} (journal: {:?})", ctx.settings.display_name, journal.phase);
    let Some(cur) = protocol::read_state(&*ctx.store)? else {
        println!("No shared world yet. Share one with `vws import <World>`.");
        return Ok(());
    };
    let st = &cur.value;
    let cfg = ctx.config()?;
    println!("world:   {}; last save {}", st.world_name, describe_save(st));
    match protocol::lock_view(&*ctx.store, st, &cfg)? {
        LockView::Free => println!("Nobody is hosting."),
        LockView::Held {
            lock,
            silent_for,
            stale,
        } => {
            println!(
                "{} is hosting (phase {:?}, beat {}, since {}, last signal {} s ago).",
                lock.holder,
                lock.phase,
                lock.beat,
                lock.since.as_deref().unwrap_or("?"),
                silent_for
            );
            if stale {
                println!("Their session looks dead. `vws takeover` can take over.");
            }
        }
    }
    Ok(())
}

fn import(ctx: &Ctx, world_name: &str) -> Result<()> {
    match protocol::first_import(&*ctx.store, &ctx.paths, &ctx.me(), world_name, &ctx.meta()?)? {
        Import::Shared(st) => {
            println!("Shared {} as save {}.", world_name, describe_save(&st.value));
            let cfg = ctx.config()?;
            let sidecars = ctx.paths.sidecars(world_name, &cfg.world_sidecars)?;
            if let Some(m) = world::move_world_out(&ctx.paths, world_name, &sidecars, "after-import")? {
                println!(
                    "Moved this PC's copy to {} so nobody hosts a stale copy by accident.",
                    m.backup_dir.display()
                );
            }
        }
        Import::AlreadyShared {
            state,
            kept_as_side_copy,
        } => {
            match state {
                Some(st) => println!(
                    "A world is shared already: {}, save {}.",
                    st.world_name,
                    describe_save(&st)
                ),
                None => println!("A world is shared already."),
            }
            if let Some(a) = kept_as_side_copy {
                println!("Your upload was kept as side copy {}.", a.name);
            }
        }
    }
    Ok(())
}

/// After taking the lock: install, or undo the lock if the install fails.
fn install_after_lock(ctx: &Ctx, journal: &mut Journal, state: &State, mut session: Session) -> Result<()> {
    journal.phase = JournalPhase::Installing;
    journal.world_name = Some(state.world_name.clone());
    journal.session = Some(session.clone());
    ctx.save(journal)?;
    let cfg = ctx.config()?;
    match protocol::install_latest(&*ctx.store, &ctx.paths, &cfg, state, &session.session_id) {
        Ok(inst) => {
            if let Some(m) = &inst.moved_out {
                println!("Moved what was there to {}.", m.backup_dir.display());
            }
            journal.phase = JournalPhase::Hosting;
            journal.installed = Some(inst.files);
            ctx.save(journal)?;
            match protocol::heartbeat(&*ctx.store, &mut session, Phase::Ready)? {
                Beat::Ok(_) => println!(
                    "World is ready: {} save {}.\nStart Valheim, host, close it when done, then run `vws finish`.",
                    state.world_name,
                    describe_save(state)
                ),
                Beat::Lost(l) => println!(
                    "Lost the lock right away to {:?}. Run `vws finish`.",
                    l.map(|l| l.holder)
                ),
            }
            journal.session = Some(session);
            ctx.save(journal)
        }
        Err(e) => {
            eprintln!("Install failed; releasing the lock.");
            let _ = protocol::release(&*ctx.store, &mut session);
            journal.reset();
            ctx.save(journal)?;
            Err(e)
        }
    }
}

fn require_idle(journal: &Journal) -> Result<()> {
    if journal.is_idle() {
        Ok(())
    } else {
        Err(Error::Refused(
            "this PC is in the middle of a session; run `vws recover`".into(),
        ))
    }
}

fn host(ctx: &Ctx) -> Result<()> {
    let mut journal = ctx.journal()?;
    require_idle(&journal)?;
    let sid = protocol::new_session_id();
    // Record the session id first, so a retry after a lost answer recognises its own lock.
    journal.phase = JournalPhase::Installing;
    journal.session = Some(Session {
        session_id: sid.clone(),
        base_generation: 0,
        acquired_at: None,
        since_written: false,
        lost: false,
    });
    ctx.save(&mut journal)?;
    match protocol::acquire(&*ctx.store, &ctx.me(), &sid)? {
        Acquired::Ok { state, session } => install_after_lock(ctx, &mut journal, &state.value, session),
        Acquired::HeldBy(l) => {
            journal.reset();
            ctx.save(&mut journal)?;
            println!("{} is hosting. Join through Steam.", l.holder);
            Ok(())
        }
        Acquired::NoWorld => {
            journal.reset();
            ctx.save(&mut journal)?;
            println!("No shared world yet.");
            Ok(())
        }
    }
}

fn hosting(journal: &Journal) -> Result<(String, Session)> {
    match (journal.phase, &journal.world_name, &journal.session) {
        (JournalPhase::Hosting, Some(w), Some(s)) => Ok((w.clone(), s.clone())),
        _ => Err(Error::Refused("this PC is not hosting".into())),
    }
}

fn beat(ctx: &Ctx, phase: Option<&str>) -> Result<()> {
    let mut journal = ctx.journal()?;
    let (_, mut session) = hosting(&journal)?;
    let phase = match phase.unwrap_or("in_game") {
        "ready" => Phase::Ready,
        "in_game" => Phase::InGame,
        other => return Err(Error::Invalid(format!("unknown phase {other:?}"))),
    };
    match protocol::heartbeat(&*ctx.store, &mut session, phase)? {
        Beat::Ok(st) => println!("Heartbeat {} sent.", st.value.lock.map(|l| l.beat).unwrap_or(0)),
        Beat::Lost(l) => println!(
            "{} took over hosting. Your progress will be stored as a side copy: run `vws finish`.",
            l.map(|l| l.holder).unwrap_or_else(|| "Nobody".into())
        ),
    }
    journal.session = Some(session);
    ctx.save(&mut journal)
}

/// S7, second half: the world holds at least half the data it started with.
fn size_check(ctx: &Ctx, world_name: &str, journal: &Journal) -> Result<()> {
    let c = world::check_complete(&ctx.paths.world_dir(world_name))?;
    let prev: u64 = journal
        .installed
        .iter()
        .flatten()
        .filter(|f| {
            let name = f.path.rsplit('/').next().unwrap_or("");
            f.path.starts_with("world/") && (name.starts_with("_main.") || name.ends_with(".chunk"))
        })
        .map(|f| f.size)
        .sum();
    if prev > 0 && c.data_bytes * 2 < prev {
        return Err(Error::Refused(format!(
            "the world has {} bytes of data but started with {prev}; not uploading automatically",
            c.data_bytes
        )));
    }
    Ok(())
}

fn report(c: &Commit) {
    match c {
        Commit::Published(s) => println!("Uploaded save #{} ({} bytes).", s.generation, s.size),
        Commit::SideCopy(a) => println!("Someone else hosts now; stored your progress as side copy {}.", a.name),
    }
}

fn finish(ctx: &Ctx) -> Result<()> {
    let jp = ctx.paths.journal_file();
    let mut journal = ctx.journal()?;
    let (world_name, mut session) = hosting(&journal)?;
    let cfg = ctx.config()?;
    let sidecars = ctx.paths.sidecars(&world_name, &cfg.world_sidecars)?;
    let me = ctx.me();
    let meta = ctx.meta()?;

    // An upload from an earlier attempt that never made it into state.json.
    if let Some(p) = journal.pending.clone() {
        let c = protocol::commit_save(&*ctx.store, &mut session, &me, &p, Phase::Finishing)?;
        report(&c);
        journal.last_uploaded = Some(p.files.clone());
        journal.pending = None;
        journal.session = Some(session.clone());
        journal.save(&jp)?;
    }

    let files = world::snapshot(&ctx.paths, &world_name, &sidecars)?;
    if journal.unchanged(&files) {
        println!("The world is unchanged since it was installed or last uploaded; nothing to upload.");
    } else if session.lost {
        let a = protocol::store_side_copy(
            &*ctx.store,
            &ctx.paths,
            &cfg,
            &me,
            &world_name,
            session.base_generation,
            &meta,
        )?;
        println!("Stored your progress as side copy {}.", a.name);
        journal.last_uploaded = Some(files);
    } else {
        size_check(ctx, &world_name, &journal)?;
        let c = protocol::publish_save(
            &*ctx.store,
            &ctx.paths,
            &cfg,
            &mut session,
            &me,
            &world_name,
            SaveKind::Session,
            &meta,
            Phase::Finishing,
            &mut |p| {
                journal.pending = p.cloned();
                journal.save(&jp)
            },
        )?;
        report(&c);
        journal.last_uploaded = Some(files);
    }
    journal.session = Some(session.clone());
    journal.save(&jp)?;

    if !session.lost {
        match protocol::release(&*ctx.store, &mut session)? {
            Released::Ok => println!("Released the lock."),
            Released::NotMine(l) => println!("The lock was not ours any more ({:?}).", l.map(|l| l.holder)),
        }
    }
    if let Some(m) = world::move_world_out(&ctx.paths, &world_name, &sidecars, "after-finish")? {
        println!("Moved the world to {}.", m.backup_dir.display());
    }
    journal.reset();
    journal.save(&jp)?;
    world::prune_backups(&ctx.paths, 10)?;
    Ok(())
}

fn cancel(ctx: &Ctx) -> Result<()> {
    let mut journal = ctx.journal()?;
    let (world_name, mut session) = hosting(&journal)?;
    let cfg = ctx.config()?;
    let sidecars = ctx.paths.sidecars(&world_name, &cfg.world_sidecars)?;
    let files = world::snapshot(&ctx.paths, &world_name, &sidecars)?;
    if journal.installed.as_deref() != Some(&files[..]) {
        return Err(Error::Refused(
            "the world changed since it was installed; use `vws finish`".into(),
        ));
    }
    world::move_world_out(&ctx.paths, &world_name, &sidecars, "cancel")?;
    match protocol::release(&*ctx.store, &mut session)? {
        Released::Ok => println!("Cancelled; the lock is free."),
        Released::NotMine(l) => println!("The lock was not ours any more ({:?}).", l.map(|l| l.holder)),
    }
    journal.reset();
    ctx.save(&mut journal)
}

fn takeover(ctx: &Ctx) -> Result<()> {
    let mut journal = ctx.journal()?;
    require_idle(&journal)?;
    let cur = protocol::read_state(&*ctx.store)?.ok_or_else(|| Error::Refused("no shared world yet".into()))?;
    let cfg = ctx.config()?;
    let seen: Lock = match protocol::lock_view(&*ctx.store, &cur.value, &cfg)? {
        LockView::Free => {
            println!("Nobody is hosting; use `vws host`.");
            return Ok(());
        }
        LockView::Held {
            stale: false,
            lock,
            silent_for,
        } => {
            println!(
                "{} is hosting and was heard from {silent_for} s ago; not taking over.",
                lock.holder
            );
            return Ok(());
        }
        LockView::Held { lock, .. } => lock,
    };
    println!(
        "If {} is still in the game, do not take over; ask them to reopen the app. \
         Anything they played after save #{} will be kept only as a side copy.",
        seen.holder, cur.value.generation
    );
    let sid = protocol::new_session_id();
    journal.phase = JournalPhase::Installing;
    journal.session = Some(Session {
        session_id: sid.clone(),
        base_generation: 0,
        acquired_at: None,
        since_written: false,
        lost: false,
    });
    ctx.save(&mut journal)?;
    let outcome = protocol::take_over(&*ctx.store, &ctx.me(), &cfg, &seen, &sid)?;
    match outcome {
        TakeOver::Ok {
            state,
            session,
            previous,
        } => {
            println!("Took over from {}.", previous.holder);
            install_after_lock(ctx, &mut journal, &state.value, session)
        }
        other => {
            journal.reset();
            ctx.save(&mut journal)?;
            match other {
                TakeOver::Alive(l) => println!("{} is back; not taking over.", l.holder),
                TakeOver::Changed(l) => println!("The lock changed meanwhile ({:?}).", l.map(|l| l.holder)),
                _ => println!("No shared world."),
            }
            Ok(())
        }
    }
}

fn recover(ctx: &Ctx) -> Result<()> {
    let mut journal = ctx.journal()?;
    let state = protocol::read_state(&*ctx.store)?.map(|v| v.value);
    let action = protocol::recovery_action(&journal, state.as_ref(), false);
    println!("Recovery: {action:?}");
    match action {
        Recovery::MoveOutIfPresent => {
            if let Some(st) = &state
                && world::world_present(&ctx.paths.world_dir(&st.world_name))
            {
                let cfg = ctx.config()?;
                let sidecars = ctx.paths.sidecars(&st.world_name, &cfg.world_sidecars)?;
                if let Some(m) = world::move_world_out(&ctx.paths, &st.world_name, &sidecars, "not-hosting")? {
                    println!(
                        "{} was on this PC without a session; moved it to {}.",
                        st.world_name,
                        m.backup_dir.display()
                    );
                }
            } else {
                println!("Nothing to do.");
            }
            Ok(())
        }
        Recovery::AbortInstall => {
            if let Some(mut s) = journal.session.clone() {
                match protocol::release(&*ctx.store, &mut s)? {
                    Released::Ok => println!("Released the lock taken by the unfinished install."),
                    Released::NotMine(_) => println!("The lock was not ours."),
                }
            }
            let cache = ctx.paths.cache();
            if let Ok(rd) = std::fs::read_dir(&cache) {
                for e in rd.flatten() {
                    if e.file_name().to_string_lossy().starts_with("install-") {
                        let _ = std::fs::remove_dir_all(e.path());
                    }
                }
            }
            journal.reset();
            ctx.save(&mut journal)?;
            recover(ctx)
        }
        Recovery::ResumeHeartbeat => beat(ctx, Some("in_game")),
        Recovery::Finish | Recovery::LostLock => finish(ctx),
    }
}
