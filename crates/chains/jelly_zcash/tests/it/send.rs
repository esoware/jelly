use jelly_core::{
    Amount, ChainAdapter as _, ChainError, FeeLevel, FeeSelection, SendAmount, TxPayload,
    TxWarningKind,
};
use jelly_keypair::Secp256k1Key;
use serde_json::json;
use zcash_primitives::transaction::{Transaction, TxVersion};
use zcash_protocol::consensus::BranchId;
use zcash_transparent::bundle::OutPoint;

use super::common::{
    HEIGHT, Node, OWNER, RECIPIENT, assert_refused, document, key, pay_to_owner_pubkey, request,
    transaction,
};

#[tokio::test]
async fn quotes_builds_signs_and_broadcasts_a_v5_transparent_payment() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    let request = request();
    let quote = adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.fee.base_units(), 10_000);
    let unsigned = adapter.build_tx(&request).await.unwrap();
    assert_eq!(unsigned.preview.amount.base_units(), 500_000);
    assert_eq!(unsigned.preview.fee, quote.fee);
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    let decoded = Transaction::read(signed.raw.as_bytes(), BranchId::Sprout).unwrap();
    assert_eq!(decoded.version(), TxVersion::V5);
    assert_eq!(decoded.txid().to_string(), signed.txid.value());
    assert_eq!(u32::from(decoded.expiry_height()), HEIGHT.saturating_add(41));
    assert!(decoded.sapling_bundle().is_none());
    assert!(decoded.orchard_bundle().is_none());
    let bundle = decoded.transparent_bundle().unwrap();
    assert_eq!(bundle.vin.len(), 1);
    assert_eq!(bundle.vout.len(), 2);
    assert_eq!(u64::from(bundle.vout[0].value()), 500_000);
    assert_eq!(u64::from(bundle.vout[1].value()), 490_000);
    assert_eq!(adapter.broadcast(&signed).await.unwrap(), signed.txid);
    assert_eq!(node.state.lock().submissions.len(), 1);
}

#[tokio::test]
async fn send_max_p2sh_and_dust_change_use_conventional_fees() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    request.amount = SendAmount::Max;
    request.to = String::from("t3RD6RFKhWSotNbPEY4Vw7Ku9QCfKkzrbBL");
    assert_eq!(
        adapter.quote_fee(&request).await.unwrap().max_amount.unwrap().base_units(),
        990_000
    );
    let unsigned = adapter.build_tx(&request).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    let decoded = Transaction::read(signed.raw.as_bytes(), BranchId::Sprout).unwrap();
    assert_eq!(decoded.transparent_bundle().unwrap().vout.len(), 1);
    assert_eq!(decoded.transparent_bundle().unwrap().vout[0].script_pubkey().0.0.len(), 23);
    adapter.pending_tracker().unwrap().discard(&unsigned).await.unwrap();
    request.amount = SendAmount::Exact { value: Amount::from_base_units(989_700) };
    let unsigned = adapter.build_tx(&request).await.unwrap();
    assert_eq!(unsigned.preview.fee.base_units(), 10_300);
    assert!(
        unsigned
            .preview
            .warnings
            .iter()
            .any(|warning| warning.kind == TxWarningKind::DroppedChange)
    );
}

#[tokio::test]
async fn a_custom_fee_is_paid_as_given_between_the_zip_317_fee_and_four_times_it() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    let quote = adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.custom, Some(FeeSelection::Total { fee: Amount::from_base_units(10_000) }));
    let levels: Vec<(FeeLevel, u128)> =
        quote.levels.iter().map(|row| (row.level, row.fee.base_units())).collect();
    assert_eq!(levels, [(FeeLevel::Normal, 10_000)]);

    request.fee = FeeSelection::Total { fee: Amount::from_base_units(9_999) };
    assert_refused(
        &adapter.quote_fee(&request).await.unwrap_err(),
        "The fee of 0.00009999 ZEC is below the ZIP 317 fee of 0.0001 ZEC for this payment.",
    );
    request.fee = FeeSelection::Total { fee: Amount::from_base_units(40_001) };
    assert_refused(
        &adapter.quote_fee(&request).await.unwrap_err(),
        "The fee can be at most 0.0004 ZEC for this payment. A higher fee confirms no sooner.",
    );

    request.fee = FeeSelection::Total { fee: Amount::from_base_units(25_000) };
    let quote = adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.fee.base_units(), 25_000);
    assert_eq!(quote.levels.first().map(|row| row.fee.base_units()), Some(10_000));
    let unsigned = adapter.build_tx(&request).await.unwrap();
    assert_eq!(unsigned.preview.fee.base_units(), 25_000);
    assert!(
        unsigned
            .preview
            .details
            .iter()
            .any(|row| row.label == "ZIP 317 fee" && row.value == "0.0001 ZEC"),
        "{:?}",
        unsigned.preview.details
    );
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    let decoded = Transaction::read(signed.raw.as_bytes(), BranchId::Sprout).unwrap();
    let bundle = decoded.transparent_bundle().unwrap();
    assert_eq!(u64::from(bundle.vout[1].value()), 475_000, "the change pays the custom fee");
}

#[tokio::test]
async fn a_send_max_at_a_custom_fee_drops_no_change() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    request.amount = SendAmount::Max;
    request.fee = FeeSelection::Total { fee: Amount::from_base_units(20_000) };
    let unsigned = adapter.build_tx(&request).await.unwrap();
    assert_eq!(unsigned.preview.amount.base_units(), 980_000);
    assert_eq!(unsigned.preview.fee.base_units(), 20_000);
    assert!(
        unsigned
            .preview
            .warnings
            .iter()
            .all(|warning| warning.kind != TxWarningKind::DroppedChange),
        "{:?}",
        unsigned.preview.warnings
    );
}

#[tokio::test]
async fn fee_scales_with_inputs_and_never_uses_coinbase_outputs() {
    let node = Node::new().unwrap();
    let mut rows = Vec::new();
    for index in 0..3 {
        let tx = transaction(OutPoint::new([42; 32], index), OWNER, 10_000).unwrap();
        let txid = tx.txid().to_string();
        node.state.lock().transactions.insert(txid.clone(), document(&tx, Some(HEIGHT)).unwrap());
        rows.push(json!({"txid":txid,"vout":0,"value":"10000"}));
    }
    node.state.lock().coins = rows.into();
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    request.amount = SendAmount::Max;
    let quote = adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.fee.base_units(), 15_000);
    assert_eq!(quote.max_amount.unwrap().base_units(), 15_000);
    let reward = transaction(OutPoint::NULL, OWNER, 1_000_000).unwrap();
    let txid = reward.txid().to_string();
    node.state.lock().transactions.insert(txid.clone(), document(&reward, Some(1)).unwrap());
    node.state.lock().coins = json!([{"txid":txid,"vout":0,"value":"1000000"}]);
    let error = adapter.quote_fee(&request).await.unwrap_err();
    assert!(matches!(error, ChainError::InsufficientFunds { .. }), "{error:?}");
}

#[tokio::test]
async fn a_pay_to_pubkey_output_listed_for_the_address_does_not_block_a_send() {
    let node = Node::new().unwrap();
    let parent = transaction(OutPoint::NULL, RECIPIENT, 20_000).unwrap();
    let gift = pay_to_owner_pubkey(OutPoint::new(*parent.txid().as_ref(), 0), 10_000).unwrap();
    let txid = gift.txid().to_string();
    {
        let mut state = node.state.lock();
        state.transactions.insert(txid.clone(), document(&gift, Some(HEIGHT)).unwrap());
        state.coins.as_array_mut().unwrap().push(json!({"txid":txid,"vout":0,"value":"10000"}));
    }
    let adapter = node.adapter().await.unwrap();
    let quote = adapter.quote_fee(&request()).await;
    assert!(
        quote.is_ok(),
        "expected a 500000 zatoshi payment to be quoted from the 1000000 zatoshi key-hash coin \
         when Blockbook also lists a pay-to-pubkey output for the address, got {quote:?}"
    );
}

#[tokio::test]
async fn an_amount_below_the_output_minimum_is_refused_without_asking_for_a_rebuild() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    request.amount = SendAmount::Exact { value: Amount::from_base_units(1) };
    let error = adapter.quote_fee(&request).await.unwrap_err();
    let ChainError::InvalidInput(message, _) = &error else {
        panic!("expected an invalid input refusal for a one zatoshi payment, got {error:?}");
    };
    assert!(
        !message.contains("Build it again"),
        "expected the refusal of a one zatoshi payment to say the amount is below the minimum \
         output, because building it again fails the same way, but it says {message:?}"
    );
}

#[tokio::test]
async fn signing_refuses_a_changed_preview_payload_or_key() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let mut altered = unsigned.clone();
    altered.preview.amount = Amount::from_base_units(499_999);
    assert_refused(
        &adapter.sign_tx(&altered, &[key().unwrap()]).unwrap_err(),
        "The Zcash payment no longer matches its preview.",
    );
    let wrong_key = Secp256k1Key::from_secret_bytes(&[9; 32]).unwrap().key_material();
    assert_refused(
        &adapter.sign_tx(&unsigned, &[wrong_key]).unwrap_err(),
        "The signing key does not own these Zcash inputs.",
    );
    let mut payload: serde_json::Value =
        serde_json::from_slice(unsigned.payload.as_bytes()).unwrap();
    payload["fee_zat"] = 900_000.into();
    altered = unsigned;
    altered.payload = TxPayload::from_bytes(serde_json::to_vec(&payload).unwrap());
    assert_refused(
        &adapter.sign_tx(&altered, &[key().unwrap()]).unwrap_err(),
        "The Zcash payment is not valid. Build it again.",
    );
}

#[tokio::test]
async fn false_prevout_values_and_duplicate_outpoints_are_refused() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    node.state.lock().coins[0]["value"] = "1000001".into();
    let error = adapter.quote_fee(&request()).await.unwrap_err();
    assert!(matches!(error, ChainError::Rpc(_)), "{error:?}");
    {
        let mut state = node.state.lock();
        state.coins[0]["value"] = "1000000".into();
        let duplicate = state.coins[0].clone();
        state.coins.as_array_mut().unwrap().push(duplicate);
    }
    let error = adapter.quote_fee(&request()).await.unwrap_err();
    assert!(matches!(error, ChainError::Rpc(_)), "{error:?}");
}

#[tokio::test]
async fn a_slightly_lagging_indexer_uses_its_own_tip_but_large_lag_is_refused() {
    let node = Node::new().unwrap();
    node.state.lock().backend_lead_blocks = 3;
    let adapter = node.adapter().await.unwrap();
    assert_eq!(adapter.quote_fee(&request()).await.unwrap().fee.base_units(), 10_000);
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(unsigned.payload.as_bytes()).unwrap();
    assert_eq!(payload["target_height"], HEIGHT.saturating_add(1));
    assert_eq!(payload["expiry_height"], HEIGHT.saturating_add(41));
    node.state.lock().backend_lead_blocks = 11;
    assert_refused(
        &adapter.quote_fee(&request()).await.unwrap_err(),
        "The Zcash indexer is catching up. Try again shortly.",
    );
}

#[tokio::test]
async fn stale_nodes_unknown_upgrades_and_shielded_destinations_fail_closed() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    node.state.lock().block_age_secs = 3_601;
    assert_refused(
        &adapter.build_tx(&request()).await.unwrap_err(),
        "The Zcash endpoint is not up to date. Try again shortly.",
    );
    node.state.lock().block_age_secs = 0;
    node.state.lock().branch = Some(String::from("ffffffff"));
    assert_refused(
        &adapter.build_tx(&request()).await.unwrap_err(),
        "This Zcash network upgrade is not supported. Update Jelly.",
    );
    node.state.lock().branch = None;
    node.state.lock().is_synced = false;
    assert_refused(
        &adapter.build_tx(&request()).await.unwrap_err(),
        "The Zcash endpoint is not synchronized. Try another endpoint.",
    );
    node.state.lock().is_synced = true;
    for destination in ["u1unified", "zs1shielded", "zcash:?address.1=ignored"] {
        let mut request = request();
        request.to = destination.to_owned();
        assert_refused(
            &adapter.build_tx(&request).await.unwrap_err(),
            "Enter a Zcash transparent mainnet address. Shielded and unified addresses are not \
             supported.",
        );
    }
}
