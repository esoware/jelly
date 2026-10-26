use bitcoin::hashes::Hash as _;
use bitcoin::{BlockHash, CompressedPublicKey, Script, ScriptBuf, Sequence, Txid, Weight};
use jelly_core::{
    AddressCodec, Amount, ChainId, FeeLevel, FeeRate, KeyError, ScanStream, ScriptKind, TxId,
    TxWarningKind,
};
use jelly_electrum::{Checkpoint, ElectrumChain};

use crate::address::key_script;
use crate::error::UtxoError;
use crate::script::{SpendableScript, UtxoScript};
use crate::token;

/// Everything the UTXO chains differ by. Each chain crate holds its table.
///
/// The chains share every consensus rule this engine touches that a table does
/// not name, so nothing else in the crate matches on [`UtxoParams::chain`].
#[derive(Debug, Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate consensus fact of the chain, read on its own"
)]
pub struct UtxoParams {
    pub chain: ChainId,
    /// Address to script and back, the chain's one validating codec.
    pub codec: &'static dyn AddressCodec,
    /// The script kinds this build derives keys for and spends on the chain, as its
    /// spec declares them.
    pub script_kinds: &'static [ScriptKind],
    /// The unit a fee rate is quoted in, as the chain's spec declares it.
    pub fee_rate_unit: &'static str,
    /// Block zero's hash in the display byte order Electrum, `server.features`,
    /// and every block explorer use.
    pub genesis: [u8; 32],
    /// A block past the last split from a chain that shares block zero, which is
    /// what tells the two chains' Electrum servers apart, since the wire protocol is
    /// identical across coins and the server populations share ports.
    pub checkpoint: Option<KnownBlock>,
    /// Whether a header carries its parent chain's proof of work after its eighty
    /// bytes.
    pub is_merge_mined: bool,
    /// Whether the chain has segregated witness. Without it a transaction's weight
    /// is four times its size, so a virtual byte is a byte and the network limits a
    /// transaction by its size.
    pub has_segwit: bool,
    /// The stream every sync cursor of this chain is stored under. The cursor
    /// encoding is the same on every chain.
    pub scan_stream: ScanStream,
    pub dust: DustRule,
    pub min_relay_fee: FeeRate,
    pub incremental_relay_fee: FeeRate,
    /// The most one transaction's outputs may add up to, which is Core's
    /// `MAX_MONEY`.
    pub max_money: Amount,
    pub max_standard_weight: Weight,
    /// Core's `-mintxfee`, the lowest rate the reference wallet pays even when the
    /// network relays less. No fee level goes below it.
    pub min_tx_fee: FeeRate,
    pub fee_market: FeeMarket,
    pub replacement: Replacement,
    /// How a pending send is sped up, or `None` when it cannot be.
    pub bump_method: Option<BumpMethod>,
    /// Bitcoin Cash's replay protection, which signs every input with
    /// `SIGHASH_FORKID` over the BIP-143 digest and puts this fork id in the upper
    /// bits of the sighash type.
    pub sighash_fork_id: Option<u32>,
    /// Whether an output may carry a `CashTokens` prefix in front of its locking
    /// script.
    pub has_cash_tokens: bool,
    /// Whether a transaction's first output may hold an SLP message, which assigns
    /// tokens to the outputs after it that a plain spend of them would burn.
    pub has_slp_tokens: bool,
    /// A depth, not a duration, because a reorg is counted in blocks and a faster
    /// chain reaches the same depth sooner.
    pub confirmation_watch_depth: u32,
    /// Core's `COINBASE_MATURITY`. A node refuses a spend of a coinbase output
    /// while the spending block's height minus the coin's is below it.
    pub coinbase_maturity: u32,
    /// Litecoin Core's `PEGOUT_MATURITY`, the same rule for an output a `HogEx`
    /// pays out of MWEB.
    pub pegout_maturity: u32,
    /// Chain-specific preview warnings, matched on the recipient address's prefix.
    pub address_notes: &'static [AddressNote],
    /// Address forms this build refuses by name, on every external address.
    pub address_refusals: &'static [AddressRefusal],
    /// Absolute fee a build refuses to exceed, which turns an absurd rate into an
    /// error instead of a loss.
    pub fee_ceiling: Amount,
    /// The rate a change output is assumed to cost to spend later.
    pub long_term_fee_rate: FeeRate,
}

/// A block's height and its hash in display byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownBlock {
    pub height: u32,
    pub hash: [u8; 32],
}

/// How the chain's nodes treat a BIP-125 replacement, which decides what sequence
/// every input of a built transaction carries.
///
/// No sequence is final, so the anti-fee-sniping locktime is always enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replacement {
    /// The reference node relays a replacement by default.
    ByDefault,
    /// The reference node relays one only when configured to, but inputs still
    /// signal so a node that does relay one can unstick the payment.
    WhenConfigured,
    /// No node relays one. Inputs do not signal, since a signal buys nothing and a
    /// merchant taking a payment before it confirms can read one as a risk.
    Never,
}

/// How a pending send is sped up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BumpMethod {
    /// A BIP-125 replacement pays more for the same payment, which needs nodes that
    /// relay one by default.
    Replace,
    /// A second transaction spends the send's change and pays enough for the whole
    /// unconfirmed package to reach the chosen rate, which every node relays.
    ChildPaysForParent,
}

impl Replacement {
    pub const fn input_sequence(self) -> Sequence {
        match self {
            Self::ByDefault | Self::WhenConfigured => Sequence::ENABLE_RBF_NO_LOCKTIME,
            Self::Never => Sequence::ENABLE_LOCKTIME_NO_RBF,
        }
    }
}

/// How the smallest output a node relays is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DustRule {
    /// Bitcoin Core's rule, under which an output is dust when it is worth less
    /// than spending it would cost at the chain's `DUST_RELAY_TX_FEE`, given in base
    /// units per 1000 weight units.
    RelayFee(FeeRate),
    /// Dogecoin Core's rule, one value for every spendable output.
    Fixed(Amount),
}

/// How the fee levels are priced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeMarket {
    /// Transactions bid for scarce block space, so each level follows the market.
    Auction(Auction),
    /// Blocks have room to spare and the reference wallet pays a fixed rate, which
    /// is [`UtxoParams::min_tx_fee`]. The one level pays it too, and no server is
    /// asked, since its estimate has nothing to measure.
    Flat,
}

/// Where a market rate is read from, in the order it is tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Auction {
    /// The confirmation target each fee level is estimated for when the fee service is
    /// unavailable, in [`FeeLevel::ALL`] order.
    pub target_blocks: [u16; 3],
    /// Virtual bytes of mempool one block holds, the unit a fee horizon is
    /// measured in.
    pub block_vsize: u64,
    /// The route of the mempool.space-family fee service that prices the levels, whose
    /// `hourFee`, `halfHourFee`, and `fastestFee` are the slow, normal, and fast rates.
    pub api_route: &'static str,
    /// The route read from a deployment that answers `api_route` with 404.
    pub api_fallback_route: Option<&'static str>,
}

/// A warning a chain wants on any preview whose recipient starts with `prefix`.
#[derive(Debug, Clone, Copy)]
pub struct AddressNote {
    pub prefix: &'static str,
    pub kind: TxWarningKind,
    /// Copy the interface renders verbatim. See [`jelly_core::TxWarning`].
    pub message: &'static str,
}

/// An address form a chain refuses outright, before any checksum is read.
#[derive(Debug, Clone, Copy)]
pub struct AddressRefusal {
    /// Lowercase, and matched against the trimmed address in any letter case,
    /// because bech32 permits an all-uppercase spelling.
    pub prefix: &'static str,
    /// Copy the interface renders verbatim.
    pub message: &'static str,
}

/// The locking script an output pays, with whether a token prefix rides in front
/// of it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Locking<'a> {
    pub(crate) script: &'a Script,
    pub(crate) has_tokens: bool,
}

impl UtxoParams {
    /// Adopts the network's incremental relay fee when it is above the compiled
    /// floor, because a replacement has to clear whatever the server charges.
    #[must_use]
    pub(crate) const fn with_incremental_relay_fee(mut self, observed: Option<FeeRate>) -> Self {
        if let Some(rate) = observed
            && rate.sat_per_kwu() > self.incremental_relay_fee.sat_per_kwu()
        {
            self.incremental_relay_fee = rate;
        }
        self
    }

    /// The fee levels a send can pick from. A flat market would price every level
    /// the same, so it offers only the normal one.
    pub const fn fee_levels(&self) -> &'static [FeeLevel] {
        match self.fee_market {
            FeeMarket::Auction(_) => &FeeLevel::ALL,
            FeeMarket::Flat => &[FeeLevel::Normal],
        }
    }

    /// What every Electrum server of this chain proves before it serves.
    pub fn electrum_chain(&self) -> ElectrumChain {
        ElectrumChain {
            genesis: display_hash(self.genesis),
            checkpoint: self
                .checkpoint
                .map(|block| Checkpoint { height: block.height, hash: display_hash(block.hash) }),
            is_merge_mined: self.is_merge_mined,
        }
    }

    pub fn script_pubkey(&self, address: &str) -> Result<ScriptBuf, UtxoError> {
        let checked = self.codec.validate(address)?;
        Ok(ScriptBuf::from(self.codec.script_pubkey(checked.value())?))
    }

    /// The output script of `address`, refused unless this build can spend it on
    /// this chain, which is only in a flavour the chain derives keys for. A P2SH
    /// output reads as wrapped segwit, which a chain without segwit never derives.
    pub(crate) fn spendable_script(&self, address: &str) -> Result<SpendableScript, UtxoError> {
        let script = SpendableScript::new(self.script_pubkey(address)?)?;
        if !self.script_kinds.contains(&ScriptKind::from(script.kind())) {
            return Err(UtxoError::UnspendableScript);
        }
        Ok(script)
    }

    /// Every external address, watch rows and recipients alike, passes through
    /// here, so a refused form is named before the codec reads it.
    ///
    /// Other valid output scripts stay payable even when Jelly cannot derive them.
    pub fn canonical(&self, address: &str) -> Result<String, UtxoError> {
        if !self.address_refusals.is_empty() {
            let lowered = address.trim().to_ascii_lowercase();
            if let Some(refusal) =
                self.address_refusals.iter().find(|refusal| lowered.starts_with(refusal.prefix))
            {
                return Err(UtxoError::RefusedAddress { message: refusal.message });
            }
        }
        Ok(self.codec.validate(address)?.into_value())
    }

    /// The locking script `script_pubkey` pays. On a chain with `CashTokens`, a
    /// token prefix is stepped over, and one that does not parse leaves the bytes
    /// as they are, which then pay no address.
    pub(crate) fn locking<'a>(&self, script_pubkey: &'a Script) -> Locking<'a> {
        if self.has_cash_tokens
            && let Some(script) = token::locking_script(script_pubkey.as_bytes())
        {
            return Locking { script: Script::from_bytes(script), has_tokens: true };
        }
        Locking { script: script_pubkey, has_tokens: false }
    }

    /// The address `pubkey` takes in the `script` flavour, refused when the chain
    /// derives no key in that flavour, so a taproot request on Litecoin fails rather
    /// than falling back to segwit v0.
    pub fn key_address(&self, script: ScriptKind, pubkey: &[u8]) -> Result<String, KeyError> {
        let output = UtxoScript::of(script)
            .filter(|_| self.script_kinds.contains(&script))
            .ok_or(KeyError::ScriptUnsupported { chain: self.chain, script })?;
        let key =
            CompressedPublicKey::from_slice(pubkey).map_err(|_| KeyError::InvalidPublicKey)?;
        self.codec
            .address_of(key_script(output, &key).as_bytes())
            .ok_or(KeyError::internal("a derived script has no address"))
    }

    /// The address an output pays, read through any token prefix in front of it.
    pub fn address_of(&self, script_pubkey: &Script) -> Option<String> {
        self.codec.address_of(self.locking(script_pubkey).script.as_bytes())
    }

    /// The smallest value an output paying `script_pubkey` may carry. An
    /// unspendable output is never dust, as both Cores have it.
    pub fn dust_threshold(&self, script_pubkey: &Script) -> Amount {
        if script_pubkey.is_op_return() {
            return Amount::ZERO;
        }
        match self.dust {
            DustRule::RelayFee(rate) => {
                let dust_relay_fee = bitcoin::FeeRate::from_sat_per_kwu(rate.sat_per_kwu());
                Amount::from(script_pubkey.minimal_non_dust_custom(dust_relay_fee).to_sat())
            }
            DustRule::Fixed(limit) => limit,
        }
    }

    /// The compiled minimum relay fee, or the network's reported floor when it
    /// is higher.
    pub(crate) fn relay_floor(&self, observed: Option<FeeRate>) -> FeeRate {
        observed.map_or(self.min_relay_fee, |rate| rate.max(self.min_relay_fee))
    }

    /// A rate as the interface shows it, in this chain's own unit.
    pub fn format_rate(&self, rate: FeeRate) -> String {
        format!("{} {}", rate.format_sat_per_vb(), self.fee_rate_unit)
    }

    pub(crate) fn txid(&self, txid: Txid) -> TxId {
        TxId::new(self.chain, txid.to_string())
    }
}

/// A `BlockHash` stores the reverse of the display byte order the tables use.
fn display_hash(display: [u8; 32]) -> BlockHash {
    let mut bytes = display;
    bytes.reverse();
    BlockHash::from_byte_array(bytes)
}

#[cfg(test)]
mod tests {
    use crate::fixtures::kwu;
    use crate::test_util::{BITCOIN, DOGECOIN, LITECOIN};

    #[test]
    fn the_relay_floor_never_drops_below_the_chains_own_minimum() {
        assert_eq!(BITCOIN.relay_floor(None), BITCOIN.min_relay_fee);
        assert_eq!(BITCOIN.relay_floor(Some(kwu(1))), BITCOIN.min_relay_fee);
        assert_eq!(BITCOIN.relay_floor(Some(kwu(9_000))), kwu(9_000));
    }

    #[test]
    fn a_reported_incremental_fee_below_the_compiled_floor_never_lowers_it() {
        let lowered = BITCOIN.with_incremental_relay_fee(Some(kwu(1)));
        assert_eq!(lowered.incremental_relay_fee, BITCOIN.incremental_relay_fee);
    }

    #[test]
    fn a_reported_incremental_fee_is_the_only_figure_a_reading_folds_in() {
        for table in [BITCOIN, LITECOIN] {
            let unreported = table.with_incremental_relay_fee(None);
            assert_eq!(
                unreported.incremental_relay_fee, table.incremental_relay_fee,
                "no report leaves the compiled floor alone"
            );
            assert_eq!(unreported.dust, table.dust, "a reading never touches the dust figure");

            let busy = table.with_incremental_relay_fee(Some(kwu(1_000)));
            assert_eq!(busy.incremental_relay_fee, kwu(1_000));
            assert_eq!(busy.dust, table.dust, "a reading never touches the dust figure");
        }
    }

    /// Dogecoin Core's `IsDust` exempts an unspendable output, as Bitcoin Core's
    /// rule does.
    #[test]
    fn an_unspendable_output_is_never_dust() {
        let op_return = bitcoin::ScriptBuf::new_op_return([1u8, 2, 3]);
        assert!(DOGECOIN.dust_threshold(&op_return).is_zero());
        assert!(BITCOIN.dust_threshold(&op_return).is_zero());
    }
}
