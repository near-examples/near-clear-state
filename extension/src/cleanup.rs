//! State-reading helpers around the `view_state` RPC.
//!
//! `read_state` is the preflight: it returns every key/value-byte-count for
//! the target account so the wipe transaction can attach the right gas
//! budget and pass every key into `clean()`.

use std::num::NonZeroU32;

use color_eyre::eyre::{Result, eyre};
use near_jsonrpc_client::JsonRpcClient;
use near_jsonrpc_client::errors::{JsonRpcError, JsonRpcServerError};
use near_jsonrpc_client::methods::query::{RpcQueryError, RpcQueryRequest};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use near_primitives::types::{AccountId, BlockId, BlockReference, Finality, StoreKey};
use near_primitives::views::QueryRequest;

use crate::plan::StateEntry;

/// Entries requested per `view_state` page. The node additionally caps every
/// page at ~50 KB, so the effective page size is usually smaller.
const VIEW_STATE_PAGE_ITEMS: NonZeroU32 = NonZeroU32::new(10_000).unwrap();

/// Fetch every storage entry on `account_id` as decoded `StateEntry`s.
///
/// Reads the state page by page (nearcore 2.13+ `view_state` pagination),
/// following the `last_key` cursor until the node reports no more entries.
/// Paginated requests are not subject to the ~50 KB per-account cap that most
/// public RPCs enforce on one-shot `view_state` calls, so this works for
/// arbitrarily large state. Every page after the first is read at the block
/// the first page resolved to, so the listing is a consistent snapshot.
///
/// Nodes that predate pagination ignore the paging fields and return the whole
/// (size-capped) state without a cursor; the loop then ends after one page and
/// the size-cap error, if any, surfaces as before.
///
/// `ViewState` returns values serialized as base64, but the RPC client's
/// deserializer already converts them back to raw bytes — we keep the key
/// bytes and discard the value, retaining only its byte length for gas
/// estimation.
pub async fn read_state(
    client: &JsonRpcClient,
    account_id: &AccountId,
) -> Result<Vec<StateEntry>> {
    let mut block_reference = BlockReference::Finality(Finality::Final);
    let mut after_key = None;
    let mut entries = Vec::new();
    loop {
        let response = client
            .call(RpcQueryRequest {
                block_reference: block_reference.clone(),
                request: QueryRequest::ViewState {
                    account_id: account_id.clone(),
                    prefix: StoreKey::from(Vec::new()),
                    include_proof: false,
                    after_key,
                    limit: Some(VIEW_STATE_PAGE_ITEMS),
                },
            })
            .await
            .map_err(map_view_state_error)?;

        let QueryResponseKind::ViewState(page) = response.kind else {
            return Err(eyre!(
                "Unexpected RPC response kind for ViewState on <{account_id}>",
            ));
        };

        entries.extend(page.values.into_iter().map(|kv| {
            let value_bytes = kv.value.len();
            let key: Vec<u8> = kv.key.into();
            StateEntry { key, value_bytes }
        }));

        match page.last_key {
            Some(last_key) => {
                after_key = Some(last_key);
                block_reference = BlockReference::BlockId(BlockId::Hash(response.block_hash));
            }
            None => return Ok(entries),
        }
    }
}

/// Paginated requests skip the per-account size cap, so this only fires
/// against RPC nodes that predate `view_state` pagination (nearcore < 2.13).
/// Detect it via the typed handler variant and rewrite to a message that
/// points the user at the fix.
fn map_view_state_error(err: JsonRpcError<RpcQueryError>) -> color_eyre::eyre::Report {
    if let JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
        RpcQueryError::TooLargeContractState { .. },
    )) = &err
    {
        return eyre!(
            "Account state is too large for this RPC's `view_state` cap, and the RPC \
             does not support paginated `view_state` (nearcore 2.13+).\n\
             Try configuring a different RPC with `near config edit-connection` and retry.",
        );
    }
    eyre!("Failed to fetch ViewState: {err}")
}
