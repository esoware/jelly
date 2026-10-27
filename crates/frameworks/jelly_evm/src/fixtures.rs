//! Fixtures the unit tests share.

use alloy::primitives::{Address, B256, Bytes, keccak256};
use jelly_core::{Amount, AssetId, ChainId};

use crate::fee::FeePair;
use crate::ledger::{Envelope, Ledger, State, Tracked};
use crate::params::EvmParams;
use crate::payload::Intent;
use crate::test_util::{ARBITRUM, AVALANCHE, BASE, BSC, ETHEREUM, OPTIMISM, POLYGON};
use crate::token::TokenTable;
use crate::watched::Watched;

pub(crate) const NETWORKS: [EvmParams; 7] =
    [ETHEREUM, ARBITRUM, BASE, OPTIMISM, BSC, POLYGON, AVALANCHE];

pub(crate) fn address(byte: u8) -> Address {
    Address::repeat_byte(byte)
}

pub(crate) fn watched(address_id: i64, byte: u8) -> Watched {
    Watched { address_id, owner: address(byte) }
}

pub(crate) fn token_table() -> TokenTable {
    TokenTable::for_params(&ETHEREUM).expect("the registry parses")
}

const NATIVE: AssetId = AssetId::Native(ChainId::Ethereum);

pub(crate) fn pair(cap: u128) -> FeePair {
    FeePair::new(cap, cap.saturating_div(10)).expect("a pair")
}

pub(crate) fn payment(from: Address, to: Address) -> Intent {
    Intent { from, asset: NATIVE, to, amount: Amount::from_base_units(1) }
}

pub(crate) fn held(from: Address, nonce: u64, cap: u128, now_millis: u64) -> Tracked {
    Tracked::held(payment(from, address(9)), nonce, pair(cap), now_millis)
}

/// The bytes a send signed from `seed` goes out as, which hash to its txid.
pub(crate) fn envelope(seed: u8) -> Envelope {
    let raw = Bytes::from(vec![0x02, seed]);
    Envelope { txid: keccak256(&raw), raw }
}

pub(crate) fn txid_of(seed: u8) -> B256 {
    envelope(seed).txid
}

pub(crate) fn signed(from: Address, nonce: u64, cap: u128, seed: u8) -> Tracked {
    Tracked {
        state: State::Signed { envelope: envelope(seed), is_sent: false },
        ..held(from, nonce, cap, 0)
    }
}

/// A ledger holding one send signed from `seed` at nonce 5, already broadcast.
pub(crate) fn sent_ledger(seed: u8) -> Ledger {
    let mut ledger = Ledger::new();
    ledger.record_signed(signed(address(1), 5, 1_000, seed));
    ledger.mark_sent(txid_of(seed));
    ledger
}
