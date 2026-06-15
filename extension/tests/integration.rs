//! End-to-end tests for `near-clear-state`.
//!
//! Two flavours:
//! - `wipe_clears_state` spawns the actual `near-clear-state` binary
//!   against a fresh subaccount with ~25 KB of state. Covers the full
//!   CLI surface end-to-end.
//! - The four `wipe_*_on_*` size-matrix tests drive `cleanup::read_state`
//!   and `plan::*` directly against named RPCs (FastNEAR vs Intear) to
//!   exercise the preflight code paths at different state sizes.
//!
//! Size matrix:
//!   1. 40 KB on FastNEAR   → wipe succeeds (under view_state cap, under gas)
//!   2. 100 KB on FastNEAR  → fails: read_state hits the ~50 KB view_state cap
//!   3. ~500 KB on Intear   → wipe succeeds (well above FastNEAR's cap)
//!   4. ~1.5 MB on Intear   → fails: gas estimate exceeds max_total_prepaid_gas
//!
//! Scenario 4 was originally specified as a 1.6 MB / tx-size failure, but
//! the gas formula caps at ~14k entries before tx-size becomes the
//! binding constraint, so this scenario verifies the gas-budget path on
//! a permissive RPC. The tx-size preflight is covered by a unit test in
//! `plan.rs`.
//!
//! All tests skip cleanly if `TESTNET_ACCOUNT_ID` / `TESTNET_PRIVATE_KEY`
//! are not set. Run with:
//!
//!     TESTNET_ACCOUNT_ID=... TESTNET_PRIVATE_KEY=... \
//!       cargo test --test integration -- --nocapture --test-threads=1
//!
//! `--test-threads=1` is recommended — large size scenarios hit testnet
//! hard with multi-batch fills.

use std::process::Command;
use std::sync::Arc;

use base64::Engine as _;
use near_api::{Account, AccountId, Contract, NearToken, NetworkConfig, Signer, signer};
use near_api_types::transaction::result::{ExecutionFinalResult, TransactionResult};
use near_clear_state::{cleanup, plan};
use near_jsonrpc_client::{JsonRpcClient, methods};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use near_primitives::types::{BlockReference, Finality, StoreKey};
use near_primitives::views::QueryRequest;

type AnyError = Box<dyn std::error::Error + Send + Sync>;

const STATE_FILLER_WASM: &[u8] = include_bytes!("fixtures/state_filler.wasm");
const CLEAN_WASM: &[u8] = include_bytes!("../wasm/state_cleanup.wasm");

const FASTNEAR_TESTNET: &str = "https://test.rpc.fastnear.com";
const INTEAR_TESTNET: &str = "https://testnet-rpc.intea.rs";

/// Entries inserted per fill() call. Each entry costs ~95 Ggas (storage_write
/// base ~64 Ggas + per-byte costs + LookupMap wasm execution overhead). With
/// 280 Tgas attached, 1500 entries leaves comfortable headroom under the
/// 300 Tgas single-call cap.
const FILL_BATCH_SIZE: u32 = 1500;

// ---------------------------------------------------------------------------
// Binary-spawn end-to-end test.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wipe_clears_state() -> Result<(), AnyError> {
    // Load extension/.env if present (gitignored). Process env still
    // wins over the .env file.
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env"));

    // Skip cleanly if creds aren't set — keeps `cargo test` green in dev.
    let Ok(funder_id_raw) = std::env::var("TESTNET_ACCOUNT_ID") else {
        eprintln!(
            "skipping wipe_clears_state: TESTNET_ACCOUNT_ID / TESTNET_PRIVATE_KEY not set"
        );
        return Ok(());
    };
    let funder_sk_raw = std::env::var("TESTNET_PRIVATE_KEY")?;

    let funder_id: AccountId = funder_id_raw.parse()?;
    let funder_signer: Arc<Signer> = Signer::from_secret_key(funder_sk_raw.parse()?)?;

    let network = NetworkConfig::testnet();
    let rpc = JsonRpcClient::connect(network.rpc_endpoints[0].url.as_str());

    // Random subaccount + fresh key — never collides between runs.
    let suffix = format!("clean-test-{}", rand::random::<u32>());
    let sub_id: AccountId = format!("{suffix}.{funder_id}").parse()?;
    let sub_secret = signer::generate_secret_key()?;
    let sub_signer: Arc<Signer> = Signer::from_secret_key(sub_secret.clone())?;

    // Run the test body in a closure that returns Result so cleanup
    // below runs unconditionally (no async drop in stable Rust).
    let outcome = run_cli_test_body(
        &network,
        &rpc,
        &funder_id,
        &funder_signer,
        &sub_id,
        &sub_secret,
        &sub_signer,
    )
    .await;

    // Always attempt to delete the subaccount — refunds remaining
    // balance back to the funder. Best-effort: if it fails (e.g. because
    // the subaccount was never created), surface the error after the
    // test outcome.
    let cleanup_res = Account(sub_id.clone())
        .delete_account_with_beneficiary(funder_id.clone())
        .with_signer(sub_signer.clone())
        .send_to(&network)
        .await;

    match (outcome, cleanup_res) {
        (Ok(()), Ok(_)) => Ok(()),
        (Err(e), _) => Err(e),
        (Ok(()), Err(e)) => Err(format!("cleanup failed: {e:?}").into()),
    }
}

async fn run_cli_test_body(
    network: &NetworkConfig,
    rpc: &JsonRpcClient,
    funder_id: &AccountId,
    funder_signer: &Arc<Signer>,
    sub_id: &AccountId,
    sub_secret: &near_api_types::SecretKey,
    sub_signer: &Arc<Signer>,
) -> Result<(), AnyError> {
    // Create the subaccount with 2 NEAR — covers storage staking for the
    // deploy + fill txs and leaves headroom for the wipe.
    println!("Creating subaccount {sub_id}");
    unwrap_tx(
        Account::create_account(sub_id.clone())
            .fund_myself(funder_id.clone(), NearToken::from_near(2))
            .with_public_key(sub_secret.public_key())
            .with_signer(funder_signer.clone())
            .send_to(network)
            .await?,
    )
    .map(|_| ())?;

    // Deploy the kv-store filler.
    println!("Deploying state-filler wasm ({} bytes)", STATE_FILLER_WASM.len());
    unwrap_tx(
        Contract::deploy(sub_id.clone())
            .use_code(STATE_FILLER_WASM.to_vec())
            .without_init_call()
            .with_signer(sub_signer.clone())
            .send_to(network)
            .await?,
    )
    .map(|_| ())?;

    // Write ~25 KB of state (100 entries × 200-byte values, plus
    // LookupMap overhead). Stays under the ~50 KB `view_state` RPC cap.
    println!("Filling state");
    unwrap_tx(
        Contract(sub_id.clone())
            .call_function(
                "fill",
                serde_json::json!({ "prefix": "k", "count": 100, "value_size": 200 }),
            )
            .transaction()
            .with_signer(sub_id.clone(), sub_signer.clone())
            .send_to(network)
            .await?,
    )
    .map(|_| ())?;

    let pre = read_state_count(rpc, sub_id).await?;
    println!("Pre-wipe state: {pre} keys");
    if pre < 100 {
        return Err(format!("expected substantial state pre-wipe, got {pre} keys").into());
    }

    // Spawn near-clear-state. `CARGO_BIN_EXE_near-clear-state` is set by
    // cargo when building integration tests for this crate's `[[bin]]`.
    println!("Running near-clear-state");
    let status = Command::new(env!("CARGO_BIN_EXE_near-clear-state"))
        .args([
            sub_id.as_str(),
            "network-config",
            "testnet",
            "sign-with-plaintext-private-key",
            &sub_secret.to_string(),
            "send",
        ])
        .status()?;
    if !status.success() {
        return Err(format!("near-clear-state exited {status}").into());
    }

    let post = read_state_count(rpc, sub_id).await?;
    println!("Post-wipe state: {post} keys");
    if post != 0 {
        return Err(format!("expected empty state, got {post} keys").into());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// State-size matrix.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wipe_40kb_on_fastnear_succeeds() -> Result<(), AnyError> {
    run_scenario(Scenario {
        name: "40kb-fastnear",
        rpc_url: FASTNEAR_TESTNET,
        fill_count: 100,
        fill_value_size: 200,
        fund_near: 2,
        expect: Expect::Success,
    })
    .await
}

#[tokio::test]
async fn wipe_100kb_on_fastnear_fails_view_state_cap() -> Result<(), AnyError> {
    run_scenario(Scenario {
        name: "100kb-fastnear",
        rpc_url: FASTNEAR_TESTNET,
        fill_count: 250,
        fill_value_size: 200,
        fund_near: 3,
        expect: Expect::ReadStateError { substr: "too large" },
    })
    .await
}

#[tokio::test]
async fn wipe_500kb_on_intear_succeeds() -> Result<(), AnyError> {
    // 2500 × 200 B → ~525 KB raw / ~775 KB view_state response. Below
    // 4800×200 (~1.5 MB response) which flakes on Intear's unauthenticated
    // response stream. Still 15× over FastNEAR's ~50 KB view_state cap, so
    // the point (permissive RPC handles state FastNEAR can't) still stands.
    run_scenario(Scenario {
        name: "500kb-intear",
        rpc_url: INTEAR_TESTNET,
        fill_count: 2500,
        fill_value_size: 200,
        fund_near: 10,
        expect: Expect::Success,
    })
    .await
}

#[tokio::test]
async fn wipe_1_5mb_on_intear_fails_gas_budget() -> Result<(), AnyError> {
    // ~1.3 mNEAR per entry storage stake (key + value bytes + LookupMap
    // overhead) → 14k entries = ~18 NEAR storage + gas. 25 NEAR leaves
    // a few NEAR slack for the deploy + first batches' gas before any
    // entries are written.
    run_scenario(Scenario {
        name: "1_5mb-intear",
        rpc_url: INTEAR_TESTNET,
        fill_count: 14000,
        fill_value_size: 100,
        fund_near: 25,
        expect: Expect::GasBudgetExceeded,
    })
    .await
}

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    rpc_url: &'static str,
    fill_count: u32,
    fill_value_size: u32,
    /// NEAR to forward to the per-run subaccount. Needs to cover storage
    /// staking (~10 µNEAR/byte) plus a slack for gas. Refunded on
    /// subaccount deletion at the end of the test.
    fund_near: u32,
    expect: Expect,
}

#[derive(Clone, Copy)]
enum Expect {
    /// Preflight succeeds and the wipe actually empties state on chain.
    Success,
    /// `cleanup::read_state` returns Err whose message contains `substr`.
    ReadStateError { substr: &'static str },
    /// `read_state` succeeds but `estimate_total_gas > max_total_prepaid_gas`.
    GasBudgetExceeded,
}

async fn run_scenario(scenario: Scenario) -> Result<(), AnyError> {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env"));

    let Ok(funder_id_raw) = std::env::var("TESTNET_ACCOUNT_ID") else {
        eprintln!(
            "skipping {}: TESTNET_ACCOUNT_ID / TESTNET_PRIVATE_KEY not set",
            scenario.name,
        );
        return Ok(());
    };
    let funder_sk_raw = std::env::var("TESTNET_PRIVATE_KEY")?;

    let funder_id: AccountId = funder_id_raw.parse()?;
    let funder_signer: Arc<Signer> = Signer::from_secret_key(funder_sk_raw.parse()?)?;

    // Setup uses near-api's default testnet RPC (FastNEAR). The target
    // RPC is only used for the read_state assertions and the actual wipe.
    let setup_network = NetworkConfig::testnet();
    let target_rpc = JsonRpcClient::connect(scenario.rpc_url);

    let suffix = format!("{}-{}", scenario.name, rand::random::<u32>());
    let sub_id: AccountId = format!("{suffix}.{funder_id}").parse()?;
    let sub_secret = signer::generate_secret_key()?;
    let sub_signer: Arc<Signer> = Signer::from_secret_key(sub_secret.clone())?;

    let outcome = run_scenario_body(
        &scenario,
        &setup_network,
        &target_rpc,
        &funder_id,
        &funder_signer,
        &sub_id,
        &sub_secret,
        &sub_signer,
    )
    .await;

    println!("[{}] Deleting subaccount {sub_id}", scenario.name);
    let cleanup_res = Account(sub_id.clone())
        .delete_account_with_beneficiary(funder_id.clone())
        .with_signer(sub_signer.clone())
        .send_to(&setup_network)
        .await;

    match (outcome, cleanup_res) {
        (Ok(()), Ok(_)) => Ok(()),
        (Err(e), _) => Err(e),
        (Ok(()), Err(e)) => Err(format!("[{}] cleanup failed: {e:?}", scenario.name).into()),
    }
}

async fn run_scenario_body(
    scenario: &Scenario,
    network: &NetworkConfig,
    target_rpc: &JsonRpcClient,
    funder_id: &AccountId,
    funder_signer: &Arc<Signer>,
    sub_id: &AccountId,
    sub_secret: &near_api_types::SecretKey,
    sub_signer: &Arc<Signer>,
) -> Result<(), AnyError> {
    let name = scenario.name;

    println!(
        "[{name}] Creating subaccount {sub_id} funded with {} NEAR",
        scenario.fund_near,
    );
    unwrap_tx(
        Account::create_account(sub_id.clone())
            .fund_myself(funder_id.clone(), NearToken::from_near(scenario.fund_near.into()))
            .with_public_key(sub_secret.public_key())
            .with_signer(funder_signer.clone())
            .send_to(network)
            .await?,
    )
    .map(|_| ())?;

    println!(
        "[{name}] Deploying state-filler wasm ({} bytes)",
        STATE_FILLER_WASM.len(),
    );
    unwrap_tx(
        Contract::deploy(sub_id.clone())
            .use_code(STATE_FILLER_WASM.to_vec())
            .without_init_call()
            .with_signer(sub_signer.clone())
            .send_to(network)
            .await?,
    )
    .map(|_| ())?;

    fill_state(
        network,
        sub_id,
        sub_signer,
        scenario.fill_count,
        scenario.fill_value_size,
        name,
    )
    .await?;

    match scenario.expect {
        Expect::ReadStateError { substr } => {
            println!(
                "[{name}] Calling read_state against {} — expecting error",
                scenario.rpc_url,
            );
            let err = match cleanup::read_state(target_rpc, sub_id).await {
                Err(e) => e,
                Ok(entries) => {
                    return Err(format!(
                        "expected read_state to error with substring {substr:?}, \
                         instead got {} entries",
                        entries.len(),
                    )
                    .into());
                }
            };
            let msg = format!("{err}");
            println!("[{name}] read_state error: {msg}");
            if !msg.to_lowercase().contains(&substr.to_lowercase()) {
                return Err(format!(
                    "expected read_state error to contain {substr:?}, got: {msg}",
                )
                .into());
            }
        }
        Expect::GasBudgetExceeded => {
            println!("[{name}] Calling read_state against {}", scenario.rpc_url);
            let entries = cleanup::read_state(target_rpc, sub_id).await?;
            println!("[{name}] read_state returned {} entries", entries.len());

            let constants = plan::fetch_protocol_constants(target_rpc).await?;
            let gas_estimate = plan::estimate_total_gas(&entries, &constants.gas);
            let budget = constants.max_total_prepaid_gas;
            println!(
                "[{name}] gas estimate: {:.1} Tgas (budget {:.0} Tgas)",
                gas_estimate as f64 / 1e12,
                budget as f64 / 1e12,
            );
            if gas_estimate <= budget {
                return Err(format!(
                    "expected gas estimate to exceed max_total_prepaid_gas, \
                     got {gas_estimate} (budget {budget})",
                )
                .into());
            }
        }
        Expect::Success => {
            println!("[{name}] Calling read_state against {}", scenario.rpc_url);
            let entries = cleanup::read_state(target_rpc, sub_id).await?;
            println!("[{name}] read_state returned {} entries", entries.len());
            if (entries.len() as u32) < scenario.fill_count {
                return Err(format!(
                    "expected at least {} entries, got {}",
                    scenario.fill_count,
                    entries.len(),
                )
                .into());
            }

            let constants = plan::fetch_protocol_constants(target_rpc).await?;
            let gas_estimate = plan::estimate_total_gas(&entries, &constants.gas);
            let budget = constants.max_total_prepaid_gas;
            println!(
                "[{name}] gas estimate: {:.1} Tgas (budget {:.0} Tgas)",
                gas_estimate as f64 / 1e12,
                budget as f64 / 1e12,
            );
            if gas_estimate > budget {
                return Err(format!(
                    "gas estimate {gas_estimate} > budget {budget} — scenario sized wrong",
                )
                .into());
            }

            // Actually wipe: deploy the clean wasm and call clean(keys).
            // Two separate txs instead of one (the binary does both in one
            // tx via DeployContract+FunctionCall in the same actions vec,
            // but near-api makes that awkward — functionally equivalent).
            println!("[{name}] Deploying clean wasm ({} bytes)", CLEAN_WASM.len());
            unwrap_tx(
                Contract::deploy(sub_id.clone())
                    .use_code(CLEAN_WASM.to_vec())
                    .without_init_call()
                    .with_signer(sub_signer.clone())
                    .send_to(network)
                    .await?,
            )
            .map(|_| ())?;

            let keys_b64: Vec<String> = entries
                .iter()
                .map(|e| base64::engine::general_purpose::STANDARD.encode(&e.key))
                .collect();
            println!("[{name}] Calling clean() with {} keys", keys_b64.len());
            unwrap_tx(
                Contract(sub_id.clone())
                    .call_function("clean", serde_json::json!({ "keys": keys_b64 }))
                    .transaction()
                    .gas(near_gas::NearGas::from_gas(budget as u64))
                    .with_signer(sub_id.clone(), sub_signer.clone())
                    .send_to(network)
                    .await?,
            )
            .map(|_| ())?;

            let post = cleanup::read_state(target_rpc, sub_id).await?;
            println!("[{name}] Post-wipe: {} entries", post.len());
            if !post.is_empty() {
                return Err(format!(
                    "expected empty state post-wipe, got {} entries",
                    post.len(),
                )
                .into());
            }
        }
    }

    Ok(())
}

/// Issue one or more `fill(...)` calls until `total_count` entries are written.
/// Each call inserts at most `FILL_BATCH_SIZE` entries to stay under the
/// 300 Tgas per-call cap. Distinct prefixes per batch keep keys unique.
async fn fill_state(
    network: &NetworkConfig,
    sub_id: &AccountId,
    sub_signer: &Arc<Signer>,
    total_count: u32,
    value_size: u32,
    log_name: &str,
) -> Result<(), AnyError> {
    let mut written = 0u32;
    let mut batch_idx = 0u32;
    while written < total_count {
        let this_batch = (total_count - written).min(FILL_BATCH_SIZE);
        let prefix = format!("b{batch_idx}_");
        println!(
            "[{log_name}] fill batch {batch_idx}: {this_batch} entries × {value_size} B \
             (prefix={prefix:?}, total so far {})",
            written + this_batch,
        );
        unwrap_tx(
            Contract(sub_id.clone())
                .call_function(
                    "fill",
                    serde_json::json!({
                        "prefix": prefix,
                        "count": this_batch,
                        "value_size": value_size,
                    }),
                )
                .transaction()
                .gas(near_gas::NearGas::from_tgas(280))
                .with_signer(sub_id.clone(), sub_signer.clone())
                .send_to(network)
                .await?,
        )
        .map(|_| ())?;
        written += this_batch;
        batch_idx += 1;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers.
// ---------------------------------------------------------------------------

/// Unwrap a near-api `TransactionResult`, returning an error if the
/// transaction is still pending or its on-chain execution failed.
fn unwrap_tx(tx: TransactionResult) -> Result<ExecutionFinalResult, AnyError> {
    let final_result = match tx {
        TransactionResult::Full(r) => *r,
        TransactionResult::Pending { status } => {
            return Err(format!("transaction still pending: {status:?}").into());
        }
    };
    final_result
        .clone()
        .into_result()
        .map_err(|e| -> AnyError { format!("transaction failed on chain: {e:?}").into() })?;
    Ok(final_result)
}

async fn read_state_count(
    rpc: &JsonRpcClient,
    account_id: &AccountId,
) -> Result<usize, AnyError> {
    let response = rpc
        .call(methods::query::RpcQueryRequest {
            block_reference: BlockReference::Finality(Finality::Final),
            request: QueryRequest::ViewState {
                account_id: account_id.clone(),
                prefix: StoreKey::from(Vec::new()),
                include_proof: false,
            },
        })
        .await?;
    match response.kind {
        QueryResponseKind::ViewState(s) => Ok(s.values.len()),
        _ => Err("unexpected RPC response kind".into()),
    }
}
