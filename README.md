<div align="center">

# DE-NOX

**A terminal disk-cleanup tool that knows the difference between a dead project and a busy one.**

[![Rust](https://img.shields.io/badge/Rust-2024%20edition-B7410E?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![TUI](https://img.shields.io/badge/TUI-ratatui%200.30-7C3AED?style=for-the-badge)](https://ratatui.rs/)
[![Tests](https://img.shields.io/badge/tests-22%20passing-16A34A?style=for-the-badge)](#development)
[![Platform](https://img.shields.io/badge/platform-Linux-0F172A?style=for-the-badge&logo=linux&logoColor=white)](#requirements)

</div>

---

`node_modules`, `target`, `.venv`, `.next` — build output piles up across every project
you have ever touched, and most of it belongs to work you finished months ago. DE-NOX
finds those folders, tells you how long their project has been dormant, and deletes only
the ones you approve.

It is **read-only until you explicitly confirm a deletion.**

<br>

## The idea

Most cleanup scripts ask *"how old is this `node_modules`?"* — and get the wrong answer.

`npm install` writes those files **once**, at install time. A project you code in every
single day still has a `node_modules` with months-old timestamps. Judge the folder by its
own contents and you will happily delete the dependencies of your main project.

DE-NOX asks a different question:

> **How long has the *project* been untouched?**

For a folder at `<project>/node_modules`, it scans `<project>` for the newest modified
file — **skipping every artifact directory** — and compares that against your threshold.
Your source files are the real signal of whether a project is alive.

```
project idle 10d 4h  >= 32h threshold   ->  [CANDIDATE]   safe to remove
project active 2h ago                   ->  [PROTECTED]   left alone
```

That one change also makes it **fast**: the scan never walks into a dependency tree. A
2.8 GB test corpus scans in **113 ms**.

<br>

## What it looks like

**Scan results** — every artifact folder, its verdict, and the reason for it:

```
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│ DE-NOX | Disk Cleanup Tool                                                               │
└──────────────────────────────────────────────────────────────────────────────────────────┘
┌ Artifact Folders ─────────────────────────────────┐┌ Scan Summary ───────────────────────┐
│>> [CANDIDATE] target (1.2 GB) - idle 14d 6h       ││Root:                                │
│   [CANDIDATE] node_modules (253.9 MB) - idle 10d  ││/home/you/code                       │
│   [CANDIDATE] .venv (411.9 MB) - idle 13d 2h      ││                                     │
│   [CANDIDATE] .next (7.6 MB) - idle 10d 0h        ││Threshold: 32 hours                  │
│   [PROTECTED] node_modules - project active 1h ago││Directories visited: 87              │
│   [PROTECTED] target - project active 2h ago      ││Artifact folders: 32                 │
│   [PROTECTED] node_modules - project has no reada ││Deletion candidates: 22              │
│                                                   ││Reclaimable: 2.5 GB                  │
│                                                   ││Warnings: 0                          │
│                                                   ││                                     │
│                                                   ││Selected:                            │
│                                                   ││/home/you/code/api/target            │
│                                                   ││                                     │
│                                                   ││Project idle: 14d 6h                 │
│                                                   ││Verdict: candidate for deletion      │
└───────────────────────────────────────────────────┘└─────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│ Up/Down | Shift+Enter (or D) Delete | a All Dirs | Esc Back | ^C Quit                    │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

**Confirmation** — nothing is removed until you have seen the exact list:

```
┌ CONFIRM PERMANENT DELETION ──────────────────────────────────────────────┐
│ 22 folder(s) will be permanently deleted, freeing 2.5 GB.                │
│ This cannot be undone and does not use the trash.                        │
│ These are regenerable build folders: reinstall or rebuild                │
│ to get them back.                                                        │
│                                                                          │
│ ┌ Targets ─────────────────────────────────────────────────────────────┐ │
│ │ /home/you/code/api/target                  (1.2 GB)                  │ │
│ │ /home/you/code/web/node_modules            (253.9 MB)                │ │
│ │ /home/you/code/ml/.venv                    (411.9 MB)                │ │
│ │ ...                                                                  │ │
│ └──────────────────────────────────────────────────────────────────────┘ │
│                                                                          │
│ Press Enter to delete permanently.                                       │
│ Press Esc to cancel and keep everything.                                 │
└──────────────────────────────────────────────────────────────────────────┘
```

<br>

## Quick start

```bash
git clone <your-remote> de-nox
cd de-nox
cargo build --release

./target/release/de-nox              # start in the current directory
./target/release/de-nox ~/code       # start somewhere specific
```

Then:

| Step | Key | What happens |
| :--- | :--- | :--- |
| 1 | `Enter` on **Safe Scan** | opens the directory browser |
| 2 | `Up` `Down` `Enter` `Backspace` | navigate to the folder you want to clean |
| 3 | `Shift+H` | prompts for an idle threshold in hours |
| 4 | type `32`, `Enter` | runs a read-only scan on a background thread |
| 5 | review the list | `[CANDIDATE]` vs `[PROTECTED]`, with reasons |
| 6 | `Shift+Enter` or `D` | builds the deletion queue |
| 7 | `Enter` to confirm, `Esc` to cancel | deletes, then shows a per-folder report |

A good starting threshold is **72** (three days). Use **32** if you want to reclaim space
from anything you have not touched since yesterday.

<br>

## Safety

Deletion is permanent — `remove_dir_all`, not a trash bin. Every guard lives in one
function, `check_deletable`, and it is **re-run immediately before each delete** rather
than trusting what the scan decided earlier.

| Guard | Refuses |
| :--- | :--- |
| Name allowlist | anything whose name is not in the artifact list below |
| Scan root | the directory you started the scan from |
| Containment | any path outside the scan root |
| Depth floor | paths under 4 components, so `/`, `/home`, `/home/you` are unreachable |
| Symlinks | a symlinked artifact is never followed and never deleted |
| Type check | anything that is no longer a directory |
| **Re-validation** | **a project modified since the scan ran** |

That last one is why it is safe to leave the confirmation screen open and go read
something else. If you start working on a project in the meantime, its artifacts are
dropped from the queue and reported as `[KEPT]` with the reason.

Three conditions force `[PROTECTED]`, by design:

- idle time under the threshold
- **no readable timestamp** anywhere in the project
- **any I/O error** while reading the project

The rule is one-directional: it can under-report candidates, never over-report them. One
failure never aborts a batch — it is recorded and the rest proceed.

**What counts as an artifact.** Only directories a standard project command can
regenerate:

```
node_modules   target        dist      build     .next     .nuxt   .output
.turbo         .cache        venv      .venv     __pycache__       .pytest_cache
```

Nested artifacts are skipped — an outer `node_modules` already covers anything inside it.

<br>

## History

Every cleanup is recorded, including one that deleted nothing.

```
┌ Cleanup Sessions (newest first) ──────────────────────────────────────┐
│>> 2026-10-15 16:20   22 deleted    0 kept     2.5 GB  code            │
│   2026-10-14 09:11    3 deleted    1 kept   412.0 MB  experiments     │
│   2026-10-11 22:40    0 deleted    6 kept       0 B   work            │
└───────────────────────────────────────────────────────────────────────┘
```

Open a session to see every folder it touched:

```
[DELETED] /home/you/code/api/target                        (1.2 GB)
[DELETED] /home/you/code/web/node_modules                  (253.9 MB)
[KEPT]    /home/you/code/live/node_modules - project was modified 1h ago,
                                             under the 32h threshold
```

The `[KEPT]` rows are the useful half: they make a protection decision auditable long
after the fact.

Stored at `$XDG_DATA_HOME/de-nox/history.log`, falling back to
`~/.local/share/de-nox/history.log`. The format is an append-only tab-separated log, so a
crash mid-write can only ever truncate the tail — it can never corrupt an earlier session.
A malformed line is skipped with a warning rather than discarding the whole file.

> History records **paths, not contents.** It tells you what was removed, never how to
> restore it. Recovery means rebuilding or reinstalling.

<br>

## Keybindings

**Universal:** `Ctrl+C` quits from any screen. On the deleting screen it asks for a second
press first, because abandoning a running `remove_dir_all` can leave a partly removed
folder.

| Screen | Keys |
| :--- | :--- |
| Menu | `Up` `Down`, `Enter` select, `q` quit |
| Browser | `Up` `Down`, `Enter` open, `Backspace` parent, `Shift+H` scan, `Esc` menu |
| Threshold | digits, `Backspace`, `Enter` start, `Esc` cancel |
| Scanning | `Esc` cancel |
| Scan results | `Up` `Down`, `Shift+Enter` or `D` delete, `a` all directories, `Esc` back |
| All directories | `Up` `Down`, `Esc` back |
| Confirm | `Enter` delete permanently, `Esc` cancel |
| Deleting | input ignored, `Ctrl+C` twice to force quit |
| Report | `Enter` / `Esc` back, `q` quit |
| History | `Up` `Down`, `Enter` open session, `Esc` menu |
| Session detail | `Up` `Down`, `Esc` back |

> **On `Shift+Enter`:** most terminals cannot report Shift together with Enter — without
> the kitty keyboard protocol the application only sees a plain Enter. `D` is wired as an
> equivalent and always works.

<br>

## Requirements

| | |
| :--- | :--- |
| Rust | 2024 edition (developed on 1.99) |
| Platform | Linux. Uses only `std` filesystem APIs, so other Unix targets are likely to work but are untested |
| Terminal | a Nerd Font gives the file-type glyphs in the browser; without one they render as blank boxes, and nothing else changes |

Dependencies: `ratatui`, `crossterm`, `walkdir`, `chrono`. No async runtime, no
serialisation framework.

<br>

## Development

```bash
cargo test        # 22 tests
cargo clippy
cargo fmt
cargo run -- ~/some/dir
```

Tests build real fixture trees under the system temp directory and backdate timestamps
with `File::set_times`, so they exercise genuine filesystem behaviour rather than a mock —
including one test that performs a real `remove_dir_all` inside its own temp directory.
Coverage focuses on the parts that can destroy data: the eligibility rule, every deletion
guard, nested-artifact skipping, the history round-trip, and corrupt-log recovery.

### Test corpus

Two scripts build a disposable tree of fake projects:

```bash
scripts/make-testbed.sh   ~/tech/dump    # structure + timestamps  (instant, offline)
scripts/fatten-testbed.sh ~/tech/dump    # real dependencies       (minutes, network)
```

`make-testbed.sh` creates 20 project trees covering stale, active, borderline (30h vs 34h
against a 32h threshold), nested, symlinked, unreadable, artifact-free and monorepo cases,
plus an `EXPECTATIONS.md` listing the verdict each one should receive.
`fatten-testbed.sh` then runs real `npm install`, `cargo build` and `pip install` to grow
it to roughly 2.8 GB.

The two are coupled on purpose: npm, cargo and pip all write files into the *project root*
(`package-lock.json`, `Cargo.lock`, `pyvenv.cfg`), which would make every project look
freshly active. `make-testbed.sh` logs each backdating step to `.denox-ages` and
`fatten-testbed.sh` replays it afterwards. Installing without replaying produces a testbed
with zero candidates.

Regenerate between destructive runs. The generator refuses any non-empty directory that
does not carry its own marker file.

<br>

## Status

| Feature | State |
| :--- | :--- |
| **Safe Scan** — browse, scan, review, confirm, delete | working |
| **History** — past sessions and what each one touched | working |
| **Deep Clean** | declared in the menu, not implemented |

Known gaps:

- No way to prune the history log from inside the application; delete the file by hand.
- No panic hook, so a panic would leave the terminal in raw mode.
- `clap`, `serde` and `nerd_fonts` are declared in `Cargo.toml` but unused — argument
  parsing is manual, history uses a hand-rolled line format, and the glyphs are string
  literals. All three are candidates for removal.

A future Deep Clean must not reuse `check_deletable` with looser arguments. A more
aggressive mode needs its own guards and its own confirmation.

<br>

## License

No license has been declared for this project yet.

<div align="center">
<br>
<sub>Nothing is deleted without your explicit confirmation.</sub>
</div>
