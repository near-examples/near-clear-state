#!/usr/bin/env bash
#
# Verify that a `state_cleanup.wasm` matches a reproducible build of
# its source.
#
# Usage:
#   ./scripts/verify-wasm.sh [path/to/state_cleanup.wasm]
#
# Default path is `extension/wasm/state_cleanup.wasm` (the wasm
# committed in this repo). Pass another path to verify a copy obtained
# elsewhere (e.g. extracted from an installed binary).
#
# Why this script exists
# ----------------------
# `cargo near build reproducible-wasm` builds the contract inside a
# docker image pinned by digest. Same source + same image => same bytes.
# But the resulting wasm also has the *build-context commit hash* baked
# into its NEP-330 metadata blob (as a github tree URL). That means a
# fresh build at any other commit produces a wasm with a different
# embedded hash, even though the actual code bytes are identical.
#
# To verify reproducibly we therefore have to rebuild *at the same
# commit the supplied wasm was built at*, not at current HEAD. We read
# that commit straight out of the wasm itself — it's the only piece of
# info needed to find the exact source state to rebuild from.
#
# Requires:
#   - docker (cargo-near pulls the pinned sourcescan/cargo-near image)
#   - the embedded commit reachable in this repo's git history

set -euo pipefail

# Resolve repo root regardless of where the script is invoked from.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Optional first arg overrides the wasm path. Default = in-repo copy.
COMMITTED="${1:-$ROOT/extension/wasm/state_cleanup.wasm}"

if [ ! -f "$COMMITTED" ]; then
  echo "Missing $COMMITTED" >&2
  echo "Usage: $0 [path/to/state_cleanup.wasm]" >&2
  exit 1
fi
echo "Verifying: $COMMITTED"

# Step 1 — find the build-context commit.
#
# cargo-near embeds a JSON metadata blob into the wasm that includes a
# github tree URL pointing at the source commit:
#   "https://github.com/<owner>/<repo>/tree/<40-hex-commit>"
#
# `strings` walks the binary and pulls out printable runs; `grep -oE`
# isolates the URL. There can be other 40-hex blobs elsewhere in the
# wasm (cryptographic hashes etc.) so matching against the URL prefix
# is safer than grepping for bare hex.
URL="$(strings "$COMMITTED" \
  | grep -oE 'https://github\.com/[^"]+/tree/[0-9a-f]{40}' \
  | head -n 1 || true)"

if [ -z "$URL" ]; then
  echo "Could not find an embedded source URL in $COMMITTED." >&2
  echo "Was it built with cargo-near reproducible-wasm?" >&2
  exit 1
fi

# `${URL##*/tree/}` strips the longest prefix ending in "/tree/", leaving
# only the 40-char commit hash.
COMMIT="${URL##*/tree/}"
echo "Committed wasm was built at: $URL"

# Step 2 — confirm we have that commit locally.
#
# `git cat-file -e <rev>^{commit}` succeeds if the named commit object
# exists in this repo's object database. If it doesn't, the user needs
# to fetch from origin (or the commit was never pushed anywhere).
cd "$ROOT"
if ! git cat-file -e "$COMMIT^{commit}" 2>/dev/null; then
  echo "Commit $COMMIT is not in this repo's history." >&2
  echo "Try: git fetch origin" >&2
  exit 1
fi

# Step 3 — check out that commit into a throwaway worktree.
#
# We need an on-disk copy of the source at the build-context commit
# without disturbing the user's main working tree (which may have
# uncommitted changes). `git worktree add --detach` materialises a
# detached-HEAD checkout in a separate directory backed by the same
# .git. The EXIT trap removes it so we leave nothing behind.
WORKTREE="$(mktemp -d)"
trap 'git worktree remove --force "$WORKTREE" 2>/dev/null || rm -rf "$WORKTREE"' EXIT

git worktree add --detach "$WORKTREE" "$COMMIT"

# Step 4 — rebuild reproducibly.
#
# Inside the worktree's contract/ directory, run the same reproducible
# build command that originally produced the committed wasm. cargo-near
# spins up the pinned docker image, builds, then optionally runs
# wasm-opt. The resulting wasm should be byte-identical to the
# committed one (including the embedded metadata blob, since we're at
# the same commit).
( cd "$WORKTREE/contract" && cargo near build reproducible-wasm )

# Step 5 — compare sha256s.
#
# EXPECTED = the sha of the pre-built wasm checked into the repo
#            (what a verifier expects to see).
# NEW      = the sha of the wasm we just rebuilt from source in the
#            temp worktree.
FRESH="$WORKTREE/contract/target/near/state_cleanup.wasm"
EXPECTED="$(shasum -a 256 "$COMMITTED" | awk '{print $1}')"
NEW="$(shasum -a 256 "$FRESH"          | awk '{print $1}')"

echo
echo "Expected (committed):      $EXPECTED"
echo "New (rebuilt at $COMMIT):  $NEW"
echo

if [ "$EXPECTED" = "$NEW" ]; then
  echo "wasm matches"
  exit 0
fi

echo "MISMATCH"
exit 1
