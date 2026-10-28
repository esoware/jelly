use jelly_core::{Address, ChainAdapter as _, ChainId, KeyError, ScriptKind, TokenId};
use jelly_ton::{ENTRY, Method};
use tonlib_core::types::TonAddress;

use super::common::{Node, jetton_wallet_of, key};

/// The default key's V4R2 and V3R2 wallets, the versions a probe asks about beside the
/// V5R1 wallet it was found under.
fn other_versions() -> Result<Vec<Address>, KeyError> {
    let key = key()?;
    [ScriptKind::TonV4r2, ScriptKind::TonV3r2]
        .into_iter()
        .map(|script| {
            Ok(Address::assume_canonical(ChainId::Ton, ENTRY.address(&key, Some(script))?))
        })
        .collect()
}

#[tokio::test]
async fn the_probe_stops_asking_about_an_address_at_its_first_sign_of_use() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let candidates = other_versions().unwrap();
    node.state.lock().used = vec![TonAddress::from_base64_url(candidates[0].value()).unwrap()];
    let before = node.state.lock().calls.len();
    let used = adapter.script_probe().unwrap().used(&candidates).await.unwrap();
    assert_eq!(used, [candidates[0].clone()]);
    let calls = node.state.lock().calls[before..].to_vec();
    let methods: Vec<Method> = calls.iter().map(|(method, _)| *method).collect();
    assert_eq!(
        methods,
        [
            Method::GetAddressInformation,
            Method::GetAddressInformation,
            Method::RunGet,
            Method::GetAddressInformation,
            Method::RunGet,
            Method::GetAddressInformation,
            Method::RunGet,
            Method::GetAddressInformation,
        ],
        "a candidate in use is asked once, and an unused one about each jetton wallet"
    );
    assert!(
        calls.iter().all(|(_, params)| params.get("seqno").is_none()),
        "the probe asks at the node's latest block, so it spends no call on the tip"
    );
}

#[tokio::test]
async fn a_wallet_that_only_ever_received_jettons_is_found_in_use() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let candidates = other_versions().unwrap();
    let owner = TonAddress::from_base64_url(candidates[1].value()).unwrap();
    node.state.lock().used = vec![jetton_wallet_of(TokenId::XautJetton, &owner)];
    let used = adapter.script_probe().unwrap().used(&candidates).await.unwrap();
    assert_eq!(used, [candidates[1].clone()], "the deployed XAUT wallet shows the V3R2 key used");
}
