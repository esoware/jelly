use alloy::primitives::{Address, B256, Bytes, keccak256};
use jelly_core::{Amount, AssetId, ChainId};

use crate::fee::FeePair;
use crate::ledger::{Envelope, Ledger, Tracked};
use crate::params::EvmParams;
use crate::payload::Intent;
use crate::token::TokenTable;
use crate::watched::Watched;

pub(crate) const ETHEREUM: EvmParams = shipped(&jelly_ethereum::PARAMS);
pub(crate) const ARBITRUM: EvmParams = shipped(&jelly_arbitrum::PARAMS);
pub(crate) const BASE: EvmParams = shipped(&jelly_base::PARAMS);
pub(crate) const OPTIMISM: EvmParams = shipped(&jelly_optimism::PARAMS);
pub(crate) const BSC: EvmParams = shipped(&jelly_bsc::PARAMS);
pub(crate) const POLYGON: EvmParams = shipped(&jelly_polygon::PARAMS);
pub(crate) const AVALANCHE: EvmParams = shipped(&jelly_avalanche::PARAMS);
pub(crate) const NETWORKS: [EvmParams; 7] =
    [ETHEREUM, ARBITRUM, BASE, OPTIMISM, BSC, POLYGON, AVALANCHE];

/// The chain crates link this crate's library build, so the params they ship are that
/// build's `EvmParams`, a different type from the one these tests see.
const fn shipped(params: &jelly_evm::EvmParams) -> EvmParams {
    let jelly_evm::EvmParams {
        chain,
        chain_id,
        request_timeout,
        tokens,
        token_standard,
        block_millis,
        watch_floor,
        fee_history_blocks,
        min_priority_fee_wei,
        max_priority_fee_wei,
        confirmations_target,
        log_window_blocks,
        backfill_floor,
        has_native_transfer_logs,
        has_l1_data_fee,
        has_keyless_history,
    } = *params;
    EvmParams {
        chain,
        chain_id,
        request_timeout,
        tokens,
        token_standard,
        block_millis,
        watch_floor,
        fee_history_blocks,
        min_priority_fee_wei,
        max_priority_fee_wei,
        confirmations_target,
        log_window_blocks,
        backfill_floor,
        has_native_transfer_logs,
        has_l1_data_fee,
        has_keyless_history,
    }
}

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

pub(crate) fn held(from: Address, nonce: u64, cap: u128) -> Tracked {
    Tracked::held(payment(from, address(9)), nonce, pair(cap), 0)
}

/// Stand-in signed bytes for `seed`, paired with their keccak hash as the txid.
pub(crate) fn envelope(seed: u8) -> Envelope {
    let raw = Bytes::from(vec![0x02, seed]);
    Envelope { txid: keccak256(&raw), raw }
}

pub(crate) fn txid_of(seed: u8) -> B256 {
    envelope(seed).txid
}

pub(crate) fn signed(from: Address, nonce: u64, cap: u128, seed: u8) -> Tracked {
    Tracked::signed(payment(from, address(9)), nonce, pair(cap), None, envelope(seed))
}

/// A ledger holding one send signed from `seed` at nonce 5, already broadcast.
pub(crate) fn sent_ledger(seed: u8) -> Ledger {
    let mut ledger = Ledger::new();
    ledger.record_signed(signed(address(1), 5, 1_000, seed));
    ledger.mark_sent(txid_of(seed));
    ledger
}
