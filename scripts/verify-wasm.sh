#!/usr/bin/env bash
#
# Verify that the committed `state_cleanup.wasm` is byte-identical to the
# prebuilt artifact NEAR published in core-contracts at the pinned commit.
#
# Usage:
#   ./scripts/verify-wasm.sh [path/to/state_cleanup.wasm]
#
# Default path is `extension/wasm/state_cleanup.wasm` (the wasm committed in
# this repo). Pass another path to verify a copy obtained elsewhere (e.g.
# extracted from an installed binary).
#
# Why this script exists
# ----------------------
# We no longer build the cleanup contract here — we bundle NEAR's prebuilt
# `state-manipulation/res/state_cleanup.wasm`. That artifact has no
# reproducible-build metadata, so we can't rebuild-and-diff it. Instead we
# PIN a specific upstream commit (recorded in the provenance file) and verify
# that the committed wasm matches the bytes upstream published at that commit.
#
# Trust anchor: the COMMIT in the provenance file, reviewable in this repo's
# git history. A reviewer audits that commit's source on github, then this
# script confirms the shipped bytes match it. Checking the committed wasm
# against the SHA256 *in the same file* would be circular (an attacker who
# swaps the wasm also edits the provenance) — so we re-download from the
# pinned commit and compare against that.
#
# Requires:
#   - curl (fetch the pinned upstream artifact)
#   - sha256sum or shasum

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PROVENANCE="$ROOT/extension/wasm/state_cleanup.wasm.provenance"

# Optional first arg overrides the wasm path. Default = in-repo copy.
COMMITTED="${1:-$ROOT/extension/wasm/state_cleanup.wasm}"

if [ ! -f "$COMMITTED" ]; then
  echo "Missing $COMMITTED" >&2
  echo "Usage: $0 [path/to/state_cleanup.wasm]" >&2
  exit 1
fi
if [ ! -f "$PROVENANCE" ]; then
  echo "Missing provenance file: $PROVENANCE" >&2
  exit 1
fi
echo "Verifying: $COMMITTED"

# Read the pin (REPO, COMMIT, SOURCE_PATH, SHA256) from the provenance file.
# shellcheck source=/dev/null
source "$PROVENANCE"
: "${REPO:?provenance file missing REPO}"
: "${COMMIT:?provenance file missing COMMIT}"
: "${SOURCE_PATH:?provenance file missing SOURCE_PATH}"
: "${SHA256:?provenance file missing SHA256}"

if ! [[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "Provenance COMMIT is not a 40-hex commit SHA: $COMMIT" >&2
  exit 1
fi

# Pick a sha256 tool.
if command -v sha256sum >/dev/null 2>&1; then
  SHA256SUM="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
  SHA256SUM="shasum -a 256"
else
  echo "Neither sha256sum nor shasum is available." >&2
  exit 1
fi

# Step 1 — download the upstream artifact AT THE PINNED COMMIT.
RAW_BASE="${REPO/github.com/raw.githubusercontent.com}"
URL="$RAW_BASE/$COMMIT/$SOURCE_PATH"
echo "Pinned upstream: $URL"
UPSTREAM="$(mktemp)"
trap 'rm -f "$UPSTREAM"' EXIT
curl -fSL "$URL" -o "$UPSTREAM"

# Step 2 — compute sha256 of both the committed copy and the pinned download.
COMMITTED_SHA="$($SHA256SUM "$COMMITTED" | awk '{print $1}')"
UPSTREAM_SHA="$($SHA256SUM "$UPSTREAM"  | awk '{print $1}')"

echo
echo "Committed wasm sha256:        $COMMITTED_SHA"
echo "Upstream @ pinned commit:     $UPSTREAM_SHA"
echo "Provenance-recorded sha256:   $SHA256"
echo

# Step 3 — the committed bytes must match what upstream published at the pin.
if [ "$COMMITTED_SHA" != "$UPSTREAM_SHA" ]; then
  echo "MISMATCH: committed wasm differs from upstream at pinned commit $COMMIT" >&2
  exit 1
fi

# Sanity: the recorded sha should also agree (catches a stale provenance edit).
if [ "$SHA256" != "$UPSTREAM_SHA" ]; then
  echo "WARNING: provenance SHA256 ($SHA256) does not match upstream" >&2
  echo "         ($UPSTREAM_SHA). Re-run scripts/update-wasm.sh to refresh." >&2
  exit 1
fi

echo "wasm matches upstream at pinned commit $COMMIT"
exit 0
