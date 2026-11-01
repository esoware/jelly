use jelly_core::{
    Amount, ChainId, ChainSpec, CustomFee, FeeLevel, KeyKind, ScriptKind, Transport, TxWarning,
    TxWarningKind,
};

const UTXO: ChainSpec = ChainSpec {
    chain: ChainId::Bitcoin,
    slip44: 0,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/84'/0'/0'/0/{index}",
    script_kinds: &[
        ScriptKind::P2tr,
        ScriptKind::P2wpkh,
        ScriptKind::P2shP2wpkh,
        ScriptKind::P2pkh,
    ],
    default_script: Some(ScriptKind::P2wpkh),
    settled_confirmations: 6,
    poll_secs: 60,
    fee_rate_unit: Some("sat/vB"),
    fee_levels: &FeeLevel::ALL,
    custom_fee: Some(CustomFee::Rate),
    high_fee: Amount::from_base_units(1_000_000),
    can_pin_nonce: false,
    can_carry_data: false,
    uri_scheme: "bitcoin",
    explorer_tx_url: None,
    explorer_address_url: None,
    transports: &[Transport::ElectrumSsl],
    primary_transport: Transport::ElectrumSsl,
    can_bump_fee: true,
    can_cancel: true,
    can_delete_account: false,
    has_destination_tags: false,
    has_comments: false,
    has_subaddresses: false,
    needs_account_key: false,
};

#[test]
fn a_bip44_purpose_picks_the_script_that_derives_under_it() {
    for (purpose, script) in [
        (86, ScriptKind::P2tr),
        (84, ScriptKind::P2wpkh),
        (49, ScriptKind::P2shP2wpkh),
        (44, ScriptKind::P2pkh),
    ] {
        assert_eq!(UTXO.resolve_script(Some(purpose), None).unwrap(), Some(script));
    }
}

#[test]
fn a_purpose_that_names_no_script_falls_back_to_the_default() {
    assert_eq!(UTXO.resolve_script(Some(0), None).unwrap(), Some(ScriptKind::P2wpkh));
    assert_eq!(UTXO.resolve_script(None, None).unwrap(), Some(ScriptKind::P2wpkh));
}

#[test]
fn a_websocket_scheme_picks_the_websocket_transport_and_nothing_else_does() {
    assert_eq!(UTXO.transport_for(" WSS://node.example.org "), Transport::WsJsonrpc);
    assert_eq!(UTXO.transport_for("ws://127.0.0.1:8546"), Transport::WsJsonrpc);
    assert_eq!(UTXO.transport_for("https://node.example.org"), Transport::ElectrumSsl);
    assert_eq!(UTXO.transport_for("node.example.org:50002"), Transport::ElectrumSsl);
}

#[test]
fn a_fee_above_the_chains_limit_is_flagged_with_both_amounts() {
    assert_eq!(UTXO.high_fee_warning(Amount::from_base_units(1_000_000)), None);
    let expected =
        "The 0.01000001 BTC fee is more than 0.01 BTC. Make sure you mean to pay this much.";
    assert_eq!(
        UTXO.high_fee_warning(Amount::from_base_units(1_000_001)),
        Some(TxWarning::new(TxWarningKind::HighFee, expected))
    );
}
