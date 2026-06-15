//! State-reading helpers around the `view_state` RPC.
//!
//! `read_state` is the preflight: it returns every key/value-byte-count for
//! the target account so the wipe transaction can attach the right gas
//! budget and pass every key into `clean()`.

use color_eyre::eyre::{Result, eyre};
use near_jsonrpc_client::JsonRpcClient;
use near_jsonrpc_client::errors::{JsonRpcError, JsonRpcServerError};
use near_jsonrpc_client::methods::query::{RpcQueryError, RpcQueryRequest};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use near_primitives::types::{AccountId, BlockReference, Finality, StoreKey};
use near_primitives::views::QueryRequest;

use crate::plan::StateEntry;

/// Fetch every storage entry on `account_id` as decoded `StateEntry`s.
///
/// `ViewState` returns values serialized as base64, but the RPC client's
/// deserializer already converts them back to raw bytes — we keep the key
/// bytes and discard the value, retaining only its byte length for gas
/// estimation.
pub async fn read_state(
    client: &JsonRpcClient,
    account_id: &AccountId,
) -> Result<Vec<StateEntry>> {
    let response = client
        .call(RpcQueryRequest {
            block_reference: BlockReference::Finality(Finality::Final),
            request: QueryRequest::ViewState {
                account_id: account_id.clone(),
                prefix: StoreKey::from(Vec::new()),
                include_proof: false,
            },
        })
        .await
        .map_err(map_view_state_error)?;

    let QueryResponseKind::ViewState(state) = response.kind else {
        return Err(eyre!(
            "Unexpected RPC response kind for ViewState on <{account_id}>",
        ));
    };

    Ok(state
        .values
        .into_iter()
        .map(|kv| {
            let value_bytes = kv.value.len();
            let key: Vec<u8> = kv.key.into();
            StateEntry { key, value_bytes }
        })
        .collect())
}

/// The 50 KB ViewState cap is enforced by most public RPCs (FastNEAR /
/// official). Detect it via the typed handler variant and rewrite to a
/// message that points the user at the fix.
fn map_view_state_error(err: JsonRpcError<RpcQueryError>) -> color_eyre::eyre::Report {
    if let JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
        RpcQueryError::TooLargeContractState { .. },
    )) = &err
    {
        return eyre!(
            "Account state is too large for this RPC's `view_state` cap.\n\
             Try configuring a different RPC with `near config edit-connection` and retry.",
        );
    }
    eyre!("Failed to fetch ViewState: {err}")
}
