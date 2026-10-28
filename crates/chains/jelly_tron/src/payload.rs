//! The `raw_data` protobuf passed from build to signing, kept verbatim because
//! re-encoding it can change the txid and break the signature.
//!
//! Preview and tracking decode the owner, recipient, amount, asset, expiration,
//! fee limit, and block reference from it.

use jelly_core::{Amount, AssetId, TxPayload};

use crate::abi;
use crate::address;
use crate::config::TRX;
use crate::error::TronError;
use crate::raw::{self, DecodedBody, Header};
use crate::token::token_at;

const MAGIC: [u8; 7] = *b"tronraw";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TronPayload {
    raw_data: Vec<u8>,
}

impl TronPayload {
    pub(crate) const fn new(raw_data: Vec<u8>) -> Self {
        Self { raw_data }
    }

    /// The bytes that are hashed, signed, and broadcast.
    pub(crate) fn raw_data(&self) -> &[u8] {
        &self.raw_data
    }

    /// `txID = sha256(raw_data)`. It's known before signing, which is what lets us
    /// look the transaction up after an ambiguous send.
    pub(crate) fn txid(&self) -> [u8; 32] {
        raw::txid(&self.raw_data)
    }

    pub(crate) fn txid_hex(&self) -> String {
        hex::encode(self.txid())
    }

    pub(crate) fn encode(&self) -> TxPayload {
        let mut encoded = Vec::with_capacity(MAGIC.len().saturating_add(self.raw_data.len()));
        encoded.extend_from_slice(&MAGIC);
        encoded.extend_from_slice(&self.raw_data);
        TxPayload::from_bytes(encoded)
    }

    pub(crate) fn decode(payload: &TxPayload) -> Result<Self, TronError> {
        let bytes = payload.as_bytes();
        let (magic, raw_data) = bytes
            .split_at_checked(MAGIC.len())
            .ok_or(TronError::Payload { detail: "fewer bytes than its own header" })?;
        if magic != MAGIC {
            return Err(TronError::Payload { detail: "a header this build does not write" });
        }
        if raw_data.is_empty() {
            return Err(TronError::Payload { detail: "no transaction at all" });
        }
        Ok(Self { raw_data: raw_data.to_vec() })
    }

    pub(crate) fn facts(&self) -> Result<Facts, TronError> {
        Facts::read(&self.raw_data)
    }
}

/// A built transaction read back from its own bytes.
///
/// The asset is derived, not stored: a `TransferContract` is TRX, and a call is
/// the token whose contract it names. A call to any other contract didn't come
/// from us and is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Facts {
    pub(crate) asset: AssetId,
    pub(crate) owner: String,
    pub(crate) recipient: String,
    pub(crate) amount: Amount,
    pub(crate) calldata: Vec<u8>,
    pub(crate) header: Header,
}

impl Facts {
    fn read(raw_data: &[u8]) -> Result<Self, TronError> {
        let decoded = raw::decode(raw_data)?;
        let owner = address::display(&decoded.owner);
        match decoded.body {
            DecodedBody::Transfer { to, amount_sun } => Ok(Self {
                asset: TRX,
                owner,
                recipient: address::display(&to),
                amount: Amount::from(amount_sun),
                calldata: Vec::new(),
                header: decoded.header,
            }),
            DecodedBody::Trigger { contract, calldata } => {
                let token = token_at(&address::display(&contract)).ok_or(TronError::Foreign {
                    detail: "a call to a contract this build does not carry",
                })?;
                let (recipient, amount) = abi::decode_transfer(&calldata)?;
                Ok(Self {
                    asset: AssetId::Token(token),
                    owner,
                    recipient: address::display(&recipient),
                    amount: Amount::from_base_units(amount),
                    calldata,
                    header: decoded.header,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::{ChainId, TokenId};

    use super::*;
    use crate::fixtures::{body, header};
    use crate::test_util::{OWNER, RECIPIENT};

    fn native() -> TronPayload {
        TronPayload::new(
            raw::transfer(&header(), &body(OWNER), &body(RECIPIENT), 1_000_000)
                .expect("a transfer builds"),
        )
    }

    #[test]
    fn a_payload_round_trips_and_keeps_its_id() {
        let payload = native();
        let encoded = payload.encode();
        let back = TronPayload::decode(&encoded).expect("it decodes");
        assert_eq!(back, payload);
    }

    #[test]
    fn a_payload_this_build_did_not_write_is_refused_by_name() {
        let raw = native().raw_data().to_vec();
        let unframed = TxPayload::from_bytes(raw);
        assert!(matches!(TronPayload::decode(&unframed), Err(TronError::Payload { .. })));

        assert!(matches!(
            TronPayload::decode(&TxPayload::from_bytes(Vec::new())),
            Err(TronError::Payload { .. })
        ));

        let framed = MAGIC.to_vec();
        assert!(
            matches!(
                TronPayload::decode(&TxPayload::from_bytes(framed)),
                Err(TronError::Payload { .. })
            ),
            "a framed payload with no transaction is not a transaction"
        );
    }

    #[test]
    fn a_native_transfer_states_its_own_asset_and_amount() {
        let facts = native().facts().expect("it reads back");
        assert_eq!(facts.asset, AssetId::Native(ChainId::Tron));
        assert_eq!(facts.recipient, RECIPIENT);
        assert_eq!(facts.amount, Amount::from_base_units(1_000_000));
        assert_eq!(facts.header, header());
    }

    #[test]
    fn a_token_transfer_names_the_token_by_its_contract_address() {
        let usdt = TokenId::UsdtTrc20;
        let calldata = abi::encode_transfer(&body(RECIPIENT), 5_000_000);
        let stamped = header().with_fee_limit(15_000_000, u64::MAX).expect("in range");
        let payload = TronPayload::new(raw::trigger(
            &stamped,
            &body(OWNER),
            &body(usdt.contract()),
            calldata,
        ));
        let facts = payload.facts().expect("it reads back");
        assert_eq!(facts.asset, AssetId::Token(usdt));
        assert_eq!(facts.recipient, RECIPIENT);
        assert_eq!(facts.amount, Amount::from_base_units(5_000_000));
        assert_eq!(facts.header.fee_limit_sun, 15_000_000);
    }

    #[test]
    fn a_call_to_a_contract_this_build_does_not_carry_is_refused() {
        let calldata = abi::encode_transfer(&body(RECIPIENT), 1);
        let payload =
            TronPayload::new(raw::trigger(&header(), &body(OWNER), &body(RECIPIENT), calldata));
        let error = payload.facts().expect_err("an unknown contract");
        assert!(matches!(error, TronError::Foreign { .. }), "{error:?}");
    }
}
