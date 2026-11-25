# M0 recon (2026-11-25, Ori's PC)

Read-only inspection of the disk, logs and Gale; no app code. Items marked **NEEDS ORI** need someone in the game.
SteamIDs are redacted (`<id64>`) because this file will live in the public app repo.

## 0. Backups taken (section 10)

All in `%USERPROFILE%\ValheimBackups\`, each checked so the zip's file count matches the source:

| Zip | Source | Size |
|---|---|---|
| `IronGate-Valheim-2026-11-25.zip` | `%USERPROFILE%\AppData\LocalLow\IronGate\Valheim` (79 files) | 198 MB |
| `SteamCloud-892970-<account A>-2026-11-25.zip` | `<Steam>\userdata\<account A>\892970\remote` (257 files, **this holds the real 1.0 worlds**) | 461 MB |
| `SteamCloud-892970-<account B>-2026-11-25.zip` | `<Steam>\userdata\<account B>\892970\remote` (59 files, an older Steam account, pre-1.0 worlds only) | 76 MB |

## 1. Biggest finding: the worlds live in Steam Cloud, not `worlds_local`

All four 1.0-format worlds on this PC (`HomeServer`, `oritomer`, `hcrun`, `zagoriserver`) are **cloud worlds**, stored in
`C:\Program Files (x86)\Steam\userdata\<account A>\892970\remote\worlds\<World>\`.

`worlds_local\<World>\` holds only a minimap cache, the same four files for every world:
`cacheMinimapBiome`, `cacheMinimapHeight` (~3.8 MB), `cacheMinimapMask`, `cacheMinimapMeta` (8 bytes).
The plan's "HomeServer = 4 files, 4.27 MB" is that cache. It is not world data.

The log confirms where saves go: `Cloud Save: 329363 bytes. /worlds/oritomer/1e_20__1_9.chunk`, then `Steam writing file chunks`.

Consequences for the plan:

- **S2 as written would block every world the group has.** First import has to begin with the world moved to local (Valheim's world list → "Move to Local"), or the app has to read from the cloud folder. Recommendation: keep the plan's local-only design. First import detects a cloud world and tells the user to press "Move to Local" first. Never write into `userdata\...\remote` ourselves, because Steam syncs it behind our back.
- **Move to Local, as observed on HomeServer (2026-11-25 13:22):**
  - It copied all 48 files (44 chunks and `_main.8.*`) into `worlds_local\HomeServer\`. The copy is byte for byte, checked by SHA-256 against this morning's backup.
  - The save number stayed the same (`_main.8`).
  - It **replaced** the cache-only folder: the 4 `cacheMinimap*` files are gone.
  - It deleted `remote\worlds\HomeServer\` from Steam Cloud but **left Valheim's HomeServer backups in the cloud**: the `HomeServer_backup_auto-20260911-230456` folder and four old `.db/.fwl` backups. That matches the "5 backups" the world list shows.
  - It also deleted `IronGate\Valheim\cache\HomeServer_biomedatacache.bin`.
  - The log line for it says the opposite of what happened: `Copying world into cloud succeeded.`
- **"The world folder exists in worlds_local" is not a test for "the world is here".** The cache folder exists for cloud worlds too. I5, 5.7 row 1 and S2 must test for a complete `_main.N` set (below), not for the folder.
- A copy of the cache is harmless but useless. Decide whether to exclude `cacheMinimap*` from zips. I lean towards keeping the folder opaque and including it, since it is small.
- **Several Steam accounts can exist on one PC** (this one has two). The active one is `HKCU\Software\Valve\Steam\ActiveProcess\ActiveUser` (here `<account A>`). S2 should check every `userdata\*\892970\remote\worlds\` for the shared name, not just the active one. SteamID64 = account id + 76561197960265728.

## 2. What a 1.0 world folder contains

Example, `HomeServer`, 4 `_main` files and 44 chunks:

```
00_01__0_1.chunk   0c_20__2_1.chunk ... 1c_20__2_7.chunk ... 30_1e__1_1.chunk
_main.8.chunks  (494 B)   _main.8.db2  (163 KB)   _main.8.fwl2 (105 B)   _main.8.ok (4 B)
```

- Chunk files are named `<x hex>_<y hex>__<a>_<rev>.chunk`. On each save the game rewrites only the dirty chunks, under a new `<rev>`, so the old file goes away: `1c_20__2_3` in the backup became `1c_20__2_7` in the live folder. **File names change on every save.** That backs up the "opaque unit, never pick individual files" rule.
- Only one `_main.N` set was present in each live folder.
- `_main.N.ok` is 4 bytes, `29 00 00 00` = 41, the same as the game's "network version 41". `_main.N.fwl2` starts with the world name and contains the creator's ID, `Steam_<id64>`.
- Sizes: `oritomer` is about 2.5 MB, `HomeServer` about 16 MB. Small, so the plan's whole-zip upload holds up.

### Save sequence (from `Player.log`, Valheim 1.0.21)

```
World save (1/5) Cloud & Backup checks done => Save number 20
Cloud Save: /worlds/oritomer/1e_20__1_9.chunk        <- dirty chunks
Cloud Save: /worlds/oritomer/_main.20.chunks
World save (2/5) Chunks writing done
Cloud Save: /worlds/oritomer/_main.20.db2            (3/5)
Cloud Save: /worlds/oritomer/_main.20.fwl2           (4/5)
Cloud Save: /worlds/oritomer/_main.20.ok             (5/5) done. Total time [331ms]
Game - OnApplicationQuit
... a second full save, number 21, about 1 s later, during ZNet Shutdown
```

- `.ok` is written last. That confirms the plan's completeness rule for S7: **the newest N has all four `_main.N.{chunks,db2,fwl2,ok}`**.
- **Quitting saves twice** (N and N+1 within about 1 s). The 5.4 trigger (process gone 10 s, folder quiet 5 s) covers it.
- A save takes about 0.3 s, so the 10 s quiet window for checkpoints (7.6) is generous.
- On load the log prints `ZNet.LoadWorld: oritomer (oritomer), save number 19`. That line can tell us which world the host actually opened (useful for S6 and for "wrong world opened" warnings).

### Backup folders

- 1.0: sibling **folders** `<World>_backup_auto-YYYYMMDD-HHMMSS` (note the dash), e.g. `HomeServer_backup_auto-20260911-230456\`, holding a full world.
- One backup folder held only `_main.0.fwl2` (`oritomer_backup_auto-20261114-204243`), so **backups can be partial**. Never restore from one without checking the `_main` set.
- Before 1.0: `<World>_backup_auto-YYYYMMDDHHMMSS.db/.fwl` (no dash) and `<World>_backup_YYYYMMDD-HHMMSS.db/.fwl`.
- The game's rule, from the log: `short time: 7200, long time: 43200, backup count: 3`, and it skips when "World session not long enough".
- Backups sit next to the world, inside Steam Cloud for cloud worlds.

## 3. Mod data stored OUTSIDE the world folder

Plan 2.1 treats the world folder as the whole world. That is not true with this mod list:

- **One Map To Rule Them All** keeps the shared map and pins in
  `IronGate\Valheim\worlds\<World>.one_map_to_rule_them_all.explored` (about 527 KB, in the old *local* `worlds\` folder, not in `worlds_local` and not in Steam Cloud). It loads this file when the server starts and saves it on shutdown. Without it, the next host's world has no shared map or pins.
- **Extra Slots** logs `Extra slots backup saved ..., world oritomer`. Where it writes is not found yet; it may live inside the character file.

Proposal: add `config.world_sidecars`, a list of path patterns relative to `IronGate\Valheim` with `{world}` substituted (start with `worlds/{world}.one_map_to_rule_them_all.explored`). Zip them under `sidecars/` and install them with the same I4 move-to-backups rule. **Decision for Ori.**

## 4. Game build

`C:\Program Files (x86)\Steam\steamapps\appmanifest_892970.acf`:

```
"buildid"        "26249040"
"TargetBuildID"  "26870846"
```

- `buildid` is Steam's build number, which only goes up. It is not a date. The plan's example `"20261006"` should become something like `"26249040"`. Compare the values as integers.
- `TargetBuildID` ≠ `buildid` meant **an update was waiting**.
- **Update, 13:21 the same day:** starting the game let Steam install that update. Now:
  - `buildid` = `26870846`, `TargetBuildID` = `0`, `StateFlags` = `4` (fully installed);
  - the log says `Valheim version: 1.0.30 (network version 42)`.

  So a pending update shows as a nonzero `TargetBuildID` that differs from `buildid`. The network version changed (41 → 42), so players on different builds most likely can't join each other. PLAN.md makes this a warning in S4, not a block.
- The log line for the in-game version: `Valheim version: 1.0.21 (network version 41)`. The plan says 1.0.17+ is the safe floor; this PC qualifies. Steam build to version mapping is not recorded, so I suggest storing both `game_build` (Steam buildid) and `game_version` (from the log) in each save.
- Single Steam library here: `C:\Program Files (x86)\Steam`. Others must be read from `steamapps\libraryfolders.vdf`.

## 5. Player IDs (section 2.4)

The game logs each player as `Steam_<id64>` plus a 16-hex-digit ID, e.g.
`Player history entry with index 0:  Axcel (Steam_<id64>, B34F...AAB7)`.
`_main.N.fwl2` also stores `Steam_<id64>`. So **`Steam_<id64>` is the format the game uses internally in 1.0.21**. No `V_<id>` form appears anywhere.

`adminlist.txt` currently holds only the header comment `// List admin players ID  ONE per line`. The log shows the game reads it at startup (`...Valheim\adminlist.txt (40)`).

**NEEDS ORI:** put `Steam_<your id64>` on line 2 (and the bare id64 on line 3), host from the client, open the console (F5), try an admin command such as `ban`/`kick` on nobody. Report whether it says you are not an admin. If it has no effect, drop 7.2 as the plan says.

## 6. Gale

| What | Found |
|---|---|
| Version | 1.23.3 |
| Executable | `C:\Program Files\Gale\gale.exe` (**per-machine install**, not per-user) |
| Data dir (default, Windows) | `%APPDATA%\com.kesomannen.gale\` (`data.sqlite3`, `latest.log`, `valheim\`) |
| Profiles | `%APPDATA%\com.kesomannen.gale\valheim\profiles\<name>\`; here `CoopWorld` and `Default` |
| Mods | `CoopWorld\BepInEx\plugins\<Author-Name>\manifest.json`; 50 manifests found |
| Other folders in the profile | `snapshots`, `_state`, `_disabled_by_codex`. The S5 fingerprint must read only `BepInEx\plugins\` |

### CLI (from the Gale wiki's CLI page, edited 2026-10-22)

| Flag | Meaning |
|---|---|
| `--game <slug>` / `-g` | Active game, Thunderstore slug (`valheim`) |
| `--profile <name>` / `-p` | Active profile |
| `--install <path>` / `-i` | Install a local mod zip |
| `--launch` / `-l` | Launch the active game with the active profile |
| `--no-gui` | Exit after the arguments are handled |

So the launch button can run `gale.exe --game valheim --profile CoopWorld --launch --no-gui`. **Not run yet**, because it would start the game. There is **no `--help`**: I tried it and Gale treated it as a deep link (`unsupported deep link protocol: --help`) and opened its window; I closed it. The wiki documents no "vanilla launch" option.

### How Gale launches, and the S6 signal

From `latest.log`, Gale copies `winhttp.dll`, `doorstop_config.ini` and `doorstop_libs` into the game folder, then runs:

```
"C:\Program Files (x86)\Steam\steam.exe" -applaunch 892970 --doorstop-enabled true
    --doorstop-target-assembly "C:\Users\...\com.kesomannen.gale\valheim\profiles\CoopWorld\BepInEx\core\BepInEx.Preloader.dll"
```

- **S6 signal 1 (preferred):** `valheim.exe`'s command line should contain `--doorstop-target-assembly` pointing into the expected profile. That answers both "modded?" and "which profile?". **NEEDS a check while Valheim runs after a Gale launch:** `Get-CimInstance Win32_Process -Filter "Name='valheim.exe'" | Select CommandLine`.
- **S6 signal 2 (fallback):** `CoopWorld\BepInEx\LogOutput.log` gets a new modified time after launch (it was last written 01:44:00 on 11/15, the second the game quit). Its line 1 starts `BepInEx 5.4.23.6 - valheim`.
- The game folder's own `doorstop_config.ini` has `enabled = true`, `target_assembly=BepInEx\core\BepInEx.Preloader.dll` (relative, no `BepInEx` in the game folder). So a plain Steam launch loads no mods, which is the S6 danger case. **Confirmed 2026-11-25:** the plain Steam launch used for Move to Local wrote no `BepInEx` lines to `Player.log` and left `CoopWorld\BepInEx\LogOutput.log` untouched.
- **S6 signal 3:** a modded run's `Player.log` has `[Message:   BepInEx] BepInEx 5.4.23.6 - valheim` near the top, written within seconds of start. It shows "modded or not" but not which profile.
- Gale's `latest.log` is replaced on every Gale start, so it is no good as a long-term signal.

## 7. Other things noticed

- The world played most recently is **`oritomer`** (last save 2026-11-15, players Axcel and Tomer), not `HomeServer` (last save 2026-09-11). **Settled 2026-11-25:** the shared world will be a new world. HomeServer is the test world until then.
- The Steam Cloud folder also holds `characters\*.fch`. The app must never touch it (plan 2.2 already says so).
- `worlds\steam_autocloud.vdf` sits in the old local `worlds\` folder. Leave it alone.

## 8. Corrections to make in PLAN.md

| Section | Change |
|---|---|
| 2.1 | Worlds made in 1.0 default to Steam Cloud storage. `worlds_local\<World>` can be cache-only. Chunk files are named `x_y__a_rev.chunk` and get new names on each save. Quitting saves twice. Backups can be partial |
| 2.1 / 4.4 | Add mod sidecar files (`*.one_map_to_rule_them_all.explored`) to what a save carries |
| 2.2 | Add the Steam Cloud worlds path as the *default* location, plus `ActiveUser` and the multi-account note. Gale data: `%APPDATA%\com.kesomannen.gale` |
| 2.3 | CLI flags confirmed from the wiki (above); `--help` does not exist. Gale installs per-machine |
| 2.4 | The game uses `Steam_<id64>` |
| 4.3 | `game_build` is the Steam buildid (`26249040`); add `game_version` (`1.0.21`) |
| 5.7 / I5 / S2 | Decide "world present" by a complete `_main.N` set, not by the folder existing |
| 5.8 | First import must handle a cloud world: ask for "Move to Local", then continue |
| S4 | Also warn (not block) when `TargetBuildID` is nonzero and ≠ `buildid` |
| S6 | Use the `--doorstop-target-assembly` command-line check, with LogOutput.log as fallback |
| S7 | Completeness = all four `_main.N.{chunks,db2,fwl2,ok}` present for the highest N |

These corrections are applied in `PLAN.md` at the repo root (2026-11-25). The original in Downloads is unchanged.

## 9. Still open (needs someone in the game)

1. Whether `adminlist.txt` works when hosting from the client, and which ID form it accepts (§5).
2. The `valheim.exe` command line during a Gale launch (§6). The plain Steam case is settled.
3. Whether loading a *local* world writes `cacheMinimap*` into its folder. HomeServer has not been loaded since the move. This only affects how S7 counts size.
4. How a save in progress looks on disk. The log shows the order, but no snapshot was taken mid-save (a save lasts only about 0.3 s, so low priority).
5. Settled: the shared world will be a new world. HomeServer is the test world until then.
6. Whether to carry mod sidecars in saves (§3). PLAN.md now does so by default.
