#!/usr/bin/env bash
#
# Verify that a published de-nox GitHub release is complete and usable.
#
# Usage:
#   scripts/verify-release.sh [tag]
#
# With no argument, the newest local tag is used. The script checks that every
# expected asset answers with HTTP 200, downloads everything listed in
# sha256.sum, verifies every checksum, unpacks the archive for the host
# architecture, and runs the binary. It exits non-zero on the first problem.
#
# Environment:
#   REPO           owner/name of the repository (default: soul-abhi/De-Nox)
#   VERIFY_TARGET  target triple to unpack and run (default: host arch + musl)
#   GH_TOKEN       optional token used by gh for the metadata check

set -euo pipefail

REPO="${REPO:-soul-abhi/De-Nox}"
TAG="${1:-$(git describe --tags --abbrev=0)}"
BASE="https://github.com/${REPO}/releases/download/${TAG}"
VERSION="${TAG#v}"

TARGETS=(
  x86_64-unknown-linux-gnu
  x86_64-unknown-linux-musl
  aarch64-unknown-linux-gnu
  aarch64-unknown-linux-musl
)

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

note() {
  echo "  $*"
}

# A HEAD request is the cheapest way to prove an asset exists. Fall back to a
# one-byte ranged GET for servers that refuse HEAD. GitHub can take a moment to
# make a freshly uploaded asset reachable, so retry briefly before giving up.
url_status() {
  local url="$1" code
  code="$(curl -sS -o /dev/null -w '%{http_code}' -L --head "$url" || true)"
  if [ "$code" != "200" ]; then
    code="$(curl -sS -o /dev/null -w '%{http_code}' -L --range 0-0 "$url" || true)"
  fi
  printf '%s' "$code"
}

url_exists() {
  local url="$1" attempt code
  for attempt in 1 2 3 4 5; do
    code="$(url_status "$url")"
    case "$code" in
      200 | 206) return 0 ;;
    esac
    [ "$attempt" -lt 5 ] && sleep 3
  done
  return 1
}

echo "Verifying ${REPO} release ${TAG}"
echo "  base URL: ${BASE}"
echo

# --- 1. every expected asset must be present ------------------------------

expected=(
  de-nox-installer.sh
  sha256.sum
  source.tar.gz
  source.tar.gz.sha256
)
for target in "${TARGETS[@]}"; do
  expected+=(
    "de-nox-${target}.tar.xz"
    "de-nox-${target}.tar.xz.sha256"
    "de-nox-${target}-update"
  )
done

echo "Checking asset URLs"
for asset in "${expected[@]}"; do
  url_exists "${BASE}/${asset}" || fail "missing asset: ${BASE}/${asset}"
  note "ok ${asset}"
done
echo

# --- 2. release metadata must agree with the expected asset list -----------

if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  echo "Checking release metadata"
  is_draft="$(gh release view "$TAG" --repo "$REPO" --json isDraft --jq '.isDraft' 2>/dev/null || true)"
  if [ -n "$is_draft" ]; then
    [ "$is_draft" = "false" ] || fail "release ${TAG} is still a draft"
    actual="$(gh release view "$TAG" --repo "$REPO" --json assets --jq '.assets[].name' 2>/dev/null | sort)"
    missing=()
    for asset in "${expected[@]}"; do
      grep -qxF "$asset" <<<"$actual" || missing+=("$asset")
    done
    [ "${#missing[@]}" -eq 0 ] || fail "release metadata is missing: ${missing[*]}"
    note "metadata lists all ${#expected[@]} expected assets and the release is published"
  else
    note "skipped (release not visible to gh; asset URLs were already checked)"
  fi
  echo
fi

# --- 3. download everything covered by sha256.sum and verify it ------------

echo "Downloading checksum manifest"
curl -fsSL --retry 3 --retry-delay 2 --output "${WORKDIR}/sha256.sum" "${BASE}/sha256.sum"
[ -s "${WORKDIR}/sha256.sum" ] || fail "sha256.sum is empty"

mapfile -t checksum_files < <(
  awk '{ name = $2; sub(/^\*/, "", name); sub(/^\.\//, "", name); if (name != "") print name }' "${WORKDIR}/sha256.sum"
)
[ "${#checksum_files[@]}" -gt 0 ] || fail "sha256.sum does not list any files"

echo "Downloading checksummed assets (${#checksum_files[@]} files)"
for name in "${checksum_files[@]}"; do
  case "$name" in
    */*) fail "unexpected path in sha256.sum: ${name}" ;;
  esac
  curl -fsSL --retry 3 --retry-delay 2 --output "${WORKDIR}/${name}" "${BASE}/${name}"
done

echo "Verifying SHA-256 checksums"
( cd "${WORKDIR}" && sha256sum --strict --check sha256.sum )
echo

# --- 4. unpack the host archive and run it --------------------------------

case "$(uname -m)" in
  x86_64 | amd64) host_arch="x86_64" ;;
  aarch64 | arm64) host_arch="aarch64" ;;
  *) fail "unsupported host architecture: $(uname -m)" ;;
esac

HOST_TARGET="${VERIFY_TARGET:-${host_arch}-unknown-linux-musl}"
printf '%s\n' "${TARGETS[@]}" | grep -qxF "${HOST_TARGET}" \
  || fail "unknown VERIFY_TARGET: ${HOST_TARGET}"

echo "Unpacking de-nox-${HOST_TARGET}.tar.xz"
mkdir -p "${WORKDIR}/unpack"
tar -xJf "${WORKDIR}/de-nox-${HOST_TARGET}.tar.xz" -C "${WORKDIR}/unpack"

binary="$(find "${WORKDIR}/unpack" -type f -name de-nox -perm -u+x | head -n 1)"
[ -n "$binary" ] || fail "no executable de-nox binary inside the archive"

reported="$("$binary" --version)"
case "$reported" in
  *"$VERSION"*) note "binary reports: ${reported}" ;;
  *) fail "version mismatch: expected ${VERSION}, binary reported: ${reported}" ;;
esac

echo
echo "PASS: ${TAG} is complete, checksummed, unpackable, and runnable."
