use std::collections::BTreeMap;
use std::sync::Arc;

use jelly_core::{
    BuildError, BuildRequest, ChainId, KeyKind, KeyMaterial, Transport, test_util::build_request,
};
use jelly_monero::{ENTRY, build, probe};
use jelly_net::{
    Endpoint, NetError, Network, ProbeFailure, ProbeReport, test_util::direct_network,
};
use zeroize::Zeroizing;

use crate::common::hd_account;

const CHAIN: ChainId = ChainId::Monero;

fn network() -> Result<Arc<Network>, NetError> {
    direct_network(&[ENTRY])
}

fn request(account_key: Option<KeyMaterial>) -> BuildRequest {
    BuildRequest {
        restore_heights: BTreeMap::from([(CHAIN, 3_000_000)]),
        account_keys: account_key
            .map(|key| BTreeMap::from([(CHAIN, Arc::new(key))]))
            .unwrap_or_default(),
        ..build_request(&[CHAIN])
    }
}

#[tokio::test]
async fn the_adapter_builds_from_the_account_key_and_the_curated_endpoints() {
    let adapter = build(&network().unwrap(), &request(Some(hd_account(0).unwrap()))).await.unwrap();
    assert_eq!(adapter.chain(), CHAIN);
}

#[tokio::test]
async fn a_wallet_without_the_account_key_has_no_adapter() {
    let refused = build(&network().unwrap(), &request(None)).await;
    assert!(matches!(refused, Err(BuildError::NoKeys { chain: ChainId::Monero })));
}

#[tokio::test]
async fn an_account_key_of_another_kind_fails_the_build() {
    let other = KeyMaterial::new(KeyKind::Secp256k1, Zeroizing::new(vec![7; 32]));
    let refused = build(&network().unwrap(), &request(Some(other))).await;
    assert!(matches!(refused, Err(BuildError::Failed(_))));
}

#[tokio::test]
async fn an_endpoint_on_a_transport_the_chain_does_not_use_is_not_probed() {
    let endpoint = Endpoint::parse(CHAIN, Transport::HttpJsonrpc, "https://node.invalid").unwrap();
    assert_eq!(
        probe(&network().unwrap(), &endpoint).await,
        ProbeReport::Unreachable { failure: ProbeFailure::NotProbed }
    );
}
