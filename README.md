# `near-clear-state`

A [near-cli-rs](https://github.com/near/near-cli-rs) extension that wipes a
contract account's on-chain state without deleting the account.

Before using this tool review the [NOTICE.txt](./NOTICE.txt) file.

Interactive mode:

```bash
near clear-state
```

Full command: 

```bash
near clear-state <account-id> network-config testnet sign-with-keychain send
```

Under the hood it deploys a tiny `clean()` contract to the target account
and function-calls `clean(keys=[…])` with every key in one transaction. 
Note, it does not remove the cleaning contract it is left on the account
until the user deploys a new contract.

If the tool errors with `Account state is too large for this RPC's view_state cap`, switch to an RPC with a larger `view_state` cap via `near config edit-connection`. Intear's RPCs are a good option:

- Mainnet: `https://rpc.intea.rs`
- Testnet: `https://testnet-rpc.intea.rs`

## Limitations

One-shot wipe in a single transaction, so bounded by three ceilings:

- **RPC `view_state` cap** — most public RPCs return at most ~50 KB; try using a different RPC (Intear above) for larger state.
- **Gas budget** — the full `max_total_prepaid_gas` (currently 1000 Tgas) is attached to the single `clean()` call; fits ~13–14k typical entries (a few MB of state) including a +30% safety factor on the estimate.
- **`max_transaction_size = 1.5 MB`** — caps the serialized tx; with the ~104 KB wasm, that leaves room for ~46k small keys (gas hits first in practice).

## Install

### Prerequisites

- [`near-cli-rs`](https://github.com/near/near-cli-rs) must already be installed and on your `$PATH` — this is an extension to it, not a standalone tool.
- A Rust toolchain **≥ 1.88** (the extension is developed against 1.92). `cargo install` builds with your *default* toolchain and ignores the repo's `rust-toolchain.toml`, so if your default is older, install a newer one and prefix the commands below with it — e.g. `rustup toolchain install 1.92`, then `cargo +1.92 install …`.

### Installing the extension

Directly from this GitHub repo:

```
cargo install --locked --git https://github.com/near-examples/near-clear-state near-clear-state
```

Or from a local checkout:

```
git clone https://github.com/near-examples/near-clear-state
cargo install --locked --path near-clear-state/extension
```

`--locked` installs the exact dependency versions pinned in the committed
`Cargo.lock`. Without it `cargo install` re-resolves to the latest compatible
crates, some of which raise their minimum supported Rust version over time and
will then fail to build on an otherwise-fine toolchain.

Both put a `near-clear-state` binary in `~/.cargo/bin/`. As long as that
directory is on your `$PATH` (alongside the `near` binary itself),
`near` resolves `near clear-state …` to this extension via its
`near-${command}` PATH lookup.

You can also invoke it directly — `near-clear-state …` is identical in
behaviour to `near clear-state …`.

### Verifying installation

```
which near-clear-state
near clear-state --help
```

The second command should print the usage block from this extension
(not near-cli-rs's own help).

## Repo layout

| Path | What it is |
|------|------------|
| `contract/` | The `state-cleanup` contract source (near-sdk 5.26.1). Its own `cargo-near` project. |
| `extension/wasm/state_cleanup.wasm` | The reproducibly-built wasm embedded into the extension binary. |
| `extension/src/` | The `near-clear-state` Rust extension. |
| `scripts/verify-wasm.sh` | Read the embedded build-context commit from the committed wasm, check it out into a temp worktree, rebuild reproducibly, and diff. Requires docker. |

This extension attempts to clean all state in a single transaction to fit with the near-cli-rs model of one command equalling one transaction. It's assumed that any limitations of cleaning the contract will come from RPCs not being able to serve a large enough view_state rather than the call running out of gas or exceeding the max_transaction_size of 1.5MB.

## Verifying the bundled wasm

The wasm at `extension/wasm/state_cleanup.wasm` was built reproducibly.
To audit the exact source that produced it, check out the commit it was built in:

```
git checkout 4a0bcb2016f4a59186023906adb8359af6d44ab2
```

When you're done auditing, switch back and run the verify script
(requires docker and cargo near):

```
git switch -
./scripts/verify-wasm.sh
```

The script reads the build-context commit out of the wasm's NEP-330
metadata, checks that commit out into a throwaway worktree, runs
`cargo near build reproducible-wasm` there, and compares the sha256
of the rebuilt wasm against the committed one.

## Running the tests

Drop a funder account into `extension/.env` (gitignored) — needs ~2.1
testnet NEAR per run:

```
TESTNET_ACCOUNT_ID=mywallet.testnet
TESTNET_PRIVATE_KEY=ed25519:...
```

Full suite — uses ~30 testnet NEAR per run (mostly refunded on subaccount delete):

```
cd extension && cargo test -- --nocapture --test-threads=1
```

Note: the two `*_on_intear_*` scenarios in `tests/integration.rs` hit Intear's testnet RPC, which throttles unauthenticated traffic — expect them to flake with `error decoding response body` on `view_state`. Get an API key at <https://rainy.intea.rs> for reliable runs.