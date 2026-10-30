use crate::amount::Amount;
use crate::chain::{ChainId, ScriptKind};
use crate::error::KeyError;
use crate::secret::KeyKind;
use crate::transport::Transport;
use crate::tx::{CustomFee, FeeLevel, TxWarning, TxWarningKind};

/// Templates use `{index}`, `{txid}`, and `{address}` placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent capabilities, not states of one thing"
)]
pub struct ChainSpec {
    pub chain: ChainId,
    /// SLIP-44 coin type of the chain's default derivation path.
    pub slip44: u32,
    pub key_kind: KeyKind,
    /// The path family new addresses derive under, with `{index}` where the address
    /// index goes, such as `m/84'/0'/0'/0/{index}`.
    pub default_path: &'static str,
    /// The script flavors the chain supports, if it has any.
    pub script_kinds: &'static [ScriptKind],
    pub default_script: Option<ScriptKind>,
    /// The depth at which a reversal stops being a practical concern, short of the deeper
    /// finality an exchange waits for. The interface shows progress up to it, so a chain
    /// that settles in its first block shows none.
    pub settled_confirmations: u32,
    /// The sync loop's poll interval. On a chain that listens for pushes it's a safety net
    /// and can stay wide; on one that doesn't, it's the update latency.
    pub poll_secs: u32,
    /// The unit a fee rate is quoted in, such as `sat/vB`, or `None` when the chain
    /// doesn't price fees by size.
    pub fee_rate_unit: Option<&'static str>,
    /// The fee levels a send can pick from, slowest first. Empty when the protocol fixes
    /// the fee.
    pub fee_levels: &'static [FeeLevel],
    /// What a custom fee names, or `None` when the chain takes only its levels.
    pub custom_fee: Option<CustomFee>,
    /// The fee in the chain's coin above which a preview warns that it's more than an
    /// everyday user would pay: about $2 when it was set, or more on a chain where an
    /// ordinary send costs that much. It's a fixed amount because a fiat price can be
    /// switched off or stale.
    pub high_fee: Amount,
    /// Whether a send can set its own nonce instead of the account's next one.
    pub can_pin_nonce: bool,
    /// Whether a send can carry calldata in place of the generated transfer.
    pub can_carry_data: bool,
    /// The payment URI scheme, such as `bitcoin` in `bitcoin:bc1...`.
    pub uri_scheme: &'static str,
    pub explorer_tx_url: Option<&'static str>,
    pub explorer_address_url: Option<&'static str>,
    /// Every transport the chain builds a pool for, with [`ChainSpec::primary_transport`]
    /// first. The endpoint registry refuses an address on any other.
    pub transports: &'static [Transport],
    /// What a user-typed endpoint defaults to, since the endpoint editor asks only for a
    /// URL.
    pub primary_transport: Transport,
    pub can_bump_fee: bool,
    /// Whether a pending send can be canceled by a replacement that moves nothing.
    pub can_cancel: bool,
    /// Whether a send can delete its sending account from the ledger, paying out
    /// everything the account holds and recovering its reserve.
    pub can_delete_account: bool,
    /// Whether a payment can carry a destination tag, the number a shared receiving
    /// account files it under.
    pub has_destination_tags: bool,
    pub has_comments: bool,
    pub has_subaddresses: bool,
    /// Whether the adapter needs the wallet's account key to scan, as on a chain where
    /// only a view key reveals incoming funds.
    pub needs_account_key: bool,
}

impl ChainSpec {
    /// A chain whose scanner watches only the wallet's own account would never see the
    /// funds of a standalone key.
    pub const fn can_hold_standalone_keys(&self) -> bool {
        !self.needs_account_key
    }

    /// The script flavor an address takes: `requested`, else the flavor `purpose` names
    /// when the chain's flavors derive under BIP-44 purposes, else the chain default.
    ///
    /// A chain with no script flavors answers `None` and refuses a request for one. A
    /// flavor the chain doesn't support is an error, never a silent downgrade, so `m/86'`
    /// on Litecoin fails instead of producing a P2WPKH address the user's other wallet
    /// would never show.
    pub fn resolve_script(
        &self,
        purpose: Option<u32>,
        requested: Option<ScriptKind>,
    ) -> Result<Option<ScriptKind>, KeyError> {
        if self.script_kinds.is_empty() {
            return match requested {
                None => Ok(None),
                Some(_) => Err(KeyError::ScriptNotApplicable { chain: self.chain }),
            };
        }
        let is_keyed_by_purpose = self.script_kinds.iter().any(|kind| kind.purpose().is_some());
        let named_by_purpose = purpose.filter(|_| is_keyed_by_purpose).and_then(|purpose| {
            ScriptKind::ALL.into_iter().find(|kind| kind.purpose() == Some(purpose))
        });
        let script = requested
            .or(named_by_purpose)
            .or(self.default_script)
            .ok_or(KeyError::Internal { context: "chain spec has no default script" })?;
        if !self.script_kinds.contains(&script) {
            return Err(KeyError::ScriptUnsupported { chain: self.chain, script });
        }
        Ok(Some(script))
    }

    /// The transport a user-typed endpoint means on this chain: the websocket transport for
    /// a `ws://` or `wss://` URL, and the primary transport for anything else.
    ///
    /// That way `wss://` typed into the endpoint editor becomes a subscription endpoint
    /// instead of being folded into HTTPS.
    pub fn transport_for(&self, input: &str) -> Transport {
        let is_websocket = input.trim().split_once("://").is_some_and(|(scheme, _)| {
            let scheme = scheme.trim();
            scheme.eq_ignore_ascii_case("wss") || scheme.eq_ignore_ascii_case("ws")
        });
        if is_websocket { Transport::WsJsonrpc } else { self.primary_transport }
    }

    /// The warning a preview needs when its `fee` is above [`ChainSpec::high_fee`], even if
    /// the adapter already warns about the fee: the adapter judges the fee against its
    /// market, and this judges its size.
    pub fn high_fee_warning(&self, fee: Amount) -> Option<TxWarning> {
        (fee > self.high_fee).then(|| {
            TxWarning::new(
                TxWarningKind::HighFee,
                format!(
                    "The {} fee is more than {}. Make sure you mean to pay this much.",
                    self.chain.format_amount(fee),
                    self.chain.format_amount(self.high_fee)
                ),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
