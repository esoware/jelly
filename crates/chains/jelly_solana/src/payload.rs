//! The unsigned transaction payload.
//!
//! Wire format, little-endian throughout:
//!
//! ```text
//! 0..4    magic, the ascii bytes JSOL
//! 4..12   last valid block height, u64
//! 12..20  amount in the asset's base units, u64
//! 20      length of the asset key, u8
//! ..      asset key, ascii, as jelly_core spells it
//! ..      length of the recipient, u8
//! ..      recipient address, base58 ascii
//! ..      the serialized message, to the end
//! ```
//!
//! The trailing bytes are exactly the ed25519 signing preimage. They're carried rather
//! than rebuilt, because a message recompiled from an intent could differ by a byte,
//! and the signature would cover something the user never saw.
//!
//! The fields in front are what a transaction can't state about itself. Past the
//! height, the transaction can no longer land, so a rebuild is provably safe. Signing
//! and follow-up need the asset, amount, and recipient for the history record without
//! a network read.

use jelly_core::{AssetId, TxPayload};
use solana_message::VersionedMessage;
use solana_pubkey::Pubkey;

use crate::error::SolanaError;
use crate::token;

const TARGET: &str = "jelly::solana::payload";

const MAGIC: [u8; 4] = *b"JSOL";

/// The magic, height and amount, which come before the two
/// length-prefixed texts.
const HEADER_BYTES: usize = 20;

#[derive(Debug, Clone)]
pub(crate) struct SolanaPayload {
    expiry_height: u64,
    amount: u64,
    asset: AssetId,
    recipient: String,
    payer: Pubkey,
    message: VersionedMessage,
    message_bytes: Vec<u8>,
}

impl SolanaPayload {
    /// `payer` is the compiled message's first account key. The amount is in the
    /// asset's base units, which Solana counts in 64 bits.
    pub(crate) fn new(
        message: VersionedMessage,
        payer: Pubkey,
        expiry_height: u64,
        asset: AssetId,
        amount: u64,
        recipient: String,
    ) -> Self {
        let message_bytes = message.serialize();
        Self { expiry_height, amount, asset, recipient, payer, message, message_bytes }
    }

    pub(crate) fn encode(&self) -> TxPayload {
        let asset = self.asset.key();
        let width = HEADER_BYTES
            .saturating_add(1)
            .saturating_add(asset.len())
            .saturating_add(1)
            .saturating_add(self.recipient.len())
            .saturating_add(self.message_bytes.len());
        let mut encoded = Vec::with_capacity(width);
        encoded.extend_from_slice(&MAGIC);
        encoded.extend_from_slice(&self.expiry_height.to_le_bytes());
        encoded.extend_from_slice(&self.amount.to_le_bytes());
        push_text(&mut encoded, &asset);
        push_text(&mut encoded, &self.recipient);
        encoded.extend_from_slice(&self.message_bytes);
        TxPayload::from_bytes(encoded)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, SolanaError> {
        let mut reader = Reader::new(bytes);
        if reader.array()? != MAGIC {
            return Err(SolanaError::payload("the framing is not this adapter's"));
        }
        let expiry_height = u64::from_le_bytes(reader.array()?);
        let amount = u64::from_le_bytes(reader.array()?);
        let asset_key = reader.text("asset")?;
        let recipient = reader.text("recipient")?;
        let message_bytes = reader.rest().to_vec();

        let asset = AssetId::parse_key(&asset_key)
            .ok_or_else(|| SolanaError::payload("the asset is not one this build knows"))?;
        // A payload naming another chain's asset wasn't written here and mustn't be
        // signed here.
        token::asset_token(asset)?;

        // Trailing bytes are a corrupted payload, never something to ignore.
        let message =
            wincode::deserialize_exact::<VersionedMessage>(&message_bytes).map_err(|source| {
                tracing::debug!(target: TARGET, %source, "the payload's message does not decode");
                SolanaError::payload("the message does not decode")
            })?;
        let Some(&payer) = message.static_account_keys().first() else {
            return Err(SolanaError::payload("the message has no account keys"));
        };
        Ok(Self { expiry_height, amount, asset, recipient, payer, message, message_bytes })
    }

    /// The last block height at which this transaction can still land.
    pub(crate) const fn expiry_height(&self) -> u64 {
        self.expiry_height
    }

    /// Base units of the asset.
    pub(crate) const fn amount(&self) -> u64 {
        self.amount
    }

    pub(crate) const fn asset(&self) -> AssetId {
        self.asset
    }

    pub(crate) fn recipient(&self) -> &str {
        &self.recipient
    }

    pub(crate) const fn message(&self) -> &VersionedMessage {
        &self.message
    }

    /// The bytes a signature covers.
    pub(crate) fn message_bytes(&self) -> &[u8] {
        &self.message_bytes
    }

    /// The account that pays the fee, which is the first required signer.
    pub(crate) const fn payer(&self) -> Pubkey {
        self.payer
    }

    pub(crate) fn signatures_required(&self) -> u8 {
        self.message.header().num_required_signatures
    }
}

fn push_text(bytes: &mut Vec<u8>, text: &str) {
    // Asset keys and base58 addresses are at most 44 bytes, so the length always
    // fits in one byte.
    bytes.push(u8::try_from(text.len()).unwrap_or(u8::MAX));
    bytes.extend_from_slice(text.as_bytes());
}

/// Every read is a checked split, so no length is taken on trust.
struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], SolanaError> {
        let (taken, rest) = self.rest.split_at_checked(length).ok_or_else(too_short)?;
        self.rest = rest;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], SolanaError> {
        let (taken, rest) = self.rest.split_first_chunk().ok_or_else(too_short)?;
        self.rest = rest;
        Ok(*taken)
    }

    fn text(&mut self, what: &'static str) -> Result<String, SolanaError> {
        let [length] = self.array()?;
        let bytes = self.take(usize::from(length))?;
        str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| SolanaError::payload(format!("the {what} is not text")))
    }

    const fn rest(self) -> &'a [u8] {
        self.rest
    }
}

fn too_short() -> SolanaError {
    SolanaError::payload("the payload ends before its framing does")
}

#[cfg(test)]
mod tests {
    use jelly_core::{ChainId, TokenId};
    use solana_hash::Hash;
    use solana_message::v0;
    use solana_system_interface::instruction as system_instruction;

    use super::*;
    use crate::fixtures::{OWNER, PEER};

    fn message() -> VersionedMessage {
        let instruction = system_instruction::transfer(&OWNER, &PEER, 1_000);
        let compiled = v0::Message::try_compile(&OWNER, &[instruction], &[], Hash::default())
            .expect("it compiles");
        VersionedMessage::V0(compiled)
    }

    fn payload() -> SolanaPayload {
        SolanaPayload::new(
            message(),
            OWNER,
            330,
            AssetId::Native(ChainId::Solana),
            1_000,
            PEER.to_string(),
        )
    }

    #[test]
    fn a_payload_round_trips_every_fact_it_carries() {
        let original = payload();
        let encoded = original.encode();
        let decoded = SolanaPayload::decode(encoded.as_bytes()).expect("it decodes");

        assert_eq!(decoded.expiry_height(), 330);
        assert_eq!(decoded.amount(), 1_000);
        assert_eq!(decoded.asset(), AssetId::Native(ChainId::Solana));
        assert_eq!(decoded.recipient(), PEER.to_string());
        assert_eq!(decoded.payer(), OWNER);
        assert_eq!(decoded.signatures_required(), 1);
        assert_eq!(decoded.message_bytes(), original.message_bytes());
        assert!(matches!(*decoded.message(), VersionedMessage::V0(_)));
        // A v0 message carries the version prefix in its first byte.
        assert_eq!(decoded.message_bytes().first().copied(), Some(0x80));
    }

    #[test]
    fn a_token_payload_round_trips_its_asset() {
        let original = SolanaPayload::new(
            message(),
            OWNER,
            1,
            AssetId::Token(TokenId::UsdcSpl),
            50_000_000,
            PEER.to_string(),
        );
        let decoded = SolanaPayload::decode(original.encode().as_bytes()).expect("it decodes");
        assert_eq!(decoded.asset(), AssetId::Token(TokenId::UsdcSpl));
        assert_eq!(decoded.amount(), 50_000_000);
    }

    #[test]
    fn a_trailing_byte_is_a_corrupt_payload_rather_than_something_to_ignore() {
        let mut bytes = payload().encode().as_bytes().to_vec();
        bytes.push(0xAA);
        assert!(matches!(SolanaPayload::decode(&bytes), Err(SolanaError::Payload { .. })));
    }

    #[test]
    fn a_foreign_or_truncated_framing_is_refused() {
        let good = payload().encode().as_bytes().to_vec();

        let mut wrong_magic = good.clone();
        if let Some(byte) = wrong_magic.first_mut() {
            *byte = b'X';
        }
        assert!(matches!(SolanaPayload::decode(&wrong_magic), Err(SolanaError::Payload { .. })));

        for truncated in [0usize, 4, 12, 20, 21] {
            let short = good.get(..truncated).unwrap_or_default();
            assert!(
                matches!(SolanaPayload::decode(short), Err(SolanaError::Payload { .. })),
                "{truncated}"
            );
        }
    }

    #[test]
    fn a_payload_naming_another_chains_asset_is_refused() {
        let foreign = SolanaPayload::new(
            message(),
            OWNER,
            1,
            AssetId::Token(TokenId::UsdcErc20),
            1,
            PEER.to_string(),
        );
        assert!(matches!(
            SolanaPayload::decode(foreign.encode().as_bytes()),
            Err(SolanaError::ForeignAsset { .. })
        ));
    }

    #[test]
    fn a_payload_with_an_empty_message_or_no_payer_is_refused() {
        let mut bytes = payload().encode().as_bytes().to_vec();
        bytes.truncate(
            HEADER_BYTES
                .saturating_add(1)
                .saturating_add("native:solana".len())
                .saturating_add(1)
                .saturating_add(PEER.to_string().len()),
        );
        assert!(
            matches!(SolanaPayload::decode(&bytes), Err(SolanaError::Payload { .. })),
            "an empty message is not a message"
        );

        let keyless = VersionedMessage::V0(v0::Message::default());
        bytes.extend_from_slice(&keyless.serialize());
        let error = SolanaPayload::decode(&bytes).expect_err("a message with no payer");
        assert!(
            matches!(error, SolanaError::Payload { ref detail } if detail.contains("account keys")),
            "{error:?}"
        );
    }
}
