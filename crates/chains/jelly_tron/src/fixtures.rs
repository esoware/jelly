use std::time::Duration;

use jelly_core::Amount;
use serde_json::Value;

use crate::address;
use crate::config::TRX;
use crate::node::Answered;
use crate::params::ChainParams;
use crate::pending::Tracked;
use crate::raw::Header;
use crate::route::Route;
use crate::shape::{BlockRef, Resources};
use crate::test_util::{account_resource, endpoint};

const CONTRACTS: &str = include_str!("../tests/vectors/contracts.json");

pub(crate) fn body(value: &str) -> [u8; address::BODY_BYTES] {
    address::body("test", value).expect("a test address")
}

/// A ten-minute window pinned to a made-up head block.
pub(crate) fn header() -> Header {
    let tip = BlockRef { number: 85_304_913, block_id: [3u8; 32], timestamp_millis: 1_700_000 };
    Header::from_block(&tip, Duration::from_secs(600)).expect("a header")
}

/// An account with the given pools, as `getaccountresource` reports it.
pub(crate) fn resources(free_used: u64, staked_limit: u64, energy_limit: u64) -> Resources {
    serde_json::from_str(&account_resource(free_used, staked_limit, energy_limit))
        .expect("resources decode")
}

/// The compiled prices, labeled as a node's answer so quotes built on them skip
/// the compiled-figures warning.
pub(crate) fn priced_params() -> ChainParams {
    ChainParams { is_from_chain: true, ..ChainParams::FALLBACK }
}

/// An unsent TRX send of address `address_id`, whose window closes at a
/// head-block time of 1,000,000 milliseconds.
pub(crate) fn tracked(txid: &str, address_id: i64) -> Tracked {
    Tracked {
        txid: txid.to_owned(),
        address_id,
        owner: String::from("TOwner"),
        recipient: String::from("TPeer"),
        asset: TRX,
        amount: Amount::from_base_units(1_000),
        expiration_millis: 1_000_000,
        raw: String::from("0a00"),
        is_sent: false,
    }
}

pub(crate) fn answered(route: Route, body: &str) -> Answered {
    Answered::new(endpoint(), route, body.to_owned())
}

/// `tests/vectors/contracts.json`, the contract table and selectors pinned against
/// sources outside this crate.
pub(crate) fn contracts() -> Value {
    serde_json::from_str(CONTRACTS).expect("the contract vectors parse")
}
