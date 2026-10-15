#!/usr/bin/env bash
#
# Installs REAL dependencies into a testbed built by make-testbed.sh, so
# the artifact folders reach a realistic size (gigabytes rather than
# megabytes).
#
# Usage:
#   scripts/fatten-testbed.sh [target-dir]      # default: ~/tech/dump
#
# This downloads packages from npmjs.org, crates.io and PyPI. It only
# touches a directory carrying make-testbed.sh's own marker file.
#
# IMPORTANT: npm, cargo and pip all write files into the project root
# (package-lock.json, Cargo.lock, pyvenv.cfg, ...). Those are not
# artifact files, so they would make every project look freshly active
# and de-nox would correctly protect everything. The intended ages are
# therefore replayed from .denox-ages at the end. Do not skip that step.

set -uo pipefail

TARGET="${1:-$HOME/tech/dump}"
MARKER=".denox-testbed"
AGES="$TARGET/.denox-ages"

ARTIFACT_NAMES=(
  node_modules target dist build .next .nuxt .output .turbo .cache
  __pycache__ .pytest_cache venv .venv
)

if [ ! -e "$TARGET/$MARKER" ]; then
  echo "'$TARGET' has no $MARKER file -- run scripts/make-testbed.sh first." >&2
  exit 1
fi

if [ ! -f "$AGES" ]; then
  echo "'$AGES' is missing. Rebuild with scripts/make-testbed.sh first," >&2
  echo "otherwise the intended file ages cannot be restored." >&2
  exit 1
fi

log() { printf '\n==> %s\n' "$*"; }

# --- node -------------------------------------------------------------

# Installs into a project, replacing the synthetic node_modules.
npm_install() {
  local proj="$1"
  [ -f "$proj/package.json" ] || return 0
  log "npm install  ${proj#"$TARGET/"}"
  rm -rf "$proj/node_modules"
  ( cd "$proj" && npm install --no-audit --no-fund --loglevel=error ) \
    || echo "    npm install failed in $proj (continuing)"
}

for proj in \
  "$TARGET/01-stale-node-app" \
  "$TARGET/02-active-node-app" \
  "$TARGET/06-borderline-under-30h" \
  "$TARGET/07-borderline-over-34h" \
  "$TARGET/08-nested-artifacts" \
  "$TARGET/10-monorepo" \
  "$TARGET/10-monorepo/packages/stale-pkg" \
  "$TARGET/10-monorepo/packages/active-pkg" \
  "$TARGET/13-old-deps-active-source" \
  "$TARGET/14-large-stale-artifact" \
  "$TARGET/18-stale-nextjs-app"
do
  npm_install "$proj"
done

# 08 needs a genuinely nested node_modules to prove the skip logic. A
# conflicting transitive version produces one naturally.
log "creating a real nested node_modules in 08-nested-artifacts"
nested="$TARGET/08-nested-artifacts/node_modules/nested-demo"
mkdir -p "$nested"
printf '{"name":"nested-demo","version":"1.0.0","dependencies":{"lodash":"^4.17.21"}}\n' \
  > "$nested/package.json"
( cd "$nested" && npm install --no-audit --no-fund --loglevel=error ) \
  || echo "    nested install failed (continuing)"

# --- rust -------------------------------------------------------------

cargo_build() {
  local proj="$1"
  [ -f "$proj/Cargo.toml" ] || return 0
  log "cargo build  ${proj#"$TARGET/"}"
  rm -rf "$proj/target"
  ( cd "$proj" && cargo build --quiet ) \
    || echo "    cargo build failed in $proj (continuing)"
}

cargo_build "$TARGET/03-stale-rust-app"
cargo_build "$TARGET/04-active-rust-app"
cargo_build "$TARGET/20-stale-rust-heavy"

# A release profile alongside debug roughly doubles target/, which is
# exactly what accumulates on a real machine.
cargo_build_release() {
  local proj="$1"
  [ -f "$proj/Cargo.toml" ] || return 0
  log "cargo build --release  ${proj#"$TARGET/"}"
  ( cd "$proj" && cargo build --release --quiet ) \
    || echo "    release build failed in $proj (continuing)"
}

cargo_build_release "$TARGET/03-stale-rust-app"
cargo_build_release "$TARGET/20-stale-rust-heavy"

# --- python -----------------------------------------------------------

log "python venv + pip install  05-stale-python-app"
p="$TARGET/05-stale-python-app"
rm -rf "$p/.venv"
if python3 -m venv "$p/.venv"; then
  "$p/.venv/bin/pip" install --quiet --upgrade pip \
    || echo "    pip self-upgrade failed (continuing)"
  "$p/.venv/bin/pip" install --quiet -r "$p/requirements.txt" \
    || echo "    pip install failed (continuing)"
  # Real bytecode caches, so __pycache__ is not synthetic either.
  "$p/.venv/bin/python" -c "import compileall, sys; compileall.compile_dir('$p/app', quiet=1)" \
    || true
else
  echo "    venv creation failed (continuing)"
fi

log "python venv + pip install  19-stale-datascience-py  (large)"
p="$TARGET/19-stale-datascience-py"
rm -rf "$p/.venv"
if python3 -m venv "$p/.venv"; then
  "$p/.venv/bin/pip" install --quiet --upgrade pip \
    || echo "    pip self-upgrade failed (continuing)"
  "$p/.venv/bin/pip" install --quiet -r "$p/requirements.txt" \
    || echo "    pip install failed (continuing)"
else
  echo "    venv creation failed (continuing)"
fi

log "python venv  15-misc-artifact-names"
p="$TARGET/15-misc-artifact-names"
rm -rf "$p/venv"
if python3 -m venv "$p/venv"; then
  "$p/venv/bin/pip" install --quiet requests \
    || echo "    pip install failed (continuing)"
else
  echo "    venv creation failed (continuing)"
fi

# --- go ---------------------------------------------------------------

# Go caches modules outside the project, so there is no in-project
# artifact directory to grow. 12-deeply-nested keeps a real npm install
# instead, which is what its artifact folder is named for.
log "npm install  12-deeply-nested/a/b/c/d/service"
svc="$TARGET/12-deeply-nested/a/b/c/d/service"
printf '{"name":"deep-service","dependencies":{"express":"^4.19.2","pino":"^9.2.0"}}\n' \
  > "$svc/package.json"
rm -rf "$svc/node_modules"
( cd "$svc" && npm install --no-audit --no-fund --loglevel=error ) \
  || echo "    npm install failed in $svc (continuing)"

# --- restore the intended ages ----------------------------------------

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

apply_dir_age() {
  find "$1" -type f -print0 | xargs -0 -r touch -d "$2 hours ago"
}

log "restoring intended file ages from .denox-ages"
while IFS=$'\t' read -r kind rel hours; do
  [ -n "${kind:-}" ] || continue
  path="$TARGET/$rel"
  [ -e "$path" ] || { echo "    skip (missing): $rel"; continue; }

  case "$kind" in
    P) apply_project_age "$path" "$hours"; echo "    project  $rel -> ${hours}h" ;;
    D) apply_dir_age "$path" "$hours";     echo "    dir      $rel -> ${hours}h" ;;
    *) echo "    unknown record: $kind $rel" ;;
  esac
done < "$AGES"

# The unreadable fixture must stay unreadable.
chmod 000 "$TARGET/16-unreadable-subdir/locked" 2>/dev/null || true

# --- summary ----------------------------------------------------------

total_size="$(du -sh "$TARGET" 2>/dev/null | cut -f1 || true)"
log "done: ${total_size} total"
du -sh "$TARGET"/*/ 2>/dev/null | sort -h | tail -12
echo
echo "test with:  cargo run -- $TARGET     (threshold 32)"
