//! `near-clean-state` — a near-cli-rs extension that wipes a contract
//! account's on-chain state without deleting the account.
//!
//! Flow per invocation:
//!   1. Read state via ViewState RPC.
//!   2. Fetch live `storage_remove_*` gas costs from protocol_config.
//!   3. Estimate the gas needed to remove every key; error out if it
//!      exceeds `MAX_CLEAN_GAS` (single-tx budget).
//!   4. Build one transaction = DeployContract(bundled wasm)
//!      + one FunctionCall("clean", all_keys) carrying `MAX_CLEAN_GAS`.
//!   5. Hand it to near-cli-rs's signer chain.
//!
//! At 1000 Tgas/tx (protocol version 83+) a single call can remove a
//! few megabytes of contract state, which is far above what `view_state`
//! is willing to return. A multi-tx fallback for permissive-RPC,
//! many-MB cases is intentional future work — see [`plan::plan_batches`].

pub mod cleanup;
pub mod plan;

use color_eyre::eyre::{Result, eyre};
use near_primitives::action::{Action, DeployContractAction, FunctionCallAction};
use serde::Serialize;

/// The state-cleanup wasm compiled from `contract/`. Built reproducibly
/// via `cargo near build reproducible-wasm`; verifiable with
/// `scripts/verify-wasm.sh`.
const BUNDLED_WASM: &[u8] = include_bytes!("../wasm/state_cleanup.wasm");

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = near_cli_rs::GlobalContext)]
#[interactive_clap(output_context = CleanStateContext)]
pub struct CleanStateCommand {
    /// Quiet mode — suppress non-essential output.
    #[interactive_clap(long)]
    pub quiet: bool,
    /// TEACH-ME mode — print detailed explanations of what the CLI is doing.
    #[interactive_clap(long)]
    pub teach_me: bool,
    #[interactive_clap(skip_default_input_arg)]
    /// What is the contract account ID to wipe?
    account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl CleanStateCommand {
    pub fn input_account_id(
        context: &near_cli_rs::GlobalContext,
    ) -> Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "What is the contract account ID to wipe?",
        )
    }
}

#[derive(Debug, Clone)]
pub struct CleanStateContext {
    global_context: near_cli_rs::GlobalContext,
    account_id: near_primitives::types::AccountId,
}

impl CleanStateContext {
    pub fn from_previous_context(
        previous_context: near_cli_rs::GlobalContext,
        scope: &<CleanStateCommand as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> Result<Self> {
        Ok(Self {
            global_context: previous_context,
            account_id: scope.account_id.clone().into(),
        })
    }
}

#[derive(Serialize)]
struct CleanArgs<'a> {
    keys: Vec<&'a str>,
}

impl From<CleanStateContext> for near_cli_rs::commands::ActionContext {
    fn from(item: CleanStateContext) -> Self {
        let account_id = item.account_id.clone();

        let get_prepopulated_transaction_after_getting_network_callback: near_cli_rs::commands::GetPrepopulatedTransactionAfterGettingNetworkCallback =
            std::sync::Arc::new(move |network_config| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| eyre!("Failed to start tokio runtime: {e}"))?;

                runtime.block_on(build_transaction(network_config, &account_id))
            });

        Self {
            global_context: item.global_context,
            interacting_with_account_ids: vec![item.account_id.clone()],
            get_prepopulated_transaction_after_getting_network_callback,
            on_before_signing_callback: std::sync::Arc::new(
                |_prepopulated_unsigned_transaction, _network_config| Ok(()),
            ),
            on_before_sending_transaction_callback: std::sync::Arc::new(
                |_signed_transaction, _network_config| Ok(String::new()),
            ),
            on_after_sending_transaction_callback: std::sync::Arc::new(
                |_outcome_view, _network_config| Ok(()),
            ),
            sign_as_delegate_action: false,
            on_sending_delegate_action_callback: None,
        }
    }
}

async fn build_transaction(
    network_config: &near_cli_rs::config::NetworkConfig,
    account_id: &near_primitives::types::AccountId,
) -> Result<near_cli_rs::commands::PrepopulatedTransaction> {
    let client = network_config.json_rpc_client();

    let gas_constants = plan::fetch_gas_constants(&client).await?;
    let entries = cleanup::read_state(&client, account_id).await?;

    if entries.is_empty() {
        return Err(eyre!(
            "Account <{account_id}> has no contract state to clean.",
        ));
    }

    // Single-tx model: refuse if the wipe wouldn't fit in one tx. The
    // multi-tx fallback (chunk across N txs using plan::plan_batches) is
    // future work — needed only when a permissive RPC returns many MB.
    let estimated = plan::estimate_total_gas(&entries, &gas_constants);
    if estimated > plan::MAX_CLEAN_GAS {
        return Err(eyre!(
            "State is too large to wipe in a single transaction \
             (estimated {estimated_tgas:.1} Tgas, budget {budget_tgas:.0} Tgas). \
             Multi-transaction wipes are not yet supported.",
            estimated_tgas = estimated as f64 / 1e12,
            budget_tgas = plan::MAX_CLEAN_GAS as f64 / 1e12,
        ));
    }

    eprintln!(
        "Wiping {} key(s) from {account_id} (est. {:.1} Tgas).",
        entries.len(),
        estimated as f64 / 1e12,
    );

    let encoded: Vec<String> = entries
        .iter()
        .map(|e| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(&e.key)
        })
        .collect();
    let args = CleanArgs {
        keys: encoded.iter().map(String::as_str).collect(),
    };
    let args_bytes = serde_json::to_vec(&args)
        .map_err(|e| eyre!("Failed to serialize clean() args: {e}"))?;

    let actions = vec![
        Action::DeployContract(DeployContractAction {
            code: BUNDLED_WASM.to_vec(),
        }),
        Action::FunctionCall(Box::new(FunctionCallAction {
            method_name: "clean".to_string(),
            args: args_bytes,
            gas: near_primitives::gas::Gas::from_gas(plan::MAX_CLEAN_GAS as u64),
            deposit: near_token::NearToken::from_yoctonear(0),
        })),
    ];

    Ok(near_cli_rs::commands::PrepopulatedTransaction {
        signer_id: account_id.clone(),
        receiver_id: account_id.clone(),
        actions,
    })
}
