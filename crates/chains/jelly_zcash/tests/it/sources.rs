use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use jelly_core::{
    Amount, ChainAdapter as _, ChainError, CursorScope, CursorStore as _, KeyMaterial,
    ReservationSlot, ScriptKind, SendAmount, TxRequest,
    test_util::{RefusingCursorStore, WatchedAddressBuilder},
};
use jelly_keypair::Secp256k1Key;
use jelly_zcash::{ENTRY, ZcashAdapter, ZcashError};
use serde_json::json;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;
use zcash_transparent::bundle::OutPoint;

use super::common::{CHAIN, HEIGHT, Node, OWNER, document, key, request, transaction, watched};

const SECOND_ID: i64 = 2;
const SECOND_COIN_ZAT: u64 = 300_000;

fn second_key() -> Result<KeyMaterial, ZcashError> {
    Ok(Secp256k1Key::from_secret_bytes(&[9; 32])?.key_material())
}

/// The owner, address 1, holding its 1 000 000 zatoshi coin, and a second address of
/// the wallet holding 300 000, both watched.
async fn two_sources() -> Result<(Arc<Node>, ZcashAdapter, String), Box<dyn Error>> {
    let node = Node::new()?;
    let second = ENTRY.address(&second_key()?, Some(ScriptKind::P2pkh))?;
    let coin = transaction(OutPoint::new([43; 32], 0), &second, SECOND_COIN_ZAT)?;
    let txid = coin.txid().to_string();
    {
        let mut state = node.state.lock();
        state.transactions.insert(txid.clone(), document(&coin, Some(HEIGHT))?);
        state
            .address_coins
            .insert(second.clone(), json!([{"txid":txid,"vout":0,"value":"300000"}]));
    }
    let adapter = node.adapter().await?;
    adapter
        .set_watch_set(&[watched(), WatchedAddressBuilder::new(SECOND_ID, CHAIN, &second).build()])
        .await?;
    Ok((node, adapter, second))
}

fn pooled(sources: &[i64], amount_zat: u128) -> TxRequest {
    TxRequest {
        from_address_ids: sources.to_vec(),
        amount: SendAmount::Exact { value: Amount::from_base_units(amount_zat) },
        ..request()
    }
}

/// Neither coin alone pays, so the payment spends both, each signed by the key of the
/// address that holds it, returns its change to the first source, and is reserved
/// under both before it is broadcast.
#[tokio::test]
async fn a_payment_pools_the_coins_of_its_sources_with_change_to_the_first() {
    let (node, adapter, second) = two_sources().await.unwrap();
    let request = pooled(&[1, SECOND_ID], 1_200_000);
    assert_eq!(adapter.quote_fee(&request).await.unwrap().fee.base_units(), 10_000);
    let unsigned = adapter.build_tx(&request).await.unwrap();
    assert_eq!(unsigned.preview.from, [OWNER.to_owned(), second.clone()]);
    assert_eq!(unsigned.preview.amount.base_units(), 1_200_000);

    let signed = adapter.sign_tx(&unsigned, &[key().unwrap(), second_key().unwrap()]).unwrap();
    let decoded = Transaction::read(signed.raw.as_bytes(), BranchId::Sprout).unwrap();
    let bundle = decoded.transparent_bundle().unwrap();
    assert_eq!(bundle.vin.len(), 2);
    assert_eq!(u64::from(bundle.vout[1].value()), 90_000);
    let owner_script = ENTRY.codec().script_pubkey(OWNER).unwrap();
    assert_eq!(bundle.vout[1].script_pubkey().0.0, owner_script, "change goes to the first");

    let swapped = adapter.sign_tx(&unsigned, &[second_key().unwrap(), key().unwrap()]);
    assert!(matches!(swapped, Err(ChainError::InvalidInput(..))), "{swapped:?}");

    assert_eq!(adapter.broadcast(&signed).await.unwrap(), signed.txid);
    let raw_hex = hex::encode(signed.raw.as_bytes());
    for address_id in [1, SECOND_ID] {
        let saved = node
            .cursors
            .load_reservation(CursorScope::Address(address_id), ReservationSlot::ZcashSends)
            .await
            .unwrap()
            .expect("a reservation");
        assert!(saved.with_bytes(|bytes| String::from_utf8_lossy(bytes).contains(&raw_hex)));
    }
    let again = adapter.quote_fee(&request).await.unwrap_err();
    assert!(matches!(again, ChainError::InsufficientFunds { .. }), "{again:?}");
}

/// The first source receives the change even when the selection needed none of its
/// coins, and it then signs nothing.
#[tokio::test]
async fn a_first_source_with_no_coin_spent_still_receives_the_change() {
    let (_node, adapter, second) = two_sources().await.unwrap();
    let unsigned = adapter.build_tx(&pooled(&[SECOND_ID, 1], 500_000)).await.unwrap();
    assert_eq!(unsigned.preview.from, [second.clone(), OWNER.to_owned()]);
    let signed = adapter.sign_tx(&unsigned, &[second_key().unwrap(), key().unwrap()]).unwrap();
    let decoded = Transaction::read(signed.raw.as_bytes(), BranchId::Sprout).unwrap();
    let bundle = decoded.transparent_bundle().unwrap();
    assert_eq!(bundle.vin.len(), 1);
    let second_script = ENTRY.codec().script_pubkey(&second).unwrap();
    assert_eq!(bundle.vout[1].script_pubkey().0.0, second_script);
    assert_eq!(u64::from(bundle.vout[1].value()), 490_000);
}

/// A send-max quotes the total of every source.
#[tokio::test]
async fn a_send_max_spends_every_source() {
    let (_node, adapter, _second) = two_sources().await.unwrap();
    let request =
        TxRequest { from_address_ids: vec![1, SECOND_ID], amount: SendAmount::Max, ..request() };
    let quote = adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.max_amount.unwrap().base_units(), 1_300_000 - 10_000);
}

#[tokio::test]
async fn a_failed_send_write_leaves_every_source_releasable() {
    let (node, _, second) = two_sources().await.unwrap();
    let cursors = Arc::new(RefusingCursorStore::default());
    let adapter = node.adapter_over(Arc::<RefusingCursorStore>::clone(&cursors));
    adapter
        .set_watch_set(&[watched(), WatchedAddressBuilder::new(SECOND_ID, CHAIN, &second).build()])
        .await
        .unwrap();
    let request = pooled(&[1, SECOND_ID], 1_200_000);
    let unsigned = adapter.build_tx(&request).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap(), second_key().unwrap()]).unwrap();
    cursors.reservation_refusal().store(true, Ordering::SeqCst);
    adapter.broadcast(&signed).await.unwrap_err();
    assert!(node.state.lock().submissions.is_empty());
    adapter.pending_tracker().unwrap().discard(&unsigned).await.unwrap();
    cursors.reservation_refusal().store(false, Ordering::SeqCst);
    adapter.build_tx(&request).await.unwrap();
}

#[tokio::test]
async fn a_payment_from_no_source_or_an_unwatched_one_is_refused() {
    let (_node, adapter, _second) = two_sources().await.unwrap();
    for sources in [&[][..], &[1, 99][..]] {
        let error = adapter.quote_fee(&pooled(sources, 500_000)).await.unwrap_err();
        assert!(matches!(error, ChainError::InvalidInput(..)), "{sources:?}: {error:?}");
    }
}
