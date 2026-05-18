//! Gas estimation for the state-cleanup wipe flow.
//!
//! Gas constants are pulled from the live chain via
//! `EXPERIMENTAL_protocol_config` rather than hardcoded.
//!
//! Current model: the entire wipe goes in **one** transaction — one
//! `DeployContract` action + one `FunctionCall("clean", all_keys)`
//! action with the full gas budget attached. With protocol version 83
//! lifting the per-tx cap to 1000 Tgas, a single call can clear
//! several megabytes of contract state, which comfortably covers any
//! state size that `view_state` is willing to return.
//!
//! [`plan_batches`] is retained for the future multi-tx fallback —
//! useful when a permissive RPC returns a state larger than one tx
//! can wipe.

use color_eyre::eyre::Result;
use near_jsonrpc_client::JsonRpcClient;
use near_jsonrpc_client::methods::EXPERIMENTAL_protocol_config::RpcProtocolConfigError;
use near_primitives::types::{BlockReference, Finality};
use serde::Deserialize;

/// Gas attached to the single `clean(keys=[...])` call. 990 Tgas leaves
/// ~10 Tgas of slack under the protocol's 1000 Tgas per-tx cap to cover
/// action-level overhead (function-call base ~1 Ggas, receipt creation
/// ~216 Mgas) plus headroom for future protocol param changes.
pub const MAX_CLEAN_GAS: u128 = 990_000_000_000_000;

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
}

/// Fetch the live `storage_remove_*` costs from the chain. Reads only the
/// fields we need so the deserialization isn't coupled to the rest of the
/// runtime-config shape.
pub async fn fetch_gas_constants(client: &JsonRpcClient) -> Result<GasConstants> {
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
    Ok(response.runtime_config.wasm_config.ext_costs)
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

/// Pack entries into batches, each ≤ `target_gas`. Streaming-greedy:
/// process in arrival order, close the current batch when adding the
/// next entry would overflow. A single oversized entry (one whose
/// `est > target_gas` alone) is placed in its own batch — the
/// `!current.is_empty()` guard prevents an infinite "close empty,
/// start empty, overflow again" loop.
pub fn plan_batches(
    entries: &[StateEntry],
    target_gas: u128,
    c: &GasConstants,
) -> Vec<Vec<Vec<u8>>> {
    let mut batches: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut current: Vec<Vec<u8>> = Vec::new();
    let mut current_gas: u128 = 0;

    for entry in entries {
        let est = estimate_key_gas(entry.key.len(), entry.value_bytes, c);
        if current_gas + est > target_gas && !current.is_empty() {
            batches.push(std::mem::take(&mut current));
            current_gas = 0;
        }
        current.push(entry.key.clone());
        current_gas += est;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-batch budget the planner is exercised against in these
    /// tests — small enough to force splitting on the inputs used.
    /// Independent of the production `MAX_CLEAN_GAS` constant.
    const TEST_TARGET_GAS: u128 = 28_000_000_000_000;

    // Sample constants matching what we observed empirically from
    // testnet's protocol_config (close to nearcore defaults).
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

    fn entry(k: usize, v: usize) -> StateEntry {
        StateEntry { key: vec![0; k], value_bytes: v }
    }

    #[test]
    fn estimate_total_gas_is_sum_of_per_key() {
        let c = constants();
        let entries = vec![entry(10, 50), entry(20, 200), entry(40, 4096)];
        let expected: u128 = entries
            .iter()
            .map(|e| estimate_key_gas(e.key.len(), e.value_bytes, &c))
            .sum();
        assert_eq!(estimate_total_gas(&entries, &c), expected);
        assert_eq!(estimate_total_gas(&[], &c), 0);
    }

    #[test]
    fn empty_input_yields_no_batches() {
        let batches = plan_batches(&[], TEST_TARGET_GAS, &constants());
        assert!(batches.is_empty());
    }

    #[test]
    fn small_entries_pack_into_one_batch() {
        let entries: Vec<_> = (0..5).map(|i| entry(8 + i, 50)).collect();
        let batches = plan_batches(&entries, TEST_TARGET_GAS, &constants());
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 5);
    }

    #[test]
    fn splits_at_budget_boundary_for_uniform_entries() {
        let per_key = estimate_key_gas(20, 3072, &constants());
        let per_batch = (TEST_TARGET_GAS / per_key) as usize;
        let total = per_batch * 2 + 1;
        let entries: Vec<_> = (0..total).map(|_| entry(20, 3072)).collect();
        let batches = plan_batches(&entries, TEST_TARGET_GAS, &constants());
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), per_batch);
        assert_eq!(batches[1].len(), per_batch);
        assert_eq!(batches[2].len(), 1);
    }

    #[test]
    fn oversized_solo_entry_gets_its_own_batch() {
        // Derive the minimum value-bytes that pushes one entry above TARGET.
        let c = constants();
        let raw_cap = TEST_TARGET_GAS * 100 / SAFETY_FACTOR_PCT;
        let overhead = c.storage_remove_base + 10 * c.storage_remove_key_byte;
        let oversized = ((raw_cap - overhead) / c.storage_remove_ret_value_byte + 1) as usize;

        let entries = vec![
            entry(10, 50),
            entry(10, oversized),
            entry(10, 50),
        ];
        let batches = plan_batches(&entries, TEST_TARGET_GAS, &c);
        assert_eq!(batches.len(), 3);
        for b in &batches {
            assert_eq!(b.len(), 1);
        }
    }

    #[test]
    fn mixed_sizes_respect_target() {
        let entries = vec![
            entry(20, 500),
            entry(20, 8_000),
            entry(20, 800_000),
            entry(20, 800_000),
        ];
        let c = constants();
        let batches = plan_batches(&entries, TEST_TARGET_GAS, &c);

        // Re-derive each batch's gas from the original entries (we held
        // onto sizes; plan_batches returns only keys).
        let mut idx = 0;
        for batch in &batches {
            let batch_entries = &entries[idx..idx + batch.len()];
            idx += batch.len();
            if batch.len() == 1 {
                continue; // oversized solo allowed to exceed
            }
            let sum: u128 = batch_entries
                .iter()
                .map(|e| estimate_key_gas(e.key.len(), e.value_bytes, &c))
                .sum();
            assert!(sum < TEST_TARGET_GAS, "batch over target: {sum}");
        }
        let total_keys: usize = batches.iter().map(|b| b.len()).sum();
        assert_eq!(total_keys, entries.len());
    }
}
