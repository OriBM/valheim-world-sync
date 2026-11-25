# Valheim World Sync: build plan

Working title, rename freely. Written 2026-11-25 as the handoff from planning to Claude Code. Owner: Ori.

**Updated 2026-11-25 after the M0 recon** (raw notes in `docs/recon.md`). Main changes:
- Worlds were in Steam Cloud, not `worlds_local` (2.1, 2.2, 5.8, S2).
- A mod keeps world data outside the world folder, so saves now carry "sidecar" files (2.1, 4.2, 4.4, 5.1).
- Gale's CLI flags and how it launches the game are confirmed (2.3, 7.4, S6).
- `game_build` is Steam's build number (4.3, S4).
- "The world is here" means a `_main.*` file is present (5.1, 5.7, S7).
- Moving a world out also takes Valheim's own backups of it (5.1).
- **M1 decisions:**
  - `world_name` moved from `config.json` to `state.json` (4.2, 4.3).
  - Details for acquire, publish and takeover (5.3–5.5).
  - Install checks (S8).
  - A local stand-in for GitHub and a guard for the live tests (10).

## 1. Goal

A small Windows desktop app for a group of friends who share one modded Valheim world and take turns hosting it from their own PCs. Hosting is the normal in-game "Start Server"; the others join through Steam as usual.

The app makes the world travel between PCs safely:

- One shared copy of the world lives in a private GitHub repo.
- Only one person can host at a time, and everyone can see who it is.
- Hosting is one button. When the host closes Valheim, the new save uploads and the lock releases with no further clicks.
- Every failure (crash, power cut, no internet, two people clicking at once) leaves the world recoverable and never silently overwrites newer progress.

The users are not technical. If a flow needs explaining, the flow is wrong.

**Out of scope:** dedicated servers and any networking between players; installing or updating mods (Gale does that); merging saves (not possible, which is why the design is one writer at a time plus history); other games (keep the Valheim-specific values in one module and leave it there).

## 2. Read this first: facts newer than your training data

Do not code any of this from memory. Sources are in section 14, and milestone M0 exists to confirm them on Ori's PC. M0's findings are merged into this section; the raw notes are in `docs/recon.md`.

### 2.1 Valheim 1.0 changed the save format

Valheim 1.0 shipped on 2026-09-09. A world is now a **folder**, `worlds_local\<WorldName>\` for a local world (cloud worlds: see below), not a `<World>.db` + `<World>.fwl` pair. Nearly every guide written before that date describes the old pair.

What hosting-provider docs say is inside the folder, checked against Ori's disk in M0:

- `_main.N.fwl2`, `_main.N.db2`, `_main.N.chunks`, `_main.N.ok`, where `N` counts saves and `.ok` is written last as the save-finished marker. **Confirmed.** A save writes the changed chunks, then `.chunks`, `.db2`, `.fwl2` and finally `.ok`, in about 0.3 s. Quitting the game saves twice, about 1 s apart.
- Additional `*.chunk` files holding world data. **Confirmed**, named `<x>_<y>__<a>_<rev>.chunk`. A save rewrites only the changed chunks, under a new `<rev>`, so **file names change from save to save**. HomeServer has 44 chunks plus the 4 `_main` files, about 15 MB.
- On load, the game keeps the newest complete `_main.N` set and deletes the other `_main` files. The disk agrees: each live folder held exactly one set.
- Valheim's own rolling backups are sibling folders named `<World>_backup_auto-<datetime>`. **Confirmed** as `<World>_backup_auto-YYYYMMDD-HHMMSS`. They can be partial (one held only `_main.0.fwl2`), and the game's save manager can restore them.
- A world saved on a newer game build does not load on an older build.
- A partial copy of a world folder does not load, and the game may generate a fresh world under that name instead.
- Patch 1.0.17 (2026-10-06) fixed a rare bug in which splitting a large chunk file erased objects in part of the world. Treat older builds as unsafe. Ori's PC has run 1.0.30 (network version 42) since 2026-11-25. Before that it ran 1.0.21 (network version 41).

**Where worlds live (M0 correction).** New 1.0 worlds default to **Steam Cloud** storage, `<Steam>\userdata\<account id>\892970\remote\worlds\<World>\`. All four 1.0 worlds on Ori's PC were cloud worlds. For a cloud world, `worlds_local\<World>\` holds only a minimap cache: `cacheMinimapBiome`, `cacheMinimapHeight`, `cacheMinimapMask` and `cacheMinimapMeta`. That cache is the "4 files, 4.27 MB" first recorded for HomeServer. Valheim's **Move to Local** turns a cloud world into a local one. Tested on HomeServer on 2026-11-25:
- It copies the world byte for byte into `worlds_local\<World>\`, replacing the cache-only folder.
- It keeps the save number.
- It deletes the cloud copy.
- It leaves Valheim's backups of that world in the cloud.

**World data outside the world folder (M0 finding).** The One Map To Rule Them All mod keeps the shared map and pins in `IronGate\Valheim\worlds\<World>.one_map_to_rule_them_all.explored` (about 0.5 MB, in the old `worlds\` folder). It loads the file when the world starts and saves it together with the world. A host without it starts with a blank shared map. This plan calls such files **sidecars** (4.2).

Consequences:

- Treat the world folder as an **opaque unit**. Copy, zip and restore the whole folder. Never rename or pick individual files. Reading the file names to check completeness (S7) is fine.
- A world is its folder **plus its sidecars**. They upload, install and move out together.
- "The world is on this PC" means `worlds_local\<World>\` holds a `_main.*` file. A folder holding only `cacheMinimap*` files belongs to a cloud world (S2).
- Worlds are tiny. Upload the whole folder as one zip every time. Do not build incremental sync.

### 2.2 Paths (Windows, Steam version; everyone in the group is on Steam)

| What | Where |
|---|---|
| Game data root (written `IronGate\Valheim` below) | `%USERPROFILE%\AppData\LocalLow\IronGate\Valheim\` |
| Local worlds (the only ones the app manages) | `IronGate\Valheim\worlds_local\` |
| Cloud worlds (the default for new worlds) | `<Steam>\userdata\<account id>\892970\remote\worlds\` |
| Mod sidecars | e.g. `IronGate\Valheim\worlds\<World>.one_map_to_rule_them_all.explored` |
| Admin list | `IronGate\Valheim\adminlist.txt` |
| Game log (version, world loaded, BepInEx lines) | `IronGate\Valheim\Player.log`; the previous run's is `Player-prev.log` |
| Characters (never touch) | `IronGate\Valheim\characters_local\` and Steam Cloud |
| Steam install | `SteamPath` under `HKCU\Software\Valve\Steam`. Other libraries are listed in `<Steam>\steamapps\libraryfolders.vdf` |
| Active Steam account | `ActiveUser` under `HKCU\Software\Valve\Steam\ActiveProcess`. One PC can have several `userdata\<account id>` folders. SteamID64 = account id + 76561197960265728 |
| Game build | `buildid` in `<SteamLibrary>\steamapps\appmanifest_892970.acf` (`26870846` on Ori's PC). An update is waiting when `TargetBuildID` there is nonzero and differs from `buildid` |
| Game version | The `Valheim version: 1.0.30 (network version 42)` line in `Player.log` |
| Gale data (default, user-configurable) | `%APPDATA%\com.kesomannen.gale\`, with profiles under `valheim\profiles\<name>\` |
| Gale executable | `C:\Program Files\Gale\gale.exe` on Ori's PC (a per-machine install). Locate it rather than assume |

### 2.3 Gale (the group's mod manager)

- Everyone launches modded Valheim through Gale. Ori's profile is named `CoopWorld` and already holds the mods.
- Gale is a Tauri app with profiles, profile codes, **synced profiles** (owner pushes, others pull updates), profile shortcuts, a "vanilla launch" option, and a CLI documented on its wiki. Since mid-2026 it can pull Valheim mods from two sources, Thunderstore and Hexium.
- Gale's data directory is user-configurable. Do not hardcode the profile path. The default is `%APPDATA%\com.kesomannen.gale\`. Ori runs Gale 1.23.3.
- **CLI flags, confirmed from the wiki's CLI page (edited 2026-10-22):** `--game <slug>` / `-g`, `--profile <name>` / `-p`, `--install <zip>` / `-i`, `--launch` / `-l`, `--no-gui`. There is **no `--help`**: Gale treats it as a deep link and opens its window. The wiki documents no CLI flag for a vanilla launch.
- **How Gale launches the game (from Gale's log):** it copies `winhttp.dll`, `doorstop_config.ini` and `doorstop_libs` into the game folder, then runs `steam.exe -applaunch 892970 --doorstop-enabled true --doorstop-target-assembly "<profile>\BepInEx\core\BepInEx.Preloader.dll"`. A plain Steam launch afterwards loads **no** mods (confirmed 2026-11-25), so the S6 risk is real. Gale replaces its `latest.log` every time it starts.

### 2.4 Admin IDs

- `adminlist.txt` takes one player ID per line and is read only when the game starts.
- Valheim 1.0 introduced a new player-ID format, and hosting guides disagree on what the list accepts for a Steam player: a bare SteamID64, `Steam_<id64>`, or the new `V_<id>` style.
- M0: Valheim logs and stores each player as `Steam_<id64>`, plus a separate 16-hex-digit ID. No `V_` form appears anywhere on Ori's PC. Which form `adminlist.txt` accepts is still untested.
- **Not verified:** whether `adminlist.txt` has any effect when a player hosts from the game client, as opposed to a dedicated server.

### 2.5 GitHub API behaviour the design depends on

| Fact | Status |
|---|---|
| `PUT /repos/{o}/{r}/contents/{path}` needs `sha` (blob SHA of the file being replaced) to update a file | In the docs |
| A stale `sha` is rejected with 409 | The docs list 409 without explaining it. **M1 must prove it with a test** |
| The docs warn that this endpoint and "Delete a file" conflict when used in parallel and must be used serially. Treat every content write that way | In the docs |
| Reading a file through the contents endpoint is fully supported up to 1 MB | In the docs. Keep `state.json` far below that |
| One permission, Contents (read and write), covers file contents, releases and release assets | In the docs |
| Download an asset with `GET /releases/assets/{id}` and `Accept: application/octet-stream`; handle both 200 and 302 | In the docs. Design rule on top: never forward the token to the redirect target |
| Release assets: under 2 GiB each, no limit on total size or bandwidth. Files committed to a repo are blocked above 100 MiB | In the docs |
| Creating a release sends notifications and can hit secondary rate limits | In the docs. So create **one** release once and only add assets to it |
| An authenticated conditional request answered with 304 does not count against the primary rate limit | In the docs |
| Leave about a second between mutating requests | In the docs |
| A fine-grained token on a personal account may be created with no expiry | GitHub changelog, Oct 2024 |

## 3. Architecture

Two repos, on purpose:

| Repo | Visibility | Holds | Who can write |
|---|---|---|---|
| `valheim-world-sync` (app) | Public | Source code, installer releases, update manifest | Ori only |
| `valheim-world-data` (data) | Private | `config.json`, `state.json`, one Release whose assets are the saves | Everyone, through one shared token |

The shared token must never be able to write to the repo the auto-updater installs from. With this split, a leaked token can damage saves (which have history) but can never run code on anyone's PC.

The client talks to the GitHub REST API over HTTPS. No git binary, and friends need no GitHub account.

Local data per PC, under `%LOCALAPPDATA%\ValheimWorldSync\`:

- `settings.json`: display name, client id, Gale profile path, preferences. The token goes in Windows Credential Manager, not in this file.
- `journal.json`: the current session's phase, so a crash or reboot resumes instead of losing track.
- `backups\`: everything the app ever moved out of `worlds_local` or a sidecar path (keep the last 10).
- `cache\`: downloaded and outgoing zips.

## 4. Data formats

### 4.1 Setup code

What Ori sends each friend once, in a DM: `VWS1.` followed by base64url of `{"owner": "...", "repo": "...", "token": "..."}`. First run asks for this code and a display name. Treat the code as a secret: never log it, never show it again after entry.

### 4.2 `config.json` (data repo, changes rarely)

```json
{
  "schema": 1,
  "min_app_version": "0.1.0",
  "min_game_build": "0",
  "discord_webhook_url": "",
  "admins": [],
  "gale_profile_name": "CoopWorld",
  "world_sidecars": ["worlds/{world}.one_map_to_rule_them_all.explored"],
  "keep_saves": 20,
  "keep_checkpoints": 3,
  "heartbeat_seconds": 300,
  "stale_after_seconds": 900,
  "release_grace_seconds": 60
}
```

Missing fields take these defaults; a missing `config.json` means all defaults. (`world_name` used to be here; M1 moved it to `state.json`, see 4.3.) `admins` holds exact ID strings (section 7.2). `world_sidecars` lists the files outside the world folder that belong to the world (2.1). Each is a path relative to `IronGate\Valheim`, with `{world}` replaced by `world_name`. A listed file that does not exist is skipped. The list lives here so a new mod's file can be added without a new build.

### 4.3 `state.json` (data repo, the single source of truth)

```json
{
  "schema": 1,
  "world_name": "HomeServer",
  "generation": 43,
  "latest": {
    "generation": 43,
    "asset_id": 123456789,
    "asset_name": "save-000043-9f2c.zip",
    "sha256": "0f3a...",
    "size": 1830211,
    "kind": "session",
    "saved_by": "Tomer",
    "saved_at": "2026-11-25T19:40:12Z",
    "game_build": "26870846",
    "game_version": "1.0.30",
    "mods_fingerprint": "sha256:77ab..."
  },
  "history": [],
  "lock": {
    "holder": "Ori",
    "client_id": "c0ffee00-0000-4000-8000-000000000001",
    "session_id": "5e55104e-0000-4000-8000-000000000002",
    "base_generation": 43,
    "since": "2026-11-25T19:55:03Z",
    "phase": "in_game",
    "beat": 17
  }
}
```

- `world_name` is the folder name of the shared world. The first import's create-only write sets it, together with save #1, so one write both elects the importer and fixes the world. Keeping it in `config.json` instead could leave the two files disagreeing after a crash or a race between two imports (M1).
- `lock` is `null` when nobody is hosting.
- Unknown fields are kept when a client rewrites the file, so an older app never drops what a newer one wrote.
- `kind` is one of `import`, `session`, `checkpoint`, `restore`.
- `history` is newest first, entries shaped like `latest`, trimmed to the retention settings.
- `saved_at` and `since` are GitHub's times (the asset's `created_at`, and the commit time returned by the acquiring write). They are for display. Correctness never depends on a clock a client wrote (invariant I6).
- `phase` is one of `preparing`, `ready`, `in_game`, `finishing`.
- `game_build` is Steam's `buildid` for the game (2.2). It only goes up, so compare it as an integer. `game_version` comes from `Player.log` and is for display only.

### 4.4 Saves

All saves are assets on one Release tagged `saves`, created once by the first import.

- Normal save: `save-<generation, 6 digits>-<first 4 hex of session id>.zip`.
- Side copy (section 5.6): `side-<base generation>-<name>-<UTC timestamp>.zip`. Side copies are never referenced by `state.json`; the History screen finds them by listing assets.
- Zip layout: `manifest.json` at the root (generation, saved_by, game_build, game_version, mods_fingerprint, and name, size and SHA-256 of every file), the world under `world/<WorldName>/`, and each sidecar that exists under `sidecars/<its path relative to IronGate\Valheim>`. Nothing extra is ever placed inside the world folder itself.

## 5. Protocol

### 5.1 Invariants

- **I1.** `state.json` changes only by compare-and-swap: read it, remember the blob SHA, modify, PUT with that SHA. On 409, re-read and decide again. Never resend the same body blindly. A 409 can be caused by an unrelated commit, so "re-read and decide" covers both cases.
- **I2.** `latest` changes only in a write made by the current lock holder: the state just read must have `lock.session_id` equal to this client's session.
- **I3.** A save asset is fully uploaded and its hash verified before any state write points at it.
- **I4.** Nothing in `worlds_local` or at a sidecar path is overwritten or deleted. It is moved into the app's `backups\` first.
- **I5.** The shared world exists in `worlds_local` only while this PC holds the lock. That is what stops someone hosting a stale copy by launching the game on their own. "Exists" means the folder holds a `_main.*` file (2.1).
- **I6.** Staleness is judged on GitHub's clock only: the time of the latest commit touching `state.json` compared with the `Date` header of the current response. Never the local clock.
- **I7.** A client that learns it no longer holds the lock stops writing `lock` and `latest` at once. From then on it can only store side copies.
- **I8.** One writer task per client. All GitHub mutations go through it, serially, about a second apart.

**Moving the world out** (used by I5, Cancel, Finish, Lost lock, first import and recovery) moves the following into one dated folder under `backups\`:
- `worlds_local\<World>\`
- the sidecars
- every `worlds_local\<World>_backup*` sibling

The siblings go too because Valheim's save manager can restore them, which would recreate the world outside the lock. Copies left in Steam Cloud are S2's job.

### 5.2 Watching (everyone who is not hosting)

Poll every 30 s while the window is visible and every 120 s in the tray. Suggested: `GET /repos/{o}/{r}/commits?path=state.json&per_page=1` with `If-None-Match`; a 304 means nothing changed and is free. On change, fetch `state.json` through the contents endpoint. The lock is **stale** when it is held and `Date` minus the latest commit time exceeds `stale_after_seconds`.

### 5.3 Host

1. **Preflight, no writes.** Run the checks in section 6. Any failure stops here with a plain-language message.
2. **Acquire.** Read state. If `lock` is not null, show who holds it and stop (or offer takeover if stale, 5.5). Otherwise CAS-write `lock` with a new session id, `base_generation` = current generation, `phase: preparing`. On 409 follow I1; if someone else now holds it, say "<name> just started hosting". The session id is generated and saved in the journal before the write. Then, if the write lands but its answer is lost, the retry finds its own session in `lock` and carries on.
3. Write the journal. Start the heartbeat: every `heartbeat_seconds`, CAS-write `beat + 1` and the current `phase`. The first heartbeat also writes `since`, taken from the commit time GitHub returned for the acquiring write. If a heartbeat finds another session in `lock`, go to 5.6. Keep the heartbeat inside `state.json` even though each one is a commit: nobody reads this repo's git log, and 5.5 depends on heartbeat and takeover contending for the same file.
4. **Install.** Download `latest`, verify SHA-256, extract to a temp folder, verify every file against the zip's manifest. If any part of the world is present, move the world out (5.1, I4). Rename the extracted folder into place and copy the sidecars to their paths. Record the installed manifest in the journal.
5. Merge admins into `adminlist.txt` (7.2). Set `phase: ready`.
6. Show "World is ready" with a **Launch modded** button (7.4) and **Cancel hosting**.
7. When `valheim.exe` appears: `phase: in_game`, run the modded-launch check (6, S6), post to Discord.

**Cancel hosting** is allowed only while the installed files still match the journal's manifest: move the world out (5.1), CAS-write `lock: null`, journal idle.

### 5.4 Finish

Triggered when `valheim.exe` has been gone for 10 s and the world folder has been unchanged for 5 s. Quitting saves twice, about 1 s apart (2.1); this window covers both saves.

1. If the folder and sidecars are byte-identical to what was installed, or to the save this client last uploaded (the journal records both), skip to step 5.
2. Validate the folder (6, S7). On failure do not upload; keep the lock and show the options from S7.
3. Zip, hash, upload the asset, verify, then CAS-write `generation + 1`, `latest`, `history`, `phase: finishing`. The lock is still held. Details settled in M1:
   - The lock is checked before the upload. If it is already lost, the world goes up as a side copy instead.
   - Verification uses the asset's `digest` from GitHub when present; otherwise the asset is downloaded back and hashed. A bad upload is deleted, never referenced.
   - The verified upload is saved in the journal before the state write. If the lock turns out lost at that point, the asset is renamed to a side copy (5.6).
   - If the state write lands but its answer is lost, the retry sees `latest` already pointing at the asset and stops.
4. Prune (5.9).
5. Show "Releasing in 60 s" with a **Keep hosting** button. If Valheim starts again or the button is pressed, return to `in_game` or `ready` with the lock kept.
6. CAS-write `lock: null`. Move the world out (5.1). Post to Discord. Journal idle.

If the upload or a state write fails for network reasons: keep the lock, keep the world where it is, show "Save not uploaded yet. Keep this PC on." and retry with backoff. The journal makes this survive a restart.

### 5.5 Take over a dead session

Offered only when the lock is stale (5.2). The confirmation must say, in plain words: "If <name> is still in the game, do not take over. Ask them to reopen the app. Anything they played after save #N will be kept only as a side copy."

On confirm: CAS-write `lock` with this client's new session and `base_generation` = current generation, then continue from 5.3 step 3. Post to Discord. Because the old host's heartbeat and this write hit the same file, exactly one wins: if the heartbeat lands first, this CAS fails and the re-read shows a live host. M1 makes that re-read concrete: the takeover carries the lock exactly as the user saw it (session and `beat`). Any newer `beat` means the host is alive. This does not depend on GitHub's commit list being up to date, which can lag a write by a few seconds.

### 5.6 Lost lock

When a client finds another session in `lock` while its own journal says it is hosting:

1. Stop the heartbeat. Show a red banner: "<name> took over hosting. Your progress since save #N will be stored as a side copy."
2. When Valheim is closed (or at once if it is not running): zip the world and upload it as a `side-...` asset. Skip the upload if the folder is byte-identical to what was installed or to the save this client last uploaded. Do not touch `state.json`.
3. Move the world out (5.1). Post to Discord. Journal idle.

### 5.7 Recovery on app start

Read the journal before anything else.

| Journal | Lock in state | Valheim | Do |
|---|---|---|---|
| idle | any | any | If the world is here (I5: a `_main.*` file in `worlds_local\<world>`), move it out (5.1) and tell the user. A cache-only folder is left alone for S2 |
| install not finished | mine | any | Delete temp files, CAS-write `lock: null`, journal idle |
| hosting | mine | running | Resume heartbeat, back to `in_game` |
| hosting | mine | not running | Run Finish (5.4) |
| hosting | someone else's, or null | any | Run Lost lock (5.6) |

While the journal is not idle, register the app to start at Windows login so a reboot finishes the upload. Remove the entry when idle.

### 5.8 First import and Restore

- **First import** (no `state.json` yet): the user picks a world. List both local worlds (`worlds_local`) and cloud worlds (2.2). A cloud world is refused with: "In Valheim's world list, select <World> and press Move to Local. Then try again." Refuse the old `.db`/`.fwl` format with the message "Open this world once in Valheim so it converts, then try again." Create the `saves` release if missing, upload generation 1 with `kind: import`, create `config.json` with defaults and `state.json` with `lock: null`. Creating `state.json` without a `sha` fails if someone else created it first; then re-read. The import zip includes the sidecars. After a successful import, move the world out (5.1, I5) and say so on screen.
- **Restore save #K** (lock must be free): acquire the lock, download #K, upload it again as the next generation with `kind: restore`, release. History stays linear and pruning stays simple.

### 5.9 Pruning

After a successful save write, the holder trims `history` to the last `keep_saves` entries of kind `import`, `session` or `restore` plus the last `keep_checkpoints` checkpoints, then deletes `save-*` assets no history entry references. Never delete the asset `latest` points at. Delete `side-*` assets older than 30 days.

## 6. Safety checks

| # | Check | On failure |
|---|---|---|
| S1 | Valheim is not running when Host is pressed | "Close Valheim first" |
| S2 | No world with the shared name in Steam Cloud storage: no `<World>` folder and no old-format `<World>.fwl` in any `<Steam>\userdata\*\892970\remote\worlds\`. Valheim's `<World>_backup*` entries there don't count | Block. Tell the user to use "Move to Local" in Valheim's world list, or remove the cloud copy. Move to Local is safe here: the app moves that local copy out (I4) before installing the latest save |
| S3 | App version is at least `config.min_app_version` | Block hosting, offer the update |
| S4 | Local game build is at least `latest.game_build` and `config.min_game_build`, compared as integers (4.3) | Block: "Update Valheim in Steam first". A newer local build is allowed. Separately, if Steam has an update waiting (2.2), warn without blocking: friends who updated probably can't join, since builds can differ in network version |
| S5 | Mods fingerprint equals `latest.mods_fingerprint` | Dialog listing what differs, with **Host anyway** and **Cancel**. Do not hard-block; Ori may be updating mods on purpose |
| S6 | The running game was started modded from the expected Gale profile | Loud warning plus a Windows notification: "Valheim was started WITHOUT mods. Close it before opening the world." Mark the session; at Finish, ask before the save becomes `latest` and default to a side copy |
| S7 | World folder is complete: for the highest `N`, all four of `_main.N.chunks`, `.db2`, `.fwl2` and `.ok` exist. And the world's size, counting only `_main.*` and `*.chunk` files (the folder can also hold a ~4.5 MB minimap cache), is at least 50% of the previous save | Do not upload automatically. Offer: Retry, Store as side copy, Release without saving |
| S8 | Install integrity: every extracted file matches the zip manifest before the folder is moved into place. The zip may contain only `manifest.json`, `world/<World>/...` and sidecars; anything else, a `..`, or an absolute path fails the check. Sidecars the current config does not list are checked but not written | Abort, release the lock, report |

Why S5 and S6 matter: players report that objects added by a mod are removed from a world when it is loaded without that mod. The fingerprint is the sorted list of `name@version` from each mod's `manifest.json` under the profile's `BepInEx\plugins\`, hashed. For S6, three signals, in order of preference:
1. The `valheim.exe` command line contains `--doorstop-target-assembly` pointing into the expected profile folder. Gale passes it through Steam (2.3); still to confirm on a running game.
2. The profile's `BepInEx\LogOutput.log` is written after the process started.
3. `Player.log` gets `[Message:   BepInEx]` lines within seconds of start.

Signals 1 and 2 also prove it is the right profile; signal 3 only proves "modded". Confirmed 2026-11-25: a plain Steam launch produces neither signal 2 nor signal 3.

## 7. Features around the core

### 7.1 Discord webhook

If `config.discord_webhook_url` is set, post a one-line message on: hosting started, hosting stopped and save uploaded, takeover, side copy stored, restore. A failed post never blocks or delays anything.

### 7.2 Admin list

Before launch, append to `adminlist.txt` every string in `config.admins` that is not already a line in it. Create the file if missing. Never remove or reorder existing lines. The strings are written verbatim: Ori copies his ID exactly as the game shows it (M0: the game uses `Steam_<id64>`), and may list more than one format. This lives in `config.json`, not in the EXE, so changing admins needs no new build. Lowest priority; if M0 shows the list has no effect on player-hosted sessions, drop the feature.

### 7.3 Mods

Mods stay in Gale. Ori shares `CoopWorld` as a Gale synced profile and friends pull updates there. The app's only jobs are S5 and S6, plus a "Mods: in sync" or "Mods: differ" line on the main screen. First run looks in Gale's default folder first (`%APPDATA%\com.kesomannen.gale\valheim\profiles\<gale_profile_name>`), then searches for `profiles\<gale_profile_name>\BepInEx`, with a folder picker as fallback. The fingerprint reads only `BepInEx\plugins\`. The profile also holds `snapshots`, `_state` and other folders that must be ignored.

### 7.4 Launch button

Launches Valheim through Gale with the `CoopWorld` profile: `gale.exe --game valheim --profile <gale_profile_name> --launch --no-gui`. The flags are confirmed from Gale's wiki (2.3); the command has not been run end to end yet. If that cannot be made reliable, replace the button with the text "Now start Valheim from Gale". The app must work either way, since it watches for `valheim.exe` regardless of who started it.

### 7.5 History and restore

A History screen lists `state.history` (number, who, when, kind, size) with **Restore**, plus a separate list of side copies with **Promote to latest** (same mechanics as Restore).

### 7.6 Checkpoints

While `in_game`, when a new complete save set appears and the folder has been quiet for 10 s: copy the world (folder and sidecars) to `cache\`, confirm nothing changed during the copy (same file names, sizes and times before and after; otherwise discard and wait for the next save), zip, upload as `kind: checkpoint`, CAS-write `generation + 1` and `latest`. This bounds what a dead-host takeover can lose to one autosave interval. Build it after the core is solid.

### 7.7 Self-update

The app updates itself from the app repo's releases. Check at start and every few hours; install only while the journal is idle. `config.min_app_version` lets Ori force everyone off a buggy build.

### 7.8 Small things

- Closing the window during a session minimises to the tray; the app keeps running.
- If a response carries a token-expiry header (believed to be `github-authentication-token-expiration`; verify), warn from 14 days out: "Access expires on <date>. Tell Ori."
- On 401: "This setup code no longer works. Ask Ori for a new one." Settings has "Enter a new setup code".

## 8. UI

One window, dark theme by default, English strings kept in one file.

| State | Main screen |
|---|---|
| No world yet | "No shared world yet." Button: **Share a world from this PC** (first import, 5.8) |
| Free | "Nobody is hosting. Last save: #43 by Tomer, 2 h ago." Button: **Host** |
| Someone else hosting | "Tomer is hosting since 21:14 (last signal 2 min ago). Join through Steam." Host disabled |
| Stale | "Tomer's session looks dead (no signal for 22 min)." Button: **Take over** |
| Me, preparing | Progress: taking the lock, downloading, installing |
| Me, ready | "World is ready." Buttons: **Launch modded**, **Cancel hosting** |
| Me, in game | "You are hosting. Close Valheim when you are done." Last checkpoint time |
| Me, finishing | "Uploading save #44", then "Releasing in 60 s" with **Keep hosting** |
| Me, lost lock | Red banner from 5.6 |
| Upload pending | "Save not uploaded yet. Keep this PC on." Button: **Retry now** |

Also: History screen (7.5), Settings (display name, setup code, Gale profile folder, start with Windows), and a first-run screen (setup code, name).

## 9. Stack and packaging

- **Tauri 2**: Rust backend for all file, network and process work; TypeScript frontend with a light framework of your choice. Gale is itself a Tauri app, so every friend's PC already has the WebView2 runtime.
- Per-user installer, no admin rights. Updates through Tauri's updater against the public app repo. Build and publish releases with GitHub Actions.
- The installer is unsigned, so Windows shows an "unknown publisher" warning once. The friend-facing README must show the two clicks to get past it.
- If the Rust toolchain becomes a blocker, Electron is an acceptable fallback. Raise it with Ori before switching.

## 10. Development safety and testability

- **Before any code touches the real save folder,** zip a copy of the whole `IronGate\Valheim` folder to a dated backup and tell Ori where it is. **Done 2026-11-25:** three zips in `%USERPROFILE%\ValheimBackups\` (`docs/recon.md` §0). `IronGate\Valheim` alone was not enough, because the real worlds were in Steam Cloud. Take a fresh backup before the first real-world test.
- Environment overrides so everything can be tested without Valheim:
  - `VWS_GAME_DATA_DIR` stands in for `IronGate\Valheim`. `worlds_local`, sidecars, `adminlist.txt` and `Player.log` all live under it, so it replaces the earlier `VWS_WORLDS_DIR`.
  - `VWS_STEAM_DIR` points at a fake Steam folder for S2 and S4.
  - `VWS_DATA_DIR` and `VWS_GAME_PROCESS` as before.
  - `VWS_REMOTE=dir:<folder>` swaps GitHub for a local stand-in with the same compare-and-swap rules (M1). Two instances can then hand a world back and forth offline.

  Dev builds default to a sandbox game data folder, never the real one.
- A separate scratch data repo and token for tests. Never run tests against the real data repo. The live tests:
  - read `VWS_TEST_REPO` (`owner/name`) and `VWS_TEST_TOKEN`;
  - first delete the repo's `state.json`, `config.json` and save assets;
  - refuse to run unless the repo name contains `scratch` or `test`.
- **Test world: HomeServer.** Tests and dev builds use a copy of it in the sandbox, never Ori's live copy. The copy is the world folder plus `worlds\HomeServer.one_map_to_rule_them_all.explored`, so the sidecar path gets exercised too. Keep the copy out of the app repo: the repo is public, and a save holds players' Steam IDs.
- Two app instances with different `VWS_DATA_DIR` values simulate two friends on one PC.
- Never print or commit a token. Redact it in logs.

## 11. Milestones

| # | Deliverable | Accepted when |
|---|---|---|
| M0 | Recon, no app code. Write `docs/recon.md` | It records: real file names in a world folder before and after a save; how a save in progress looks on disk; backup folder naming; Gale's profile path, CLI flags and how to tell a modded launch; the ID format the game shows; whether `adminlist.txt` works on a player-hosted session. Sections 2 and 6 are corrected to match |
| M1 | Core library with a CLI harness, no UI: GitHub client, CAS state, asset upload and download, zip, install and remove, journal | Automated tests pass against the scratch repo: two simultaneous acquires give exactly one winner; a stale `sha` is rejected; a save is never referenced before its upload is verified; an upload after a lost lock becomes a side copy |
| M2 | Desktop app, happy path: first run, status screen, Host through Finish, Discord | Two instances on one PC hand a sandbox world back and forth ten times with a dummy game process and no manual steps |
| M3 | Failure paths: takeover, lost lock, pending upload, cancel, recovery table | Each row of 5.7 and each of 5.5, 5.6 is exercised by killing the app or cutting the network at each phase; no run loses or overwrites a save |
| M4 | Safety checks S1 to S8 | Each check has a test that triggers it |
| M5 | History and restore, pruning, checkpoints, admin list, launch button | Restore round-trips; pruning never deletes `latest`; a checkpoint taken mid-session installs cleanly on the second instance |
| M6 | Installer, self-update, `min_app_version` gate, README for friends | A fresh Windows user installs, pastes a setup code, and hosts the real world; an update published to the app repo reaches an installed copy |

**M0 status (2026-11-25):** done except the in-game checks in `docs/recon.md` §9: the admin list, and the `valheim.exe` command line under a Gale launch. Neither blocks M1.

**M1 status (2026-11-25):**
- Written: the core library (`crates/vws-core`) and a CLI harness (`crates/vws-cli`, binary `vws`).
- Passing: every acceptance test, against the local stand-in. Each was also checked by breaking the rule it guards and watching it fail.
- Run end to end: a sandbox copy of HomeServer went Ori → Tomer → Ori through the CLI.
- Left: run the same tests against the scratch repo (section 12, steps 1–2) with `cargo test -p vws-core --test live_github -- --ignored --test-threads=1`.

First real-world test: Ori alone, with the backup from section 10 in place, on HomeServer, then one friend. The new shared world is imported only after that (12, step 6).

## 12. One-time setup only Ori can do

1. Create the private data repo with a README so it has a default branch. Create a second scratch repo for tests.
2. Create a fine-grained personal access token: Repository access limited to the data repo, Permissions: Contents, read and write. Choose the expiry (none is allowed on a personal account). Make a second token for the scratch repo.
3. Create a Discord webhook in the channel the group uses and put its URL in `config.json`.
4. Add his own player ID to `config.admins`, copied exactly as Valheim shows it.
5. Share `CoopWorld` with the group as a Gale synced profile.
6. Create the shared world: a new world, started from Gale with `CoopWorld`, so that any mod that adds locations is loaded when the game places them. New worlds start in Steam Cloud (2.1), so before the first import, use Move to Local on it in Valheim's world list. HomeServer, the test world, was moved on 2026-11-25.
7. After M6: send each friend the installer link and the setup code in a DM.

## 13. Defaults picked, and open points

Defaults (say so to change): Tauri; public app repo; English UI; heartbeat 5 min, stale after 15 min; keep 20 saves and 3 checkpoints; 60 s release grace. Added after M0: saves carry the One Map To Rule Them All sidecar; moving the world out also takes Valheim's own backups of it; a pending Steam update warns but does not block.

Open:

- Which world is shared: **settled 2026-11-25.** It will be a **new world**, not created yet (12, step 6). Until then **HomeServer is the test world**, for development and for the first real-world tests. Where this document uses `HomeServer` as an example, it means that test world.
- Whether a Hebrew UI is wanted. Keep strings in one file and avoid layout that breaks right-to-left.
- Whether the admin list works at all for player-hosted sessions, and which ID form it takes (M0, needs an in-game test).
- Gale's CLI flags: settled from the wiki (2.3). One end-to-end launch test remains, and it also settles S6 signal 1.
- Whether loading a local world writes `cacheMinimap*` into its folder. This only affects how S7 counts size.
- Whether other `CoopWorld` mods keep world data outside the world folder. Only One Map To Rule Them All was found. Extra Slots logs a per-world "backup" whose location is unknown.

## 14. Sources

- Valheim 1.0 save format: https://www.gameserverkings.com/knowledge-base/valheim/valheim-save-location/ , https://low.ms/knowledgebase/valheim-save-location-how-to-upload , https://supercraft.host/wiki/valheim/old_format_missing_db_after_1_0/
- Patch 1.0.17: https://www.doomhosting.com/blog/valheim-1-0-17-patch-notes-chunk-bug-dedicated-servers
- Admin list and ID formats: https://low.ms/knowledgebase/valheim-adminlist , https://nodecraft.com/support/games/valheim/adding-admins-to-your-valheim-server , https://berrybyte.net/wiki/games/valheim/admin-console-commands
- Gale changelog: https://thunderstore.io/package/Kesomannen/GaleModManager/changelog
- Gale CLI: https://github.com/Kesomannen/gale/wiki/CLI
- What was found on Ori's PC (files, logs, Gale): `docs/recon.md`
- GitHub contents API: https://docs.github.com/en/rest/repos/contents
- GitHub release assets API: https://docs.github.com/en/rest/releases/assets
- GitHub API best practices (conditional requests, pacing): https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api
- GitHub file and release size limits: https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-large-files-on-github
- Fine-grained tokens without expiry: https://github.blog/changelog/2024-10-18-new-pat-rotation-policies-preview-and-optional-expiration-for-fine-grained-pats/
