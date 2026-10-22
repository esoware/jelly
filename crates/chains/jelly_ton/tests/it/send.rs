use std::slice::from_ref;

use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, KeyKind, ScriptKind, SendAmount,
    TokenId, TxWarningKind, test_util::key_material_of,
};
use jelly_ton::{Method, TonError};
use serde_json::{Value, json};
use tonlib_core::cell::Cell;
use tonlib_core::tlb_types::block::message::{CommonMsgInfo, Message};
use tonlib_core::tlb_types::primitives::either::EitherRefLayout;
use tonlib_core::tlb_types::tep::jetton::JettonTransferMessage;
use tonlib_core::tlb_types::tlb::TLB as _;
use tonlib_core::types::TonAddress;
use tonlib_core::wallet::versioned::v3::WalletExtMsgBodyV3;
use tonlib_core::wallet::versioned::v4::WalletExtMsgBodyV4;
use tonlib_core::wallet::versioned::v5::WalletExtMsgBodyV5;

use super::common::{
    Node, RECIPIENT, SECRET, ZERO_HASH, assert_refused, jetton_request, key, request, watched_as,
};

const MISMATCH: &str = "The TON payment does not match the approved preview or signing key.";
/// `MyTonWallet`'s attachment for a USDT transfer.
const USDT_ATTACHED_NANOTON: u128 = 18_000_000;

/// The one internal message a signed V5R1 external message carries, and its mode.
fn v5_message(raw: &[u8]) -> Result<(Message, u8), TonError> {
    let message = Message::from_boc(raw)?;
    let body = WalletExtMsgBodyV5::from_cell(&message.body.value)?;
    let (Some(first), Some(mode)) = (body.msgs.first(), body.msgs_modes.first()) else {
        return Err(TonError::Protocol);
    };
    Ok((Message::from_cell(first)?, *mode))
}

const fn bounce_flag(message: &Message) -> Option<bool> {
    if let CommonMsgInfo::Int(ref info) = message.info { Some(info.bounce) } else { None }
}

/// The bytes a snake holds: the data of `cell`, then of each first reference in turn.
fn snake_bytes(mut cell: &Cell) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        bytes.extend_from_slice(cell.data());
        let Some(next) = cell.references().first() else {
            return bytes;
        };
        cell = next;
    }
}

#[tokio::test]
async fn quote_build_sign_and_broadcast_match_the_ordinary_transfer_vector() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let quote = adapter.quote_fee(&request()).await.unwrap();
    assert_eq!(quote.fee.base_units(), 481_012);
    let tx = adapter.build_tx(&request()).await.unwrap();
    assert_eq!(tx.preview.fee, quote.fee);
    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    let vectors: Vec<Value> =
        serde_json::from_str(include_str!("../vectors/signing.json")).unwrap();
    let vector = vectors
        .iter()
        .find(|vector| {
            vector.get("name").and_then(Value::as_str)
                == Some("test_ton_sign_wallet_v5r1_transfer_ordinary")
        })
        .unwrap();
    assert_eq!(Some(signed.txid.value()), vector.get("normalized_hash").unwrap().as_str());
    adapter.broadcast(&signed).await.unwrap();
    assert_eq!(node.state.lock().submissions.len(), 1);
    assert_refused(
        &adapter.build_tx(&request()).await.unwrap_err(),
        "Wait for the pending TON payment to finish before sending again.",
    );
}

#[tokio::test]
async fn a_send_max_from_an_undeployed_wallet_deploys_it_and_sends_the_whole_balance() {
    let node = Node::new();
    node.state.lock().account_status = String::from("uninitialized");
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    request.amount = SendAmount::Max;
    let quote = adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.max_amount.unwrap().base_units(), 5_000_000_000 - quote.fee.base_units());
    let tx = adapter.build_tx(&request).await.unwrap();
    assert!(
        tx.preview.warnings.iter().any(|warning| warning.kind == TxWarningKind::AccountActivation)
    );
    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    let message = Message::from_boc(signed.raw.as_bytes()).unwrap();
    assert!(message.init.is_some());
    let body = WalletExtMsgBodyV5::from_cell(&message.body.value).unwrap();
    assert_eq!(body.msg_seqno, 0);
    assert_eq!(body.msgs_modes, [130]);
    adapter.broadcast(&signed).await.unwrap();
    let state = node.state.lock();
    let (_, estimate) =
        state.calls.iter().find(|(method, _)| *method == Method::EstimateFee).unwrap();
    assert!(!estimate.get("init_code").unwrap().as_str().unwrap().is_empty());
    assert!(!estimate.get("init_data").unwrap().as_str().unwrap().is_empty());
}

#[tokio::test]
async fn a_comment_from_the_request_or_the_field_is_signed_and_shown_but_not_both() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    for (to, comment) in [
        (format!("ton://transfer/{RECIPIENT}?text=order%20123"), None),
        (RECIPIENT.to_owned(), Some(String::from("order 123"))),
    ] {
        let mut request = request();
        request.to = to;
        request.comment = comment;
        let tx = adapter.build_tx(&request).await.unwrap();
        assert!(
            tx.preview
                .details
                .iter()
                .any(|detail| detail.label == "Comment" && detail.value == "order 123")
        );
        let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
        let (outgoing, _) = v5_message(signed.raw.as_bytes()).unwrap();
        let mut parser = outgoing.body.value.parser();
        assert_eq!(parser.load_u32(32).unwrap(), 0);
        assert_eq!(parser.load_bytes(9).unwrap(), b"order 123");
        adapter.pending_tracker().unwrap().discard(&tx).await.unwrap();
    }
    let mut request = request();
    request.to = format!("ton://transfer/{RECIPIENT}?text=order%20123");
    request.comment = Some(String::from("order 123"));
    for error in [
        adapter.quote_fee(&request).await.unwrap_err(),
        adapter.build_tx(&request).await.unwrap_err(),
    ] {
        assert_refused(
            &error,
            "The payment request already includes a comment. Clear this field to use it.",
        );
    }
}

#[tokio::test]
async fn long_utf8_comments_round_trip_through_snake_cells_up_to_the_limit() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    // Four-byte characters straddle the 123-byte root and most 127-byte cell boundaries,
    // and 256 of them fill the 1024-byte limit exactly.
    let comment = "\u{1f600}".repeat(256);
    for (to, explicit_comment) in [
        (format!("ton://transfer/{RECIPIENT}?text={comment}"), None),
        (RECIPIENT.to_owned(), Some(comment.clone())),
    ] {
        let mut request = request();
        request.to = to;
        request.comment = explicit_comment;
        let tx = adapter.build_tx(&request).await.unwrap();
        let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
        let (outgoing, _) = v5_message(signed.raw.as_bytes()).unwrap();
        let bytes = snake_bytes(&outgoing.body.value);
        assert_eq!(&bytes[..4], [0; 4]);
        assert_eq!(&bytes[4..], comment.as_bytes());
        adapter.pending_tracker().unwrap().discard(&tx).await.unwrap();
        let message = if let Some(comment) = request.comment.as_mut() {
            comment.push('a');
            "Use a comment of at most 1024 bytes."
        } else {
            request.to.push('a');
            "Enter a valid TON mainnet address or payment request."
        };
        assert_refused(&adapter.quote_fee(&request).await.unwrap_err(), message);
        assert_refused(&adapter.build_tx(&request).await.unwrap_err(), message);
    }
}

#[tokio::test]
async fn inactive_recipients_require_explicit_non_bounceable_addresses() {
    let node = Node::new();
    node.state.lock().recipient_status = String::from("uninitialized");
    let adapter = node.adapter().await.unwrap();
    assert_refused(
        &adapter.build_tx(&request()).await.unwrap_err(),
        "This recipient is not active. Ask for its non-bounceable address before sending.",
    );
    let mut request = request();
    request.to = TonAddress::from_base64_url(RECIPIENT).unwrap().to_base64_url_flags(true, false);
    adapter.build_tx(&request).await.unwrap();
}

#[tokio::test]
async fn a_raw_recipient_bounces_only_when_it_is_deployed() {
    let raw = TonAddress::from_base64_url(RECIPIENT).unwrap().to_hex();
    for (status, is_bounceable) in [("active", true), ("uninitialized", false), ("frozen", false)] {
        let node = Node::new();
        node.state.lock().recipient_status = String::from(status);
        let adapter = node.adapter().await.unwrap();
        let mut request = request();
        request.to = raw.clone();
        let tx = adapter.build_tx(&request).await.unwrap();
        assert_eq!(
            tx.preview.to,
            TonAddress::from_hex_str(&raw).unwrap().to_base64_url_flags(!is_bounceable, false),
            "{status}"
        );
        let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
        let (outgoing, _) = v5_message(signed.raw.as_bytes()).unwrap();
        assert_eq!(bounce_flag(&outgoing), Some(is_bounceable), "{status}");
    }
}

#[tokio::test]
async fn a_friendly_address_keeps_its_own_flag_whatever_the_recipient_state() {
    let node = Node::new();
    node.state.lock().recipient_status = String::from("active");
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    request.to = TonAddress::from_base64_url(RECIPIENT).unwrap().to_base64_url_flags(true, false);
    let tx = adapter.build_tx(&request).await.unwrap();
    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    assert_eq!(bounce_flag(&v5_message(signed.raw.as_bytes()).unwrap().0), Some(false));
}

#[tokio::test]
async fn stale_nodes_disabled_keys_and_excessive_fees_are_refused() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    node.state.lock().is_stale = true;
    assert_refused(
        &adapter.quote_fee(&request()).await.unwrap_err(),
        "The TON node is not up to date. Try again shortly.",
    );
    node.state.lock().is_stale = false;
    node.state.lock().is_signature_allowed = false;
    assert_refused(
        &adapter.quote_fee(&request()).await.unwrap_err(),
        "This TON account does not match an enabled V5R1 wallet.",
    );
    node.state.lock().is_signature_allowed = true;
    node.state.lock().fees["gas_fee"] = json!(100_000_000);
    assert_refused(
        &adapter.quote_fee(&request()).await.unwrap_err(),
        "The TON fee estimate is not reasonable. Try again later.",
    );
}

#[tokio::test]
async fn insufficient_funds_wrong_keys_and_changed_previews_are_refused() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    request.amount = SendAmount::Exact { value: Amount::from_base_units(5_000_000_000) };
    let error = adapter.build_tx(&request).await.unwrap_err();
    assert!(matches!(error, ChainError::InsufficientFunds { .. }), "{error:?}");
    request.amount = SendAmount::Exact { value: Amount::from_base_units(10) };
    let mut tx = adapter.build_tx(&request).await.unwrap();
    let wrong = key_material_of(KeyKind::Ed25519Seed, vec![2; 32]);
    assert_refused(&adapter.sign_tx(&tx, from_ref(&wrong)).unwrap_err(), MISMATCH);
    tx.preview.to = String::from("changed");
    assert_refused(&adapter.sign_tx(&tx, &[key().unwrap()]).unwrap_err(), MISMATCH);
    assert!(node.state.lock().submissions.is_empty());
}

#[tokio::test]
async fn older_wallet_versions_sign_in_their_own_layout_and_deploy_their_own_code() {
    for (script, is_deploy) in [
        (ScriptKind::TonV4r2, false),
        (ScriptKind::TonV4r2, true),
        (ScriptKind::TonV3r2, false),
        (ScriptKind::TonV3r2, true),
    ] {
        let watched = watched_as(SECRET, script).unwrap();
        let node = Node::new();
        {
            let mut state = node.state.lock();
            state.version = script;
            state.seqno = 4;
            if is_deploy {
                state.account_status = String::from("uninitialized");
            }
        }
        let adapter = node.adapter_watching(watched.clone()).await.unwrap();
        let tx = adapter.build_tx(&request()).await.unwrap();
        assert_eq!(tx.preview.from, [watched.address.value().to_owned()]);
        assert!(tx.preview.details.iter().any(|detail| detail.label == "Wallet contract"
            && detail.value == if script == ScriptKind::TonV4r2 { "V4R2" } else { "V3R2" }));
        let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
        let message = Message::from_boc(signed.raw.as_bytes()).unwrap();
        assert_eq!(message.init.is_some(), is_deploy, "{script}");
        let mut body = message.body.value.parser();
        body.load_bytes(64).unwrap();
        let unsigned = body.load_remaining().unwrap();
        let (seqno, modes) = if script == ScriptKind::TonV4r2 {
            let body = WalletExtMsgBodyV4::from_cell(&unsigned).unwrap();
            (body.msg_seqno, body.msgs_modes)
        } else {
            let body = WalletExtMsgBodyV3::from_cell(&unsigned).unwrap();
            (body.msg_seqno, body.msgs_modes)
        };
        assert_eq!((seqno, modes), (if is_deploy { 0 } else { 4 }, vec![3]), "{script}");
        adapter.broadcast(&signed).await.unwrap();
    }
}

#[tokio::test]
async fn a_v4r2_account_with_another_wallets_code_is_refused() {
    let watched = watched_as(SECRET, ScriptKind::TonV4r2).unwrap();
    let node = Node::new();
    node.state.lock().version = ScriptKind::TonV3r2;
    let adapter = node.adapter_watching(watched).await.unwrap();
    assert_refused(
        &adapter.build_tx(&request()).await.unwrap_err(),
        "This TON account does not match a V4R2 wallet.",
    );
}

fn deploy_usdt(node: &Node, balance: u128) -> Result<TonAddress, TonError> {
    let mut state = node.state.lock();
    let jetton = state.jetton(TokenId::UsdtJetton)?;
    jetton.status = String::from("active");
    jetton.balance = balance;
    jetton.position = json!({"lt":"7","hash":ZERO_HASH});
    Ok(jetton.wallet.clone())
}

#[tokio::test]
async fn a_jetton_transfer_follows_tep_74_and_attaches_what_mytonwallet_attaches() {
    let node = Node::new();
    let wallet = deploy_usdt(&node, 2_500_000).unwrap();
    let adapter = node.adapter().await.unwrap();
    let request = jetton_request(TokenId::UsdtJetton, 1_250_000);
    let quote = adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.fee.base_units(), 481_012 + USDT_ATTACHED_NANOTON);
    let tx = adapter.build_tx(&request).await.unwrap();
    assert_eq!(tx.preview.asset, AssetId::Token(TokenId::UsdtJetton));
    assert_eq!(tx.preview.amount.base_units(), 1_250_000);
    assert_eq!(tx.preview.to, RECIPIENT);
    assert_eq!(tx.preview.fee.base_units(), 481_012 + USDT_ATTACHED_NANOTON);
    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    let (outgoing, mode) = v5_message(signed.raw.as_bytes()).unwrap();
    assert_eq!(mode, 3, "the wallet pays the forwarding fees on top of the attachment");
    let CommonMsgInfo::Int(ref info) = outgoing.info else {
        panic!("expected an internal message");
    };
    assert_eq!(TonAddress::from_msg_address(info.dest.clone()).unwrap(), wallet);
    assert!(info.bounce, "a refused transfer returns to the sender");
    assert_eq!(u128::try_from(&info.value.grams.amount).unwrap(), USDT_ATTACHED_NANOTON);
    let transfer = JettonTransferMessage::from_cell(&outgoing.body.value).unwrap();
    assert_eq!(u128::try_from(&transfer.amount).unwrap(), 1_250_000);
    assert_eq!(
        TonAddress::from_msg_address(transfer.destination).unwrap(),
        TonAddress::from_base64_url(RECIPIENT).unwrap()
    );
    assert_eq!(
        TonAddress::from_msg_address(transfer.response_destination).unwrap(),
        TonAddress::from_base64_url(&tx.preview.from[0]).unwrap(),
        "the excess comes back to the sender"
    );
    assert_eq!(transfer.custom_payload, None);
    assert_eq!(u128::try_from(&transfer.forward_ton_amount).unwrap(), 1);
    assert_eq!(*transfer.forward_payload.value, Cell::default());
    assert!(
        matches!(transfer.forward_payload.layout, EitherRefLayout::ToCell),
        "the empty forward payload is the one zero bit written inline"
    );
    assert!(outgoing.body.value.references().is_empty());
    adapter.broadcast(&signed).await.unwrap();

    let other = Node::new();
    deploy_usdt(&other, 2_500_000).unwrap();
    let again = other.adapter().await.unwrap().build_tx(&request).await.unwrap();
    assert_ne!(
        again.payload.as_bytes(),
        tx.payload.as_bytes(),
        "each transfer draws its own query id"
    );
}

#[tokio::test]
async fn a_jetton_send_needs_the_tokens_and_the_gram_for_its_gas() {
    let node = Node::new();
    deploy_usdt(&node, 1_000).unwrap();
    let adapter = node.adapter().await.unwrap();
    let error = adapter.build_tx(&jetton_request(TokenId::UsdtJetton, 1_001)).await.unwrap_err();
    assert!(
        matches!(
            error,
            ChainError::InsufficientFunds { asset: AssetId::Token(TokenId::UsdtJetton), .. }
        ),
        "{error:?}"
    );
    node.state.lock().balance_nanogram = 18_400_000;
    let error = adapter.build_tx(&jetton_request(TokenId::UsdtJetton, 1_000)).await.unwrap_err();
    let ChainError::InsufficientFunds { asset, needed, available } = error else {
        panic!("expected a shortfall of Gram, got {error:?}");
    };
    assert_eq!(asset, AssetId::Native(ChainId::Ton));
    assert_eq!(
        (needed.base_units(), available.base_units()),
        (481_012 + USDT_ATTACHED_NANOTON, 18_400_000)
    );
    node.state.lock().balance_nanogram = 5_000_000_000;
    let mut max = jetton_request(TokenId::UsdtJetton, 0);
    max.amount = SendAmount::Max;
    let quote = adapter.quote_fee(&max).await.unwrap();
    assert_eq!(quote.max_amount.map(Amount::base_units), Some(1_000));
}

#[tokio::test]
async fn an_undeployed_jetton_wallet_holds_nothing_to_send() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let error = adapter.build_tx(&jetton_request(TokenId::XautJetton, 1)).await.unwrap_err();
    let ChainError::InsufficientFunds { asset, available, .. } = error else {
        panic!("expected a shortfall, got {error:?}");
    };
    assert_eq!((asset, available), (AssetId::Token(TokenId::XautJetton), Amount::ZERO));
}

#[tokio::test]
async fn a_jetton_wallet_that_names_another_owner_is_refused() {
    let node = Node::new();
    deploy_usdt(&node, 1_000).unwrap();
    node.state.lock().jetton(TokenId::UsdtJetton).unwrap().owner =
        Some(TonAddress::new(0, [9; 32].into()));
    let adapter = node.adapter().await.unwrap();
    let error = adapter.build_tx(&jetton_request(TokenId::UsdtJetton, 10)).await.unwrap_err();
    assert!(matches!(error, ChainError::Rpc(_)), "{error:?}");
}

/// The Gram a jetton transfer forwards and its comment, read back out of its signed
/// forward payload.
fn forward_comment(raw: &[u8]) -> Result<(u128, EitherRefLayout, Vec<u8>), TonError> {
    let (outgoing, _) = v5_message(raw)?;
    let transfer = JettonTransferMessage::from_cell(&outgoing.body.value)?;
    let forwarded = u128::try_from(&transfer.forward_ton_amount).map_err(|_| TonError::Protocol)?;
    Ok((forwarded, transfer.forward_payload.layout, snake_bytes(&transfer.forward_payload.value)))
}

#[tokio::test]
async fn a_jetton_comment_rides_in_the_forward_payload_the_recipient_is_notified_with() {
    let node = Node::new();
    deploy_usdt(&node, 1_000).unwrap();
    let adapter = node.adapter().await.unwrap();
    let mut request = jetton_request(TokenId::UsdtJetton, 10);
    request.to = format!("ton://transfer/{RECIPIENT}?text=deposit%2042");
    let tx = adapter.build_tx(&request).await.unwrap();
    assert!(
        tx.preview.details.iter().any(|row| row.label == "Comment" && row.value == "deposit 42")
    );
    assert!(!tx.preview.warnings.iter().any(|warning| warning.message.contains("No comment")));
    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    let (forwarded, layout, bytes) = forward_comment(signed.raw.as_bytes()).unwrap();
    assert_eq!(forwarded, 1, "one nanogram notifies the recipient");
    assert!(matches!(layout, EitherRefLayout::ToCell), "a short comment fits inline");
    assert_eq!(&bytes[..4], [0; 4]);
    assert_eq!(&bytes[4..], b"deposit 42");
    adapter.broadcast(&signed).await.unwrap();

    let other = Node::new();
    deploy_usdt(&other, 1_000).unwrap();
    let adapter = other.adapter().await.unwrap();
    let comment = "\u{1f600}".repeat(256);
    request.to = RECIPIENT.to_owned();
    request.comment = Some(comment.clone());
    let tx = adapter.build_tx(&request).await.unwrap();
    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    let (_, layout, bytes) = forward_comment(signed.raw.as_bytes()).unwrap();
    assert!(matches!(layout, EitherRefLayout::ToRef), "a long comment travels by reference");
    assert_eq!(&bytes[4..], comment.as_bytes());
}

#[tokio::test]
async fn a_jetton_transfer_without_a_comment_warns_and_only_ton_tokens_are_sent() {
    let node = Node::new();
    deploy_usdt(&node, 1_000).unwrap();
    let adapter = node.adapter().await.unwrap();
    let tx = adapter.build_tx(&jetton_request(TokenId::UsdtJetton, 10)).await.unwrap();
    assert!(tx.preview.warnings.iter().any(|warning| warning.message.contains("No comment")));
    assert_refused(
        &adapter.build_tx(&jetton_request(TokenId::UsdtTrc20, 10)).await.unwrap_err(),
        "Choose an asset on TON.",
    );
}
