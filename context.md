# context.md — de-nox

Compact project index. Read the relevant section, then verify details
against the current source before relying on them.

## What this is

`de-nox` is a terminal UI (TUI) disk cleanup tool written in Rust. It
lets the user browse the filesystem, then run a **read-only** recursive
scan that locates build/dependency artifact directories (`node_modules`,
`target`, `dist`, `.venv`, ...) and reports how stale each one is.

Current state: **Safe Scan and History are fully implemented.** The menu
has four entries: `Safe Scan` (working), `Deep Clean` (declared, not
implemented — shows a notice), `History` (working), and `Quit`.

### The Safe Scan flow

```
Menu > Safe Scan
  -> Browser          navigate to the directory to clean
  -> Shift+H          open the threshold prompt
  -> type hours       e.g. 32
  -> Enter            read-only scan on a worker thread
  -> ScanResults      [CANDIDATE] / [PROTECTED] per artifact folder
  -> Shift+Enter (D)  build the deletion queue
  -> ConfirmDelete    exact paths, total size, red warning
  -> Enter            delete        | Esc  cancel, nothing touched
  -> DeleteReport     per-path [DELETED] / [KEPT] with reasons
```

Everything before `ConfirmDelete` + Enter is read-only.

### The History flow

```
Menu > History
  -> History          one row per past cleanup session, newest first
  -> Enter            open a session
  -> HistoryDetail    every folder it deleted or kept, with reasons
```

A session is recorded every time a deletion runs, including one that
deleted nothing.

## Stack

- Language: Rust, edition 2024 (toolchain in use: rustc 1.99.0)
- TUI: `ratatui` 0.30 over `crossterm` 0.29 (alternate screen, raw mode)
- Filesystem walk: `walkdir` 2.5 (`follow_links(false)` everywhere)
- Concurrency: `std::thread` + `std::sync::mpsc` channel
- `chrono` is used for history timestamps (`Local::now()`).
- Declared but still unused in code: `clap`, `nerd_fonts`, `serde`.
  Nerd Font glyphs are hardcoded as string literals in `file_icon`
  rather than pulled from `nerd_fonts`; argument parsing is manual, so
  `clap` is idle; `serde` is idle because history uses a hand-rolled
  line format instead of JSON. All three are candidates for removal.

## Layout

```
Cargo.toml                 package manifest and dependencies
Cargo.lock                 committed lockfile (binary crate)
.gitignore                 ignores /target only
src/main.rs                entire application (~1680 lines, incl. tests)
scripts/make-testbed.sh    generates a disposable tree of fake projects
scripts/fatten-testbed.sh  installs real dependencies into that tree
context.md                 this file
```

Single-file binary crate. No `lib.rs`, no CI, no build scripts. Tests
live in a `#[cfg(test)] mod tests` at the bottom of `main.rs`.

## src/main.rs map

| Item | Line | Role |
| --- | --- | --- |
| `ARTIFACTS` | 31 | directory names treated as regenerable output |
| `MIN_PATH_COMPONENTS` | 49 | depth floor that keeps `/`, `/home`, `/home/user` undeletable |
| `HISTORY_HEADER` | 53 | version tag on the history log's first line |
| `Entry` | 56 | one row in the file browser |
| `Artifact` | 63 | found artifact: path, `idle_hours`, `size`, `eligible`, `reason` |
| `ScanResult` | 75 | root, threshold, visited dirs, artifacts, errors |
| `ScanResult::candidates` / `reclaimable` | 83 | eligible subset and its total size |
| `ScanProgress` / `ScanMessage` | 93 | scan worker to UI |
| `DeleteProgress` / `DeleteOutcome` / `DeleteMessage` | 105 | delete worker to UI |
| `HistoryEntry` / `HistorySession` | 122 | one recorded cleanup session and its rows |
| `Screen` | 144 | 11-state enum, see below |
| `read_entries` | 158 | one directory, dirs first then case-insensitive name |
| `file_icon` | 192 | extension/name to Nerd Font glyph |
| `format_size` / `format_hours` | 231 | B/KB/MB/GB and `Nh` / `Nd Nh` |
| `is_artifact_dir` / `is_artifact_path` | 251 | membership test against `ARTIFACTS` |
| `project_last_modified` | 268 | **the activity signal** — newest mtime of a project's own files |
| `directory_size` | 314 | recursive byte total |
| `hours_since` | 325 | saturating whole-hour difference |
| `evaluate_artifact` | 331 | idle hours + eligibility + human reason, with a per-project cache |
| `scan_directory` | 389 | scan worker |
| `check_deletable` | 470 | **every deletion guard**, re-run at delete time |
| `delete_artifacts` | 532 | delete worker; records the session |
| `history_path` | 598 | `$XDG_DATA_HOME/de-nox/history.log`, else `~/.local/share/...` |
| `escape_field` / `unescape_field` | 609 | percent-escaping for tabs, newlines and `%` |
| `append_history` / `append_history_to` | 651 | append one session; the `_to` form takes an explicit log path |
| `load_history` / `load_history_from` | 713 | read all sessions, newest first, skipping bad lines |
| `main` | 778 | arg parsing, terminal setup/teardown |
| `run_app` | 803 | the single event loop: state, draw, key handling |
| `draw_worker_panel` | 1782 | shared chrome for the Scanning and Deleting screens |
| `centered_area` | 1854 | percentage-based centered `Rect` |
| `is_quit_key` | 785 | Ctrl+C detection, press-only |
| `mod tests` | 1911 | 22 tests over eligibility, guards, history and the quit key |

`Screen` states: `Menu`, `Browser`, `ThresholdInput`, `Scanning`,
`ScanResults`, `AllDirectories`, `ConfirmDelete`, `Deleting`,
`DeleteReport`, `History`, `HistoryDetail`.

## Architecture notes

**One event loop, one state machine.** `run_app` owns every piece of
mutable state (current dir, entries, list selections, threshold, scan
result, progress) as local variables. There is no `App` struct. The
loop does: drain worker messages, draw per `screen`, then
`event::poll(60ms)` and dispatch keys per `screen`.

**Polling, not blocking.** The 60 ms poll is what allows the spinner and
the progress counters to animate while a scan runs.

**Both long operations run on worker threads.** Scanning and deleting
each spawn a thread and stream progress over an `mpsc` channel; the UI
drains them non-blockingly at the top of the loop. Scanning is
cancellable with Esc (the result is simply abandoned — it is read-only).
Deleting deliberately ignores all input, because interrupting a
`remove_dir_all` would leave a half-removed tree.

**Eligibility is judged by the project, not by the artifact.** This is
the central rule, in `project_last_modified` and `evaluate_artifact`.
For an artifact at `<project>/node_modules`, the tool walks `<project>`
for the newest mtime **while skipping the contents of every artifact
directory**, and sets:

```
eligible = !failed && project_idle_hours >= threshold_hours
```

The reason this matters: npm writes `node_modules` once at install time,
so its own mtimes are old even for a project worked on daily. Judging
the artifact by its own contents therefore flags live projects as
garbage. The project's source files are the real activity signal.

Three conditions force `eligible = false`, by design: an I/O error
anywhere in the project walk, no readable timestamp at all, and idle
time under the threshold. The rule is one-directional — it can
under-report candidates but never over-report them.

The walk is cached per parent directory, because one project usually
holds several artifacts (`node_modules` + `dist` + `.next`).

**Artifacts are never descended into.** `scan_directory` calls
`walker.skip_current_dir()` on every match. An outer `node_modules`
already covers anything nested inside it, so nested artifacts are not
listed separately, and the scan never pays to walk a dependency tree.
This makes the new rule *cheaper* than the old one it replaced.

**Sizes are measured only for candidates.** `directory_size` is called
only when `eligible` is true — walking a live `node_modules` just to
print a number it will never act on is not worth the I/O.

**Deletion guards live in one function.** `check_deletable` is the only
thing standing between a path and `remove_dir_all`, and it is re-run
immediately before each delete rather than trusting the scan-time flag.
It refuses: a name not in `ARTIFACTS`, the scan root itself, any path
outside the scan root, a path with fewer than `MIN_PATH_COMPONENTS`
components, a symlink, a non-directory, and — importantly — a project
that has been modified since the scan. That last check is what makes it
safe to leave the confirmation screen open.

One failure never aborts the batch: it is recorded in
`DeleteOutcome::skipped` with its reason and shown as `[KEPT]` in the
report.

**Ctrl+C is a key event, not a signal.** `enable_raw_mode` stops the
terminal turning Ctrl+C into SIGINT, so without explicit handling the
key does nothing. `is_quit_key` recognises it and the check runs before
the per-screen dispatch, which is what makes it universal. Two details
matter: only `KeyEventKind::Press` counts, since terminals speaking the
kitty keyboard protocol also report releases and a doubled event would
skip the confirm-again guard; and quitting breaks the event loop rather
than calling `exit`, so `main`'s teardown still runs and the terminal is
always restored.

**History is an append-only tab-separated log.** Written to
`$XDG_DATA_HOME/de-nox/history.log`, falling back to
`~/.local/share/de-nox/history.log`. Format:

```
S <TAB> started <TAB> root <TAB> threshold <TAB> deleted <TAB> kept <TAB> freed
E <TAB> D|K     <TAB> path <TAB> size-or-reason
```

`E` rows belong to the preceding `S` row. Deliberate choices:

- **Append-only**, so a crash mid-write can only truncate the tail; it
  can never corrupt an earlier session.
- **Tab-separated, not JSON**, to avoid pulling in `serde_json`. Paths
  and error strings may legally contain tabs and newlines, so
  `escape_field` percent-escapes `%`, tab, LF and CR.
- **Malformed lines are skipped**, not fatal. `load_history` returns a
  count of unreadable lines as a warning shown on the History screen.
- A session is recorded **even when nothing was deleted**, because "I
  ran a cleanup and it kept everything" is useful history.
- A history write failure never hides the deletion result: it lands in
  `DeleteOutcome::history_error` and is shown on the report screen.

`delete_artifacts` takes `history_log: Option<PathBuf>` purely as a test
seam — `None` means the real user location. Without it the tests wrote
fake sessions into the developer's own history log.

**Symlinks are never followed** — `symlink_metadata` in `read_entries`
and in `check_deletable`, `follow_links(false)` on every `WalkDir`.

**Terminal teardown is unconditional.** `main` runs `run_app`, then
always restores the terminal via an immediately-invoked closure, and
returns `result.and(cleanup)` so the app error wins over a cleanup
error. A panic inside `run_app` would still leave the terminal in raw
mode (no panic hook installed).

**Progress messages are throttled.** `scan_directory` reports on every
artifact found and otherwise only every 32nd directory, so the channel
does not carry one `PathBuf` clone per directory on a large tree.

## Known rough edges

- `Deep Clean` is a menu entry with no implementation; selecting it sets
  a notice in the footer.
- The History screen has no way to clear or prune the log; it grows
  without bound. Delete `~/.local/share/de-nox/history.log` by hand.
- History records paths, not contents. It tells you what was removed,
  never how to get it back — recovery means rebuilding or reinstalling.
- **Shift+Enter is not reportable by many terminals.** Without the kitty
  keyboard protocol, crossterm sees a plain Enter. `D` is wired as an
  equivalent fallback, and `a` replaces the old Shift+Alt+Enter for the
  all-directories view. The Shift+Enter binding is kept because it is
  the documented key, but `D` is the one that always works.
- Deletion is permanent — `fs::remove_dir_all`, not a trash/recycle bin.
- The worker-panel gauge is a `Gauge` pinned at `ratio(1.0)` with a
  hand-drawn bar in its label — intentional, to avoid inventing a
  percentage, but the gauge fill itself is meaningless.
- `ScanResults` navigation does not scroll-clamp against the viewport;
  it relies on ratatui's list offset behavior.
- No panic hook, so a panic inside `run_app` leaves the terminal in raw
  mode.
- The `Browser` "Places" panel was removed in the Safe Scan rewrite; the
  browser is now a two-pane list + detail view.
- 4 `clippy::collapsible_if` style warnings remain; no correctness
  warnings. No CI.

## Commands

```
cargo check                 # type check
cargo build                 # debug build
cargo test                  # 22 tests: eligibility, guards, history, quit key
cargo run                   # start in the current directory
cargo run -- /path/to/dir   # start in an explicit directory
cargo fmt
cargo clippy
```

## Manual testbed

Two scripts, run in order:

```
scripts/make-testbed.sh   [dir]   # structure + timestamps  (fast, offline)
scripts/fatten-testbed.sh [dir]   # real dependencies       (slow, network)
```

Default target for both is `~/tech/dump`.

`make-testbed.sh` builds 20 fake project trees covering stale, active,
borderline (30h vs 34h against a 32h threshold), nested, symlinked,
unreadable, artifact-free and monorepo cases, and writes an
`EXPECTATIONS.md` listing the verdict each should get at 32 hours.
On its own it produces placeholder artifacts totalling ~21 MB.

`fatten-testbed.sh` then replaces those placeholders with real
`npm install`, `cargo build` (debug + release) and `python -m venv` +
`pip install` output, reaching ~2.8 GB.

**Why the two scripts are coupled.** npm, cargo and pip all write files
into the *project root* — `package-lock.json`, `Cargo.lock`,
`pyvenv.cfg`. Those are not artifact files, so after installing, every
project's newest mtime is "now" and de-nox correctly protects
everything. `make-testbed.sh` therefore logs every backdating operation
to `.denox-ages` (`P` = project-scoped, `D` = whole directory, replayed
in order), and `fatten-testbed.sh` replays that log as its final step.
Installing without replaying produces a testbed with zero candidates.

Measured on a generated + fattened testbed: 87 directories, 32
artifacts, 22 candidates, 2.5 GB reclaimable, 2 warnings, scan time
~113 ms. The scan stays fast at that size precisely because it never
descends into an artifact directory.

Because Safe Scan deletes for real, regenerate between destructive runs.
`make-testbed.sh` wipes and rebuilds its target and refuses any
non-empty directory lacking its own `.denox-testbed` marker.
`16-unreadable-subdir/locked` is `chmod 000` on purpose, to exercise the
"I/O error disqualifies the project" path; both scripts handle it.

## Automated tests

The tests build real fixture trees under `std::env::temp_dir()` and
backdate mtimes with `File::set_times`, so they exercise the actual
filesystem logic rather than a mock. `delete_removes_only_stale_artifacts_and_reports_the_rest`
performs a real `remove_dir_all` inside its own temp directory.

The positional path argument is read manually via `env::args().nth(1)`
and canonicalized; `clap` is a dependency but is not yet used for
argument parsing.

## Keybindings

**Universal:** `Ctrl+C` quits from any screen. On the `Deleting` screen
it asks for a second press first, because abandoning a running
`remove_dir_all` can leave a partly removed folder.

| Screen | Keys |
| --- | --- |
| Menu | Up/Down, Enter (select), `q` (quit) |
| Browser | Up/Down, Enter (open dir), Backspace (parent), Shift+H (scan), Esc (menu), `q` (quit) |
| ThresholdInput | digits, Backspace, Enter (start scan), Esc (back) |
| Scanning | Esc (cancel) |
| ScanResults | Up/Down, **Shift+Enter or `D`** (delete candidates), `a` (all directories), Esc (back) |
| AllDirectories | Up/Down, Esc (back) |
| ConfirmDelete | **Enter (delete permanently)**, Esc (cancel) |
| Deleting | input ignored, except `Ctrl+C` twice to force quit |
| DeleteReport | Enter/Esc (back to browser), `q` (quit) |
| History | Up/Down, Enter (open session), Esc (menu), `q` (quit) |
| HistoryDetail | Up/Down, Esc (back to sessions) |

## Repository status

Git repo on branch `main` with **no commits yet**. Everything
(`.gitignore`, `Cargo.toml`, `Cargo.lock`, `context.md`, `src/`,
`scripts/`) is untracked. Git user: `soul-abhi`.

## Constraints to respect

- Deletion must stay behind `ConfirmDelete`, and `check_deletable` must
  keep being re-run at delete time. Never widen the `eligible` rule to
  treat unknown timestamps or failed walks as safe.
- Nothing may enter `ARTIFACTS` unless a standard project command can
  regenerate it. A folder holding unrecoverable work does not belong
  there at any threshold.
- `Deep Clean`, when implemented, must not reuse `check_deletable` with
  looser arguments; a more aggressive mode needs its own guards and its
  own confirmation.
- Keep the TUI usable without a Nerd Font-capable terminal in mind:
  glyphs in `file_icon` degrade to blank boxes elsewhere.
- No secrets belong in this file or in the repo.
