use jelly_bitcoin_cash::{ENTRY, SPEC, build};
use jelly_core::{ChainId, test_util::build_request};
use jelly_net::test_util::direct_network;

#[tokio::test]
async fn the_shipped_endpoints_build_an_adapter_with_the_specs_fee_bump() {
    let network = direct_network(&[ENTRY]).unwrap();
    let adapter = build(&network, &build_request(&[ChainId::BitcoinCash])).await.unwrap();
    assert_eq!(adapter.chain(), SPEC.chain);
    assert_eq!(adapter.fee_bump().is_some(), SPEC.can_bump_fee);
}
