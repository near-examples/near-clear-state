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
- **`max_transaction_size = 1.5 MB`** — caps the serialized tx; with the ~72 KB wasm, that leaves room for tens of thousands of small keys (gas hits first in practice).

## Install

### Prerequisites

[`near-cli-rs`](https://github.com/near/near-cli-rs) must already be installed and on your `$PATH` — this is an extension to it, not a standalone tool.

### Installing the extension

Directly from this GitHub repo:

```
cargo install --locked --git https://github.com/near-examples/near-clear-state near-clear-state
```

Or from a local checkout:

```
git clone https://github.com/near-examples/near-clear-state
cargo install --path near-clear-state/extension --locked
```

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

| Path                                           | What it is                                                                                                                   |
| ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `extension/wasm/state_cleanup.wasm`            | NEAR's prebuilt `state-manipulation` cleanup wasm, vendored from core-contracts and embedded into the extension binary.      |
| `extension/wasm/state_cleanup.wasm.provenance` | The pin: upstream repo, commit SHA, and sha256 the bundled wasm came from. Single source of truth for the two scripts below. |
| `extension/src/`                               | The `near-clear-state` Rust extension.                                                                                       |
| `scripts/update-wasm.sh`                       | Re-pin to a core-contracts commit, download its prebuilt wasm, and refresh the provenance file.                              |
| `scripts/verify-wasm.sh`                       | Re-download the upstream wasm at the pinned commit and sha256-diff it against the committed copy. Requires only `curl`.      |

This extension attempts to clean all state in a single transaction to fit with the near-cli-rs model of one command equalling one transaction. It's assumed that any limitations of cleaning the contract will come from RPCs not being able to serve a large enough view_state rather than the call running out of gas or exceeding the max_transaction_size of 1.5MB.

## Verifying the bundled wasm

The wasm at `extension/wasm/state_cleanup.wasm` is NEAR's prebuilt
`state-manipulation` cleanup contract, vendored from
[core-contracts](https://github.com/near/core-contracts) (licensed
`MIT OR Apache-2.0`). It is not built in this repo; it is pinned to a
specific upstream commit recorded in
`extension/wasm/state_cleanup.wasm.provenance`.

To audit it, read that commit's
`state-manipulation/src/lib.rs` on github — a few-line `clean()` that
`storage_remove()`s each base64 key — then confirm the bundled bytes match
what upstream published at that commit (requires only `curl`):

```
./scripts/verify-wasm.sh
```

The script reads the pinned commit + sha256 from the provenance file,
re-downloads `state-manipulation/res/state_cleanup.wasm` at that commit, and
sha256-diffs it against the committed copy. The trust anchor is the pinned
commit (reviewable in this repo's git history); the sha256 is a convenience
value.

To bump the pin to a newer upstream commit, run `./scripts/update-wasm.sh`
(resolves `master` by default, or pass a commit). It refreshes both the wasm
and the provenance file.

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
