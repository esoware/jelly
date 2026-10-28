use jelly_core::{FeeQuote, FeeSelection, PendingTxId, TxId, TxRequest};
use jelly_engine::{PreparedTx, PreviewDigest, TxPage, TxQuery};
use secrecy::SecretString;
use tauri::State;

use crate::command::{open_explorer, with_engine};
use crate::error::IpcError;
use crate::state::AppState;

const TXID_PLACEHOLDER: &str = "{txid}";

const MAX_TXID_BYTES: usize = 128;

/// Prices the request's fee selection without building a transaction or reserving funds.
#[tauri::command]
#[specta::specta]
pub(crate) async fn quote_fees(
    state: State<'_, AppState>,
    request: TxRequest,
) -> Result<FeeQuote, IpcError> {
    with_engine(&state, async move |engine| engine.quote_fees(request).await).await
}

/// Builds a transaction and binds its signing authorization to the returned preview digest.
#[tauri::command]
#[specta::specta]
pub(crate) async fn prepare_tx(
    state: State<'_, AppState>,
    request: TxRequest,
) -> Result<PreparedTx, IpcError> {
    with_engine(&state, async move |engine| engine.prepare_tx(request).await).await
}

/// Requires the approved preview digest. A wrong password or a signing failure leaves the
/// authorization usable, and a failed broadcast consumes it.
#[tauri::command]
#[specta::specta]
pub(crate) async fn submit_tx(
    state: State<'_, AppState>,
    pending_id: PendingTxId,
    password: String,
    preview_digest: PreviewDigest,
) -> Result<TxId, IpcError> {
    let password = SecretString::from(password);
    with_engine(&state, async move |engine| {
        engine.submit_tx(pending_id, password, preview_digest).await
    })
    .await
}

/// Prices a fee bump at `fee` without reserving funds.
///
/// The bump is a replacement, or a child spending the send's change on a chain whose nodes
/// don't relay replacements. A fee level is raised to the network's minimum for the bump.
#[tauri::command]
#[specta::specta]
pub(crate) async fn quote_bump(
    state: State<'_, AppState>,
    txid: TxId,
    fee: FeeSelection,
) -> Result<FeeQuote, IpcError> {
    with_engine(&state, async move |engine| engine.quote_bump(&txid, &fee).await).await
}

/// Builds a fee bump using only the price from `fee`. The original transaction supplies
/// the recipient, amount, and reserved inputs or nonce, or the change a child spends.
#[tauri::command]
#[specta::specta]
pub(crate) async fn bump_fee(
    state: State<'_, AppState>,
    txid: TxId,
    fee: FeeSelection,
) -> Result<PreparedTx, IpcError> {
    with_engine(&state, async move |engine| engine.bump_fee(&txid, &fee).await).await
}

/// Prices a cancel at `fee` without reserving funds. A fee level is raised to the network's
/// replacement minimum.
#[tauri::command]
#[specta::specta]
pub(crate) async fn quote_cancel(
    state: State<'_, AppState>,
    txid: TxId,
    fee: FeeSelection,
) -> Result<FeeQuote, IpcError> {
    with_engine(&state, async move |engine| engine.quote_cancel(&txid, &fee).await).await
}

/// Builds a replacement that pays nothing to anyone, using only the price from `fee`. The
/// original transaction supplies the sender and the nonce.
#[tauri::command]
#[specta::specta]
pub(crate) async fn cancel_tx(
    state: State<'_, AppState>,
    txid: TxId,
    fee: FeeSelection,
) -> Result<PreparedTx, IpcError> {
    with_engine(&state, async move |engine| engine.cancel_tx(&txid, &fee).await).await
}

/// Abandons an authorization, releasing whatever the adapter reserved for it.
#[tauri::command]
#[specta::specta]
pub(crate) async fn delete_pending_tx(
    state: State<'_, AppState>,
    pending_id: PendingTxId,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.delete_pending_tx(pending_id).await).await
}

/// Returns one keyset page of the matching transactions.
#[tauri::command]
#[specta::specta]
pub(crate) async fn list_txs(
    state: State<'_, AppState>,
    query: TxQuery,
) -> Result<TxPage, IpcError> {
    with_engine(&state, async move |engine| engine.list_txs(query).await).await
}

/// Opens the chain's built-in transaction explorer, and only for a plain txid.
#[tauri::command]
#[specta::specta]
pub(crate) async fn open_tx_explorer(txid: TxId) -> Result<(), IpcError> {
    let value = txid.value();
    let is_plain = !value.is_empty()
        && value.len() <= MAX_TXID_BYTES
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric());
    let template =
        jelly_chains::chains().spec(txid.chain()).explorer_tx_url.filter(|_| is_plain).ok_or_else(
            || IpcError::invalid_input("This transaction does not have a valid explorer link."),
        )?;
    open_explorer(template.replace(TXID_PLACEHOLDER, value)).await
}

#[cfg(test)]
mod tests {
    use jelly_core::{ChainId, TxId};

    use super::{MAX_TXID_BYTES, open_tx_explorer};
    use crate::error::IpcError;

    #[tokio::test]
    async fn a_txid_that_could_change_the_explorer_destination_is_refused() {
        let oversized = "a".repeat(MAX_TXID_BYTES.saturating_add(1));
        for chain in ChainId::ALL {
            for value in [
                "",
                "..",
                "../address/other",
                "abc?redirect=elsewhere",
                "abc#other",
                "abc%2fother",
                "abc\\other",
                "abc\" --other",
                "abc & other",
                "delta:0xabc:123",
                "https://example.invalid/",
                "file:///C:/example",
                "abc\nother",
                "abc\0other",
                "\u{200b}abc",
                &oversized,
            ] {
                let error = open_tx_explorer(TxId::new(chain, value)).await.unwrap_err();
                assert_eq!(
                    error,
                    IpcError::invalid_input(
                        "This transaction does not have a valid explorer link."
                    )
                );
            }
        }
    }
}
