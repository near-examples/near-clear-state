//! `near-clean-state` — a near-cli-rs extension that wipes a contract
//! account's on-chain state without deleting the account.
//!
//! Flow per invocation:
//!   1. Read state via ViewState RPC.
//!   2. Fetch live `storage_remove_*` gas costs from protocol_config.
//!   3. Pack keys into batches that fit `per_action_gas(max_calls)`.
//!   4. Build one transaction = DeployContract(bundled wasm) + N FunctionCall("clean", ...).
//!   5. Hand it to near-cli-rs's signer chain.

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
    #[interactive_clap(skip_default_input_arg)]
    /// What is the contract account ID to wipe?
    account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// Maximum number of clean() calls in the single deploy+wipe transaction
    max_calls: u64,
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

    fn input_max_calls(_context: &near_cli_rs::GlobalContext) -> Result<Option<u64>> {
        let value = inquire::CustomType::<u64>::new(
            "Maximum number of clean() calls (default 10):",
        )
        .with_default(10)
        .prompt()?;
        Ok(Some(value))
    }
}

#[derive(Debug, Clone)]
pub struct CleanStateContext {
    global_context: near_cli_rs::GlobalContext,
    account_id: near_primitives::types::AccountId,
    max_calls: u64,
}

impl CleanStateContext {
    pub fn from_previous_context(
        previous_context: near_cli_rs::GlobalContext,
        scope: &<CleanStateCommand as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> Result<Self> {
        if scope.max_calls == 0 {
            return Err(eyre!("`--max-calls` must be at least 1"));
        }
        Ok(Self {
            global_context: previous_context,
            account_id: scope.account_id.clone().into(),
            max_calls: scope.max_calls,
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
        let max_calls: u64 = item.max_calls;

        let get_prepopulated_transaction_after_getting_network_callback: near_cli_rs::commands::GetPrepopulatedTransactionAfterGettingNetworkCallback =
            std::sync::Arc::new(move |network_config| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| eyre!("Failed to start tokio runtime: {e}"))?;

                runtime.block_on(build_transaction(network_config, &account_id, max_calls))
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
    max_calls: u64,
) -> Result<near_cli_rs::commands::PrepopulatedTransaction> {
    let client = network_config.json_rpc_client();

    let gas_constants = plan::fetch_gas_constants(&client).await?;
    let entries = cleanup::read_state(&client, account_id).await?;

    if entries.is_empty() {
        return Err(eyre!(
            "Account <{account_id}> has no contract state to clean.",
        ));
    }

    let max_calls_u32 = u32::try_from(max_calls)
        .map_err(|_| eyre!("--max-calls is unreasonably large"))?;
    let per_action = plan::per_action_gas(max_calls_u32);
    let batches = plan::plan_batches(&entries, per_action, &gas_constants);

    if batches.len() > max_calls as usize {
        return Err(eyre!(
            "Planning produced {} batches but --max-calls is {max_calls}. \
             Raise --max-calls or split the wipe across multiple invocations.",
            batches.len(),
        ));
    }

    eprintln!(
        "Planning {} batch(es) covering {} key(s) across {account_id}.",
        batches.len(),
        entries.len(),
    );

    let mut actions: Vec<Action> = Vec::with_capacity(batches.len() + 1);
    actions.push(Action::DeployContract(DeployContractAction {
        code: BUNDLED_WASM.to_vec(),
    }));

    let per_action_gas = near_primitives::gas::Gas::from_gas(per_action as u64);
    let zero_deposit = near_token::NearToken::from_yoctonear(0);

    for batch in &batches {
        let encoded: Vec<String> = batch
            .iter()
            .map(|k| {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.encode(k)
            })
            .collect();
        let args = CleanArgs {
            keys: encoded.iter().map(String::as_str).collect(),
        };
        let args_bytes = serde_json::to_vec(&args)
            .map_err(|e| eyre!("Failed to serialize clean() args: {e}"))?;

        actions.push(Action::FunctionCall(Box::new(FunctionCallAction {
            method_name: "clean".to_string(),
            args: args_bytes,
            gas: per_action_gas,
            deposit: zero_deposit,
        })));
    }

    Ok(near_cli_rs::commands::PrepopulatedTransaction {
        signer_id: account_id.clone(),
        receiver_id: account_id.clone(),
        actions,
    })
}
