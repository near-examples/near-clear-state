//! Gas estimation for the state-cleanup wipe flow.
//!
//! Gas constants are pulled from the live chain via
//! `EXPERIMENTAL_protocol_config` rather than hardcoded.

use color_eyre::eyre::{Result, eyre};
use near_jsonrpc_client::JsonRpcClient;
use near_jsonrpc_client::methods::EXPERIMENTAL_protocol_config::RpcProtocolConfigError;
use near_primitives::action::Action;
use near_primitives::types::{AccountId, BlockReference, Finality};
use serde::Deserialize;

/// +30% multiplier over the published `storage_remove` host-function cost.
/// Covers wasm execution (the contract's `for` loop + base64 decode) and
/// JSON arg parsing that the host-function cost alone doesn't include.
const SAFETY_FACTOR_PCT: u128 = 130;

/// The three gas costs that drive batching, fetched live from the chain.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct GasConstants {
    #[serde(deserialize_with = "u128_from_any")]
    pub storage_remove_base: u128,
    #[serde(deserialize_with = "u128_from_any")]
    pub storage_remove_key_byte: u128,
    #[serde(deserialize_with = "u128_from_any")]
    pub storage_remove_ret_value_byte: u128,
}

/// Protocol params relevant to preflighting a wipe transaction.
#[derive(Debug, Clone, Copy)]
pub struct ProtocolConstants {
    pub gas: GasConstants,
    /// Protocol cap on the borsh-serialized size of a single transaction,
    /// in bytes. Fetched live so we don't drift if the protocol param changes.
    pub max_transaction_size: u64,
    /// Protocol cap on the sum of `FunctionCall.gas` across a single tx's
    /// actions, in gas units. This is the gas budget we attach to the
    /// single `clean(keys=[...])` call and the ceiling the gas estimate
    /// must come in under. Action base costs are billed separately against
    /// the signer's NEAR balance and do not count against this cap.
    pub max_total_prepaid_gas: u128,
}

/// Conservative overhead (in bytes) for the parts of a SignedTransaction that
/// wrap the actions list: signer_id + receiver_id length prefixes and bytes,
/// public_key, nonce, block_hash, and the signature. We add 2 × account_id.len()
/// for the two AccountId fields and a fixed pad for everything else.
const TX_WRAPPER_OVERHEAD_BYTES: usize = 256;

/// Safety buffer (in bytes) we keep below the protocol's `max_transaction_size`
/// when preflighting. 0.1 MiB of slack absorbs any underestimate from the
/// wrapper-overhead approximation and gives headroom if the protocol param
/// is lowered between fetch and submission.
pub const TX_SIZE_BUFFER_BYTES: u64 = 100 * 1024;

/// Gas-cost fields are emitted as either JSON numbers or stringified
/// integers depending on serde-json's number-handling defaults across
/// nearcore versions. Accept both.
fn u128_from_any<'de, D>(deserializer: D) -> Result<u128, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::Number(n) => n
            .as_u64()
            .map(u128::from)
            .ok_or_else(|| D::Error::custom("ext-cost is not a u64")),
        serde_json::Value::String(s) => s.parse::<u128>().map_err(D::Error::custom),
        _ => Err(D::Error::custom("expected number or string ext-cost")),
    }
}

#[derive(Debug, Deserialize)]
struct ProtocolConfigPartial {
    runtime_config: RuntimeConfigPartial,
}

impl near_jsonrpc_client::methods::RpcHandlerResponse for ProtocolConfigPartial {}

#[derive(Debug, Deserialize)]
struct RuntimeConfigPartial {
    wasm_config: WasmConfigPartial,
}

#[derive(Debug, Deserialize)]
struct WasmConfigPartial {
    ext_costs: GasConstants,
    limit_config: LimitConfigPartial,
}

#[derive(Debug, Deserialize)]
struct LimitConfigPartial {
    max_transaction_size: u64,
    #[serde(deserialize_with = "u128_from_any")]
    max_total_prepaid_gas: u128,
}

/// Fetch the live `storage_remove_*` costs and tx-size limit from the chain.
/// Reads only the fields we need so the deserialization isn't coupled to the
/// rest of the runtime-config shape.
pub async fn fetch_protocol_constants(client: &JsonRpcClient) -> Result<ProtocolConstants> {
    let block_ref = BlockReference::Finality(Finality::Final);
    let request = near_jsonrpc_client::methods::any::<
        std::result::Result<ProtocolConfigPartial, RpcProtocolConfigError>,
    >(
        "EXPERIMENTAL_protocol_config",
        serde_json::to_value(&block_ref)?,
    );

    let response = client.call(request).await.map_err(|err| {
        color_eyre::eyre::eyre!("Failed to fetch protocol config: {err}")
    })?;
    let wasm = response.runtime_config.wasm_config;
    Ok(ProtocolConstants {
        gas: wasm.ext_costs,
        max_transaction_size: wasm.limit_config.max_transaction_size,
        max_total_prepaid_gas: wasm.limit_config.max_total_prepaid_gas,
    })
}

/// Estimated gas cost of removing a single key, including the +30% safety
/// factor.
pub fn estimate_key_gas(key_bytes: usize, value_bytes: usize, c: &GasConstants) -> u128 {
    let raw = c.storage_remove_base
        + (key_bytes as u128) * c.storage_remove_key_byte
        + (value_bytes as u128) * c.storage_remove_ret_value_byte;
    raw * SAFETY_FACTOR_PCT / 100
}

/// A single storage entry observed via `view_state`.
#[derive(Debug, Clone)]
pub struct StateEntry {
    /// Raw decoded key bytes (view_state returns these base64-encoded;
    /// we decode once at the boundary).
    pub key: Vec<u8>,
    /// Length of the value in bytes. We don't carry the value itself —
    /// only the byte count matters for gas planning, and saving every
    /// value would dominate memory on large accounts.
    pub value_bytes: usize,
}

/// Total estimated gas (with safety factor) to clean `entries` in one
/// `clean()` call. Used to verify the wipe fits in a single tx.
pub fn estimate_total_gas(entries: &[StateEntry], c: &GasConstants) -> u128 {
    entries
        .iter()
        .map(|e| estimate_key_gas(e.key.len(), e.value_bytes, c))
        .sum()
}

/// Conservative upper bound on the borsh-serialized size of the SignedTransaction
/// that will carry `actions` between an account and itself. Used to preflight
/// against `max_transaction_size` before we hand the tx to the signer chain.
pub fn estimate_transaction_size(actions: &[Action], account_id: &AccountId) -> Result<usize> {
    let actions_bytes = borsh::to_vec(actions)
        .map_err(|e| eyre!("Failed to borsh-serialize actions: {e}"))?;
    Ok(actions_bytes.len() + TX_WRAPPER_OVERHEAD_BYTES + 2 * account_id.as_str().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constants() -> GasConstants {
        GasConstants {
            storage_remove_base: 53_473_030_500,
            storage_remove_key_byte: 38_220_384,
            storage_remove_ret_value_byte: 11_531_556,
        }
    }

    fn raw_per_key(k: usize, v: usize) -> u128 {
        let c = constants();
        let raw = c.storage_remove_base
            + (k as u128) * c.storage_remove_key_byte
            + (v as u128) * c.storage_remove_ret_value_byte;
        raw * SAFETY_FACTOR_PCT / 100
    }

    #[test]
    fn estimate_matches_formula() {
        assert_eq!(estimate_key_gas(10, 100, &constants()), raw_per_key(10, 100));
        assert_eq!(estimate_key_gas(20, 3072, &constants()), raw_per_key(20, 3072));
        assert_eq!(estimate_key_gas(5, 0, &constants()), raw_per_key(5, 0));
    }

    #[test]
    fn estimate_total_gas_is_sum_of_per_key() {
        let c = constants();
        let entries = vec![
            StateEntry { key: vec![0; 10], value_bytes: 50 },
            StateEntry { key: vec![0; 20], value_bytes: 200 },
            StateEntry { key: vec![0; 40], value_bytes: 4096 },
        ];
        let expected: u128 = entries
            .iter()
            .map(|e| estimate_key_gas(e.key.len(), e.value_bytes, &c))
            .sum();
        assert_eq!(estimate_total_gas(&entries, &c), expected);
        assert_eq!(estimate_total_gas(&[], &c), 0);
    }

    #[test]
    fn estimate_transaction_size_covers_payload_plus_overhead() {
        use near_primitives::action::{DeployContractAction, FunctionCallAction};

        let wasm = vec![0u8; 100_000];
        let args = vec![0u8; 5_000];
        let actions = vec![
            Action::DeployContract(DeployContractAction { code: wasm.clone() }),
            Action::FunctionCall(Box::new(FunctionCallAction {
                method_name: "clean".to_string(),
                args: args.clone(),
                gas: near_primitives::gas::Gas::from_gas(990_000_000_000_000),
                deposit: near_token::NearToken::from_yoctonear(0),
            })),
        ];
        let account_id: AccountId = "example.testnet".parse().unwrap();
        let size = estimate_transaction_size(&actions, &account_id).unwrap();

        // Must be at least the raw payload size (wasm + args).
        assert!(size >= wasm.len() + args.len());
        // And must include the wrapper overhead + 2× account_id bytes.
        let actions_bytes = borsh::to_vec(&actions).unwrap();
        assert_eq!(
            size,
            actions_bytes.len() + TX_WRAPPER_OVERHEAD_BYTES + 2 * account_id.as_str().len(),
        );
    }
}
