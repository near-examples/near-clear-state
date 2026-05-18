//! Fill a fresh subaccount with state and leave it on chain.
//!
//! Run with:
//!     cargo test --test fill_only -- --nocapture

use std::sync::Arc;

use near_api::{Account, AccountId, Contract, NearToken, NetworkConfig, Signer, signer};
use near_api_types::transaction::result::{ExecutionFinalResult, TransactionResult};

type AnyError = Box<dyn std::error::Error + Send + Sync>;

const STATE_FILLER_WASM: &[u8] = include_bytes!("fixtures/state_filler.wasm");

#[tokio::test]
async fn fill_only() -> Result<(), AnyError> {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env"));

    let Ok(funder_id_raw) = std::env::var("TESTNET_ACCOUNT_ID") else {
        eprintln!("skipping fill_only: TESTNET_ACCOUNT_ID / TESTNET_PRIVATE_KEY not set");
        return Ok(());
    };
    let funder_sk_raw = std::env::var("TESTNET_PRIVATE_KEY")?;

    let funder_id: AccountId = funder_id_raw.parse()?;
    let funder_signer: Arc<Signer> = Signer::from_secret_key(funder_sk_raw.parse()?)?;

    let network = NetworkConfig::testnet();

    let suffix = format!("fill-{}", rand::random::<u32>());
    let sub_id: AccountId = format!("{suffix}.{funder_id}").parse()?;
    let sub_secret = signer::generate_secret_key()?;
    let sub_signer: Arc<Signer> = Signer::from_secret_key(sub_secret.clone())?;

    println!("Creating subaccount {sub_id}");
    unwrap_tx(
        Account::create_account(sub_id.clone())
            .fund_myself(funder_id.clone(), NearToken::from_near(2))
            .with_public_key(sub_secret.public_key())
            .with_signer(funder_signer.clone())
            .send_to(&network)
            .await?,
    )
    .map(|_| ())?;

    println!("Deploying state-filler wasm ({} bytes)", STATE_FILLER_WASM.len());
    unwrap_tx(
        Contract::deploy(sub_id.clone())
            .use_code(STATE_FILLER_WASM.to_vec())
            .without_init_call()
            .with_signer(sub_signer.clone())
            .send_to(&network)
            .await?,
    )
    .map(|_| ())?;

    println!("Filling state");
    unwrap_tx(
        Contract(sub_id.clone())
            .call_function(
                "fill",
                serde_json::json!({ "prefix": "k", "count": 100, "value_size": 200 }),
            )
            .transaction()
            .with_signer(sub_id.clone(), sub_signer.clone())
            .send_to(&network)
            .await?,
    )
    .map(|_| ())?;

    println!();
    println!("=== filled subaccount left on chain ===");
    println!("account_id:  {sub_id}");
    println!("private_key: {sub_secret}");

    Ok(())
}

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
