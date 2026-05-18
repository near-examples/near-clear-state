# `near-clean-state`

A [near-cli-rs](https://github.com/near/near-cli-rs) extension that wipes a
contract account's on-chain state without deleting the account.

Interactive mode:

```bash
near clean-state
```

Full command: 

```bash
near clean-state <account-id> network-config testnet sign-with-keychain send
```

Under the hood it deploys a tiny `clean()` contract to the target account
and function-calls `clean(keys=[…])` with every key in one transaction.

If the tool errors with `Account state is too large for this RPC's view_state cap`, switch to an RPC with a larger `view_state` cap via `near config edit-connection`. Intear's RPCs are a good option:

- Mainnet: `https://rpc.intea.rs`
- Testnet: `https://testnet-rpc.intea.rs`

## Install

Directly from this GitHub repo:

```
cargo install --git https://github.com/PiVortex/contract-cleaner near-clean-state
```

Or from a local checkout:

```
git clone https://github.com/PiVortex/contract-cleaner
cargo install --path contract-cleaner/extension
```

Both put a `near-clean-state` binary in `~/.cargo/bin/`. As long as that
directory is on your `$PATH` (alongside the `near` binary itself),
`near` resolves `near clean-state …` to this extension via its
`near-${command}` PATH lookup.

You can also invoke it directly — `near-clean-state …` is identical in
behaviour to `near clean-state …`.

### Verifying installation

```
which near-clean-state
near clean-state --help
```

The second command should print the usage block from this extension
(not near-cli-rs's own help).

## Repo layout

| Path | What it is |
|------|------------|
| `contract/` | The `state-cleanup` contract source (near-sdk 5.26.1). Its own `cargo-near` project. |
| `extension/wasm/state_cleanup.wasm` | The reproducibly-built wasm embedded into the extension binary. |
| `extension/src/` | The `near-clean-state` Rust extension. |
| `scripts/verify-wasm.sh` | Read the embedded build-context commit from the committed wasm, check it out into a temp worktree, rebuild reproducibly, and diff. Requires docker. |

This extention intentionally attempts to clean all state in a single transaction to fit with the near-cli-rs model of one command equalling one transaction. It's assumed that any limitations of cleaning the contract will come from RPCs not being able to serve a large enough view_state rather than the call running out of gas.

## Verifying the bundled wasm

The wasm at `extension/wasm/state_cleanup.wasm` was built reproducibly
with a docker-pinned `cargo-near` toolchain at commit:

> **`a240b4fd852840351a04d18895aa9a27ddafc4f1`**

To audit the exact source that produced it, check that commit out:

```
git checkout a240b4fd852840351a04d18895aa9a27ddafc4f1
```

Then inspect the `contract/` directory directly — `src/lib.rs` (the
contract code), `Cargo.toml` (which pins the `near-sdk` version and
the reproducible-build docker image + digest under
`[package.metadata.near.reproducible_build]`), and `rust-toolchain.toml`
(the pinned toolchain). Confirm that what's there matches what you
expect to be running on chain.

When you're done auditing, switch back and run the verify script
(requires docker):

```
git switch -
./scripts/verify-wasm.sh
```

The script reads the build-context commit out of the wasm's NEP-330
metadata, checks that commit out into a throwaway worktree, runs
`cargo near build reproducible-wasm` there, and compares the sha256
of the rebuilt wasm against the committed one.

Whenever the bundled wasm is updated, the commit above is updated
along with it.