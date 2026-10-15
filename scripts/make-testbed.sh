#!/usr/bin/env bash
#
# Builds a disposable tree of fake projects for exercising de-nox.
#
# Every case is named for what it should do at a 32 hour threshold, so
# the expected result is readable straight from the scan output. See
# EXPECTATIONS.md, which this script writes into the testbed.
#
# Usage:
#   scripts/make-testbed.sh [target-dir]      # default: ~/tech/dump
#
# Re-running wipes and rebuilds the target. It refuses to touch a
# directory that does not look like one of its own testbeds.

set -euo pipefail

TARGET="${1:-$HOME/tech/dump}"
THRESHOLD=32
MARKER=".denox-testbed"

ARTIFACT_NAMES=(
  node_modules target dist build .next .nuxt .output .turbo .cache
  __pycache__ .pytest_cache venv .venv
)

# --- guards -----------------------------------------------------------

case "$TARGET" in
  "" | "/" | "$HOME") echo "refusing to use '$TARGET' as a testbed" >&2; exit 1 ;;
esac

if [ -e "$TARGET" ] && [ -n "$(ls -A "$TARGET" 2>/dev/null)" ] && [ ! -e "$TARGET/$MARKER" ]; then
  echo "'$TARGET' is not empty and has no $MARKER file." >&2
  echo "Refusing to wipe a directory this script did not create." >&2
  exit 1
fi

# A deliberately unreadable directory is part of the fixture, so make
# sure a rebuild can still remove it.
if [ -e "$TARGET" ]; then
  chmod -R u+rwX "$TARGET" 2>/dev/null || true
  rm -rf "$TARGET"
fi

mkdir -p "$TARGET"
touch "$TARGET/$MARKER"

# Replay log of every backdating operation, in order. scripts/fatten-testbed.sh
# replays it after installing real dependencies, because npm, cargo and pip all
# write files into the project root (package-lock.json, Cargo.lock, ...) that
# would otherwise make every project look freshly active.
AGES="$TARGET/.denox-ages"
: > "$AGES"

# --- helpers ----------------------------------------------------------

# Backdates every file in a project EXCEPT those inside artifact
# directories. This mirrors how de-nox decides project activity.
apply_project_age() {
  local proj="$1" hours="$2"
  local prune=()
  for name in "${ARTIFACT_NAMES[@]}"; do
    prune+=(-name "$name" -o)
  done
  unset 'prune[${#prune[@]}-1]'

  find "$proj" \( "${prune[@]}" \) -prune -o -type f -print0 \
    | xargs -0 -r touch -d "$hours hours ago"
}

# Backdates files inside one directory, artifacts included.
apply_dir_age() {
  find "$1" -type f -print0 | xargs -0 -r touch -d "$2 hours ago"
}

# Both wrappers record what they did so the operation can be replayed.
set_project_age() {
  apply_project_age "$1" "$2"
  printf 'P\t%s\t%s\n' "${1#"$TARGET/"}" "$2" >> "$AGES"
}

set_dir_age() {
  apply_dir_age "$1" "$2"
  printf 'D\t%s\t%s\n' "${1#"$TARGET/"}" "$2" >> "$AGES"
}

filler() {
  local path="$1" bytes="$2"
  mkdir -p "$(dirname "$path")"
  head -c "$bytes" /dev/urandom > "$path"
}

fake_node_modules() {
  local dir="$1" bytes="${2:-40000}"
  for pkg in left-pad lodash react chalk; do
    mkdir -p "$dir/$pkg"
    echo "{\"name\":\"$pkg\",\"version\":\"1.0.0\"}" > "$dir/$pkg/package.json"
    filler "$dir/$pkg/index.js" "$bytes"
  done
}

# --- the cases --------------------------------------------------------

echo "building testbed in $TARGET"

# 1. Plain stale JS project. node_modules + dist both qualify.
p="$TARGET/01-stale-node-app"
mkdir -p "$p/src"
echo "export const main = () => 1;" > "$p/src/index.js"
cat > "$p/package.json" <<'EOF'
{
  "name": "stale-node-app",
  "version": "1.0.0",
  "private": true,
  "dependencies": {
    "express": "^4.19.2",
    "lodash": "^4.17.21",
    "axios": "^1.7.2",
    "moment": "^2.30.1",
    "chalk": "^5.3.0"
  }
}
EOF
fake_node_modules "$p/node_modules" 60000
mkdir -p "$p/dist"; filler "$p/dist/bundle.js" 120000
set_project_age "$p" 200

# 2. Actively developed JS project. Nothing may be touched.
p="$TARGET/02-active-node-app"
mkdir -p "$p/src"
echo "edited just now" > "$p/src/app.jsx"
cat > "$p/package.json" <<'EOF'
{
  "name": "active-node-app",
  "version": "1.0.0",
  "private": true,
  "dependencies": {
    "react": "^18.3.1",
    "react-dom": "^18.3.1"
  },
  "devDependencies": {
    "vite": "^5.3.1",
    "@vitejs/plugin-react": "^4.3.1",
    "typescript": "^5.5.2"
  }
}
EOF
fake_node_modules "$p/node_modules" 50000
mkdir -p "$p/.next"; filler "$p/.next/build-manifest.json" 30000
set_project_age "$p" 1

# 3. Stale Rust project.
p="$TARGET/03-stale-rust-app"
mkdir -p "$p/src"
echo "fn main() {}" > "$p/src/main.rs"
cat > "$p/Cargo.toml" <<'EOF'
[package]
name = "stale-rust-app"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
clap = { version = "4", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
regex = "1"
chrono = "0.4"
EOF
mkdir -p "$p/target/debug"; filler "$p/target/debug/stale" 900000
set_project_age "$p" 150

# 4. Active Rust project.
p="$TARGET/04-active-rust-app"
mkdir -p "$p/src"
echo "fn main() { println!(\"wip\"); }" > "$p/src/main.rs"
cat > "$p/Cargo.toml" <<'EOF'
[package]
name = "active-rust-app"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
clap = { version = "4", features = ["derive"] }
anyhow = "1"
rand = "0.8"
EOF
mkdir -p "$p/target/debug"; filler "$p/target/debug/active" 800000
set_project_age "$p" 2

# 5. Stale Python project with several artifact kinds at once.
p="$TARGET/05-stale-python-app"
mkdir -p "$p/app"
echo "def main(): pass" > "$p/app/main.py"
cat > "$p/requirements.txt" <<'EOF'
flask
requests
numpy
pandas
EOF
mkdir -p "$p/__pycache__"; filler "$p/__pycache__/main.cpython-311.pyc" 20000
mkdir -p "$p/.pytest_cache/v"; echo '{}' > "$p/.pytest_cache/v/lastfailed"
mkdir -p "$p/.venv/lib/python3.11/site-packages/flask"
filler "$p/.venv/lib/python3.11/site-packages/flask/app.py" 400000
set_project_age "$p" 300

# 6. Just under the threshold: 30h of idle time, protected at 32h.
p="$TARGET/06-borderline-under-30h"
mkdir -p "$p/src"; echo "almost stale" > "$p/src/index.js"
printf '{"name":"borderline-under","dependencies":{"commander":"^12.1.0"}}\n' > "$p/package.json"
fake_node_modules "$p/node_modules" 20000
set_project_age "$p" 30

# 7. Just over the threshold: 34h, qualifies at 32h.
p="$TARGET/07-borderline-over-34h"
mkdir -p "$p/src"; echo "just stale enough" > "$p/src/index.js"
printf '{"name":"borderline-over","dependencies":{"yargs":"^17.7.2"}}\n' > "$p/package.json"
fake_node_modules "$p/node_modules" 20000
set_project_age "$p" 34

# 8. Nested artifacts. Only the outer node_modules should be listed.
p="$TARGET/08-nested-artifacts"
mkdir -p "$p/src"; echo "nested" > "$p/src/index.js"
printf '{"name":"nested-artifacts","dependencies":{"express":"^4.19.2"}}\n' > "$p/package.json"
fake_node_modules "$p/node_modules" 15000
mkdir -p "$p/node_modules/outer-pkg/node_modules/inner-pkg"
filler "$p/node_modules/outer-pkg/node_modules/inner-pkg/index.js" 15000
set_project_age "$p" 180

# 9. A project with no files of its own: no readable timestamp, so the
#    conservative rule must protect it.
p="$TARGET/09-artifact-only-no-source"
mkdir -p "$p"
fake_node_modules "$p/node_modules" 25000
set_project_age "$p" 500

# 10. Monorepo: each package is judged on its own activity.
p="$TARGET/10-monorepo"
mkdir -p "$p/packages/stale-pkg/src" "$p/packages/active-pkg/src"
cat > "$p/package.json" <<'EOF'
{
  "name": "monorepo-root",
  "private": true,
  "dependencies": {
    "lodash": "^4.17.21",
    "chalk": "^5.3.0"
  }
}
EOF
echo "old" > "$p/packages/stale-pkg/src/index.ts"
echo "new" > "$p/packages/active-pkg/src/index.ts"
cat > "$p/packages/stale-pkg/package.json" <<'EOF'
{
  "name": "stale-pkg",
  "version": "1.0.0",
  "dependencies": { "dayjs": "^1.11.11", "nanoid": "^5.0.7" }
}
EOF
cat > "$p/packages/active-pkg/package.json" <<'EOF'
{
  "name": "active-pkg",
  "version": "1.0.0",
  "dependencies": { "zod": "^3.23.8", "uuid": "^10.0.0" }
}
EOF
fake_node_modules "$p/node_modules" 70000
fake_node_modules "$p/packages/stale-pkg/node_modules" 30000
fake_node_modules "$p/packages/active-pkg/node_modules" 30000
mkdir -p "$p/packages/stale-pkg/dist"; filler "$p/packages/stale-pkg/dist/out.js" 40000
mkdir -p "$p/.turbo"; filler "$p/.turbo/cache.log" 10000
set_project_age "$p" 220
# Re-touch only the active package so it stands out from the rest.
set_project_age "$p/packages/active-pkg" 1

# 11. A symlink named like an artifact. It must never be followed and
#     never offered for deletion.
p="$TARGET/11-symlinked-artifact"
mkdir -p "$p/src" "$p/real-modules/pkg"
echo "symlink case" > "$p/src/index.js"
filler "$p/real-modules/pkg/index.js" 20000
ln -s "../real-modules" "$p/node_modules"
set_project_age "$p" 250

# 12. Artifact buried several levels down.
p="$TARGET/12-deeply-nested"
mkdir -p "$p/a/b/c/d/service/src"
echo "deep" > "$p/a/b/c/d/service/src/main.go"
echo "module deep" > "$p/a/b/c/d/service/go.mod"
fake_node_modules "$p/a/b/c/d/service/node_modules" 25000
set_project_age "$p" 170

# 13. The case the old algorithm got wrong: dependencies installed long
#     ago, source edited minutes ago. Must be PROTECTED.
p="$TARGET/13-old-deps-active-source"
mkdir -p "$p/src"
echo "edited moments ago" > "$p/src/index.js"
cat > "$p/package.json" <<'EOF'
{
  "name": "long-running-project",
  "version": "1.0.0",
  "private": true,
  "devDependencies": {
    "webpack": "^5.92.1",
    "webpack-cli": "^5.1.4",
    "@babel/core": "^7.24.7",
    "@babel/preset-env": "^7.24.7",
    "babel-loader": "^9.1.3"
  }
}
EOF
fake_node_modules "$p/node_modules" 80000
set_project_age "$p" 1
set_dir_age "$p/node_modules" 2000   # deps are ~83 days old

# 14. A large stale artifact, so reclaimable size is obvious.
p="$TARGET/14-large-stale-artifact"
mkdir -p "$p/src"; echo "big" > "$p/src/index.js"
cat > "$p/package.json" <<'EOF'
{
  "name": "large-stale-artifact",
  "version": "1.0.0",
  "private": true,
  "devDependencies": {
    "typescript": "^5.5.2",
    "eslint": "^8.57.0",
    "jest": "^29.7.0",
    "prettier": "^3.3.2",
    "ts-node": "^10.9.2"
  }
}
EOF
mkdir -p "$p/node_modules/heavy-pkg"
filler "$p/node_modules/heavy-pkg/bundle.js" 6000000
mkdir -p "$p/build"; filler "$p/build/app.wasm" 2500000
set_project_age "$p" 400

# 15. Less common artifact names, all stale.
p="$TARGET/15-misc-artifact-names"
mkdir -p "$p/src"; echo "misc" > "$p/src/index.js"
for name in .nuxt .output .cache venv; do
  mkdir -p "$p/$name"; filler "$p/$name/data.bin" 30000
done
set_project_age "$p" 260

# 16. A project de-nox cannot read fully. Any I/O error must disqualify
#     it, so this stays protected despite being stale.
p="$TARGET/16-unreadable-subdir"
mkdir -p "$p/src" "$p/locked"
echo "unreadable case" > "$p/src/index.js"
echo "secret" > "$p/locked/file.txt"
fake_node_modules "$p/node_modules" 20000
set_project_age "$p" 280
chmod 000 "$p/locked"

# 18. Heavy stale Next.js app. Real dependency tree, large node_modules.
p="$TARGET/18-stale-nextjs-app"
mkdir -p "$p/app" "$p/public"
echo "export default function Page() { return null }" > "$p/app/page.tsx"
echo "/* global */" > "$p/app/globals.css"
cat > "$p/package.json" <<'EOF'
{
  "name": "stale-nextjs-app",
  "version": "1.0.0",
  "private": true,
  "dependencies": {
    "next": "^14.2.4",
    "react": "^18.3.1",
    "react-dom": "^18.3.1"
  },
  "devDependencies": {
    "typescript": "^5.5.2",
    "@types/react": "^18.3.3",
    "@types/node": "^20.14.9",
    "tailwindcss": "^3.4.4",
    "postcss": "^8.4.39",
    "autoprefixer": "^10.4.19"
  }
}
EOF
mkdir -p "$p/.next/static/chunks" "$p/.next/cache"
filler "$p/.next/static/chunks/main.js" 3000000
filler "$p/.next/cache/build.bin" 5000000
set_project_age "$p" 240

# 19. Heavy stale Python data project. Real venv with scientific stack.
p="$TARGET/19-stale-datascience-py"
mkdir -p "$p/notebooks" "$p/src"
echo "import pandas as pd" > "$p/src/analysis.py"
echo "{}" > "$p/notebooks/explore.ipynb"
cat > "$p/requirements.txt" <<'EOF'
numpy
pandas
scipy
scikit-learn
matplotlib
EOF
set_project_age "$p" 320

# 20. Heavy stale Rust workspace, judged stale while 04 stays active.
p="$TARGET/20-stale-rust-heavy"
mkdir -p "$p/src"
echo "fn main() {}" > "$p/src/main.rs"
cat > "$p/Cargo.toml" <<'EOF'
[package]
name = "stale-rust-heavy"
version = "0.1.0"
edition = "2021"

[dependencies]
reqwest = { version = "0.12", features = ["json", "blocking"] }
tokio = { version = "1", features = ["full"] }
axum = "0.7"
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite"] }
polars = "0.43"
EOF
set_project_age "$p" 350

# 17. A directory with no artifacts at all: should produce no findings.
p="$TARGET/17-clean-project"
mkdir -p "$p/src" "$p/docs"
echo "nothing to clean" > "$p/src/index.js"
echo "# docs" > "$p/docs/readme.md"
set_project_age "$p" 500

# --- expectations -----------------------------------------------------

cat > "$TARGET/EXPECTATIONS.md" <<'EOF'
# de-nox testbed expectations

Generated by `scripts/make-testbed.sh`. Run a Safe Scan on this
directory with a threshold of **32** hours.

| Project | Expected verdict | Why |
| --- | --- | --- |
| 01-stale-node-app | CANDIDATE x2 (`node_modules`, `dist`) | project idle ~200h |
| 02-active-node-app | PROTECTED x2 | project edited ~1h ago |
| 03-stale-rust-app | CANDIDATE (`target`) | idle ~150h |
| 04-active-rust-app | PROTECTED (`target`) | edited ~2h ago |
| 05-stale-python-app | CANDIDATE x3 (`__pycache__`, `.pytest_cache`, `.venv`) | idle ~300h |
| 06-borderline-under-30h | PROTECTED | 30h idle is under the 32h threshold |
| 07-borderline-over-34h | CANDIDATE | 34h idle clears the threshold |
| 08-nested-artifacts | CANDIDATE x1 only | the inner `node_modules` is covered by the outer one and must NOT be listed separately |
| 09-artifact-only-no-source | PROTECTED | no readable timestamp outside the artifact, so the rule refuses to guess |
| 10-monorepo | CANDIDATE for `packages/stale-pkg/node_modules` and `packages/stale-pkg/dist`; PROTECTED for the root `node_modules`, root `.turbo`, and `packages/active-pkg/node_modules` | each artifact is judged by its own parent project. The root counts `packages/active-pkg/src` among its files, so editing any package keeps the root's artifacts protected -- which is what you want in a workspace |
| 11-symlinked-artifact | not listed at all | `node_modules` is a symlink; symlinks are never followed |
| 12-deeply-nested | CANDIDATE | depth does not matter |
| 13-old-deps-active-source | PROTECTED | dependencies are ~83 days old but the source was just edited. This is the case that a naive "how old is node_modules" check gets wrong |
| 14-large-stale-artifact | CANDIDATE x2 (~8.5 MB) | dominates the reclaimable total |
| 15-misc-artifact-names | CANDIDATE x4 (`.nuxt`, `.output`, `.cache`, `venv`) | covers less common names |
| 16-unreadable-subdir | PROTECTED | `locked/` is chmod 000, and any I/O error disqualifies the project |
| 17-clean-project | no findings | contains no artifact directories |
| 18-stale-nextjs-app | CANDIDATE x2 (`node_modules`, `.next`) | idle ~240h, large real dependency tree |
| 19-stale-datascience-py | CANDIDATE (`.venv`) | idle ~320h, real scientific Python stack |
| 20-stale-rust-heavy | CANDIDATE (`target`) | idle ~350h, large real `cargo build` output |

## Totals to expect at a 32h threshold

This generator alone produces small placeholder artifacts (~21 MB in
total). Run `scripts/fatten-testbed.sh` afterwards to install real
dependencies and reach a realistic size.

Measured from an actual scan of a generated + fattened testbed:

```
directories visited : 87
artifact folders    : 32
deletion candidates : 22
reclaimable         : 2.5 GB   (of 2.8 GB on disk)
warnings            : 2        (both from 16-unreadable-subdir/locked)
scan time           : ~113 ms
```

The scan stays fast on a multi-gigabyte tree because it never descends
into an artifact directory.

If your scan shows different verdicts, something changed in the
eligibility rule — check which row above moved.

## Notes

- `16-unreadable-subdir/locked` is intentionally unreadable. To delete
  the testbed by hand, run `chmod -R u+rwX` on it first. The generator
  script already does this when rebuilding.
- Rebuild after any destructive test: `scripts/make-testbed.sh`
EOF

# --- summary ----------------------------------------------------------

echo
# du exits non-zero because of the intentionally unreadable directory,
# which would otherwise trip `set -o pipefail` before this summary.
total_size="$(du -sh "$TARGET" 2>/dev/null | cut -f1 || true)"
tree_count="$(find "$TARGET" -mindepth 1 -maxdepth 1 -type d | wc -l || true)"
echo "done: ${tree_count} project trees, ${total_size} total"
echo "expectations: $TARGET/EXPECTATIONS.md"
echo
echo "test with:  cargo run -- $TARGET     (threshold 32)"
