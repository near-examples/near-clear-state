//! End-to-end test for `near-clean-state`.
//!
//! Skipped automatically when `TESTNET_ACCOUNT_ID` / `TESTNET_PRIVATE_KEY`
//! are absent so plain `cargo test` in dev doesn't fail. Run with:
//!
//!     TESTNET_ACCOUNT_ID=... TESTNET_PRIVATE_KEY=... \
//!       cargo test --test integration -- --nocapture
//!
//! Flow:
//!   1. Read funder creds from env.
//!   2. Create a fresh subaccount under the funder, funded with ~2 NEAR.
//!   3. Deploy `state_filler.wasm` to it and call `fill(...)` to put
//!      ~40 KB of state on chain.
//!   4. Confirm `view_state` shows that state.
//!   5. Spawn the `near-clean-state` binary against the subaccount.
//!   6. Confirm `view_state` is now empty.
//!   7. Tear the subaccount down (refunds remaining balance to funder).

use std::process::Command;
use std::sync::Arc;

use near_api::{Account, AccountId, Contract, NearToken, NetworkConfig, Signer, signer};
use near_api_types::transaction::result::{ExecutionFinalResult, TransactionResult};
use near_jsonrpc_client::{JsonRpcClient, methods};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use near_primitives::types::{BlockReference, Finality, StoreKey};
use near_primitives::views::QueryRequest;

type AnyError = Box<dyn std::error::Error + Send + Sync>;

const STATE_FILLER_WASM: &[u8] = include_bytes!("fixtures/state_filler.wasm");

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
    let outcome = run_test_body(
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
    let cleanup = Account(sub_id.clone())
        .delete_account_with_beneficiary(funder_id.clone())
        .with_signer(sub_signer.clone())
        .send_to(&network)
        .await;

    match (outcome, cleanup) {
        (Ok(()), Ok(_)) => Ok(()),
        (Err(e), _) => Err(e),
        (Ok(()), Err(e)) => Err(format!("cleanup failed: {e:?}").into()),
    }
}

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

async fn run_test_body(
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

    // Spawn near-clean-state. `CARGO_BIN_EXE_near-clean-state` is set by
    // cargo when building integration tests for this crate's `[[bin]]`.
    println!("Running near-clean-state");
    let status = Command::new(env!("CARGO_BIN_EXE_near-clean-state"))
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
        return Err(format!("near-clean-state exited {status}").into());
    }

    let post = read_state_count(rpc, sub_id).await?;
    println!("Post-wipe state: {post} keys");
    if post != 0 {
        return Err(format!("expected empty state, got {post} keys").into());
    }

    Ok(())
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
