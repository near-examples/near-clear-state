# `near-clean-state`

A [near-cli-rs](https://github.com/near/near-cli-rs) extension that wipes a
contract account's on-chain state without deleting the account.

```
near clean-state <account-id> --max-calls 10 network-config testnet sign-with-keychain send
```

Under the hood it deploys a tiny `clean()` contract to the target account
and then function-calls `clean(keys)` for every batch of storage keys, all
in a single transaction.

## Repo layout

| Path | What it is |
|------|------------|
| `contract/` | The `state-cleanup` contract source (near-sdk 5.26.1). Its own `cargo-near` project. |
| `extension/wasm/state_cleanup.wasm` | The reproducibly-built wasm embedded into the extension binary. |
| `extension/src/` | The `near-clean-state` Rust extension (`cargo install near-clean-state`). |
| `scripts/verify-wasm.sh` | Read the embedded build-context commit from the committed wasm, check it out into a temp worktree, rebuild reproducibly, and diff. Requires docker. |

## Build & install

```
cargo install --path extension
```

Add `~/.cargo/bin` to your `PATH` so `near` can find the binary via the
extension-dispatch lookup (`near-${command}`).

## Verifying the bundled wasm

The wasm at `extension/wasm/state_cleanup.wasm` was built reproducibly
with a docker-pinned `cargo-near` toolchain at commit:

> **`a240b4fd852840351a04d18895aa9a27ddafc4f1`**

To audit the source it was built from, look at
[`contract/src/`](https://github.com/PiVortex/contract-cleaner/tree/a240b4fd852840351a04d18895aa9a27ddafc4f1/contract/src)
at that commit. Locally, that's:

```
git show a240b4fd852840351a04d18895aa9a27ddafc4f1:contract/src/lib.rs
```

Then confirm the committed wasm actually corresponds to that source by
rebuilding it (requires docker):

```
./scripts/verify-wasm.sh
```

The script reads the build-context commit out of the wasm's NEP-330
metadata, checks that commit out into a temp worktree, runs
`cargo near build reproducible-wasm` there, and compares the sha256s.

Whenever the bundled wasm is updated, the commit above is updated
along with it.

## What it does

1. Reads every storage entry on the target account via `view_state` RPC.
2. Fetches live `storage_remove_*` gas costs from `EXPERIMENTAL_protocol_config`.
3. Packs keys into batches that each fit inside `(290 Tgas - 10 Tgas) / max_calls`.
4. Builds one transaction with `DeployContract(bundled wasm)` followed by
   N × `FunctionCall("clean", { keys })`, signs it, and sends it.

If `view_state` is capped (FastNEAR / official RPCs return "state is too
large" at ~50 KB), the extension errors out with a message pointing at
`~/.near/config.toml` and recommending a permissive RPC.

## Origin

This repo started as [`PiVortex/contract-cleaner`](https://github.com/PiVortex/contract-cleaner),
a JS tool for the same job. The legacy JS implementation has been removed
in favour of the Rust extension; the git history is preserved.
