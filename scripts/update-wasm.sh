#!/usr/bin/env bash
#
# Refresh the bundled state-cleanup wasm from NEAR's core-contracts repo
# and re-pin the provenance to a chosen upstream commit.
#
# Usage:
#   ./scripts/update-wasm.sh [commit-ish]
#
# With no argument it resolves the upstream `master` branch to a concrete
# commit SHA. Pass an explicit commit SHA / tag to pin something other than
# the current master tip.
#
# What it does
# ------------
#   1. Resolve the requested ref to a concrete 40-hex commit SHA.
#   2. Download state-manipulation/res/state_cleanup.wasm AT THAT COMMIT
#      (never refs/heads/master — we pin immutable bytes, not a branch).
#   3. Write it to extension/wasm/state_cleanup.wasm.
#   4. Rewrite extension/wasm/state_cleanup.wasm.provenance with the new
#      COMMIT + SHA256 so scripts/verify-wasm.sh follows the new pin.
#
# This is NOT run at install time. The committed wasm + provenance are what
# users get; run this deliberately to bump the pin.
#
# Requires: git, curl, sha256sum or shasum.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PROVENANCE="$ROOT/extension/wasm/state_cleanup.wasm.provenance"
WASM_OUT="$ROOT/extension/wasm/state_cleanup.wasm"

# Read the upstream repo + source path from the existing provenance file so
# this script has a single source of truth too.
# shellcheck source=/dev/null
source "$PROVENANCE"
: "${REPO:?provenance file missing REPO}"
: "${SOURCE_PATH:?provenance file missing SOURCE_PATH}"

REF="${1:-master}"

# Pick a sha256 tool.
if command -v sha256sum >/dev/null 2>&1; then
  SHA256="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
  SHA256="shasum -a 256"
else
  echo "Neither sha256sum nor shasum is available." >&2
  exit 1
fi

# Step 1 — resolve the ref to a concrete commit SHA.
# `git ls-remote <repo> <ref>` prints "<sha>\t<ref>"; if <ref> is already a
# full SHA it usually prints nothing, so fall back to using it verbatim
# when it looks like a 40-hex commit.
echo "Resolving $REF in $REPO ..."
NEW_COMMIT="$(git ls-remote "$REPO" "$REF" | awk '{print $1}' | head -n1)"
if [ -z "$NEW_COMMIT" ]; then
  if [[ "$REF" =~ ^[0-9a-f]{40}$ ]]; then
    NEW_COMMIT="$REF"
  else
    echo "Could not resolve '$REF' to a commit in $REPO." >&2
    exit 1
  fi
fi
echo "Pinning commit: $NEW_COMMIT"

# Step 2 — download the wasm AT THAT COMMIT.
# REPO is https://github.com/<owner>/<repo>; rewrite to the raw host.
RAW_BASE="${REPO/github.com/raw.githubusercontent.com}"
URL="$RAW_BASE/$NEW_COMMIT/$SOURCE_PATH"
echo "Downloading: $URL"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT
curl -fSL "$URL" -o "$TMP"

NEW_SHA="$($SHA256 "$TMP" | awk '{print $1}')"
echo "Downloaded $(wc -c < "$TMP") bytes, sha256 $NEW_SHA"

# Step 3 — install the wasm.
cp "$TMP" "$WASM_OUT"

# Step 4 — rewrite the COMMIT + SHA256 lines in the provenance file in place.
# Keep every other line (comments, REPO, SOURCE_PATH) untouched.
tmp_prov="$(mktemp)"
trap 'rm -f "$TMP" "$tmp_prov"' EXIT
while IFS= read -r line; do
  case "$line" in
    COMMIT=*) echo "COMMIT=$NEW_COMMIT" ;;
    SHA256=*) echo "SHA256=$NEW_SHA" ;;
    *)        echo "$line" ;;
  esac
done < "$PROVENANCE" > "$tmp_prov"
mv "$tmp_prov" "$PROVENANCE"

echo
echo "Updated:"
echo "  $WASM_OUT"
echo "  $PROVENANCE  (COMMIT=$NEW_COMMIT, SHA256=$NEW_SHA)"
echo
echo "Now run ./scripts/verify-wasm.sh to confirm, review the diff, and commit."
