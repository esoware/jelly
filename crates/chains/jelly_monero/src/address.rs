use jelly_core::{Address, AddressCodec, AddressError, ChainId};
use monero_address::{MoneroAddress, Network};

use crate::entry::ENTRY;
use crate::error::MoneroError;
use crate::params::CHAIN;

/// Encoding and decoding belong to `monero-address`. This crate only owns the keys
/// behind them.
pub(crate) const CODEC: &dyn AddressCodec = &MoneroCodec;

#[derive(Debug)]
struct MoneroCodec;

impl AddressCodec for MoneroCodec {
    fn chain(&self) -> ChainId {
        CHAIN
    }

    fn validate(&self, value: &str) -> Result<Address, AddressError> {
        Ok(Address::assume_canonical(CHAIN, parse(value)?.to_string()))
    }
}

/// Decodes and checksum-verifies a mainnet address of any kind.
fn parse(value: &str) -> Result<MoneroAddress, AddressError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AddressError::Empty { chain: CHAIN });
    }
    MoneroAddress::from_str(Network::Mainnet, trimmed).map_err(|error| match error {
        monero_address::AddressError::DifferentNetwork { .. }
        | monero_address::AddressError::InvalidTypeByte(_) => {
            AddressError::WrongNetwork { chain: CHAIN }
        }
        monero_address::AddressError::InvalidLength => AddressError::WrongLength { chain: CHAIN },
        monero_address::AddressError::InvalidEncoding => AddressError::Checksum { chain: CHAIN },
        monero_address::AddressError::InvalidKey
        | monero_address::AddressError::UnknownFeatures(_)
        | monero_address::AddressError::PaymentIDsUnsupported => {
            AddressError::Invalid { chain: CHAIN }
        }
    })
}

/// A recipient validated by the chain entry, whose refusals carry no fragment of
/// the input.
///
/// It keeps the parsed address for the builder beside the canonical text for the
/// preview, so nothing downstream re-parses or re-encodes it.
#[derive(Debug)]
pub(crate) struct Recipient {
    address: MoneroAddress,
    canonical: String,
}

impl Recipient {
    /// Every address this crate pays goes through here.
    pub(crate) fn parse(value: &str) -> Result<Self, MoneroError> {
        let validated = ENTRY.validate_address(value).map_err(MoneroError::Address)?;
        let canonical = validated.into_value();
        // The canonical form came out of the same parser, so a failure here is a bug,
        // not bad input.
        let address = parse(&canonical).map_err(|_| MoneroError::Internal {
            context: "a validated monero address did not re-parse",
        })?;
        Ok(Self { address, canonical })
    }

    pub(crate) const fn address(&self) -> MoneroAddress {
        self.address
    }

    pub(crate) fn canonical(&self) -> &str {
        &self.canonical
    }

    /// An integrated address's payment id, which travels encrypted in the transaction
    /// and earns a warning in the preview.
    pub(crate) fn payment_id(&self) -> Option<[u8; 8]> {
        self.address.payment_id()
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::fixtures::account;

    #[derive(Deserialize)]
    struct AddressVectors {
        primary: String,
        integrated: Integrated,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Integrated {
        payment_id: String,
        address: String,
    }

    #[test]
    fn an_integrated_address_is_recognized_with_its_payment_id() {
        let vectors: AddressVectors =
            serde_json::from_str(include_str!("../tests/vectors/subaddresses.json"))
                .expect("the vector file parses");
        let integrated = &vectors.integrated;
        let payment_id: [u8; 8] = hex::decode(&integrated.payment_id)
            .expect("hex")
            .try_into()
            .expect("an eight-byte payment id");
        let primary =
            MoneroAddress::from_str(Network::Mainnet, &vectors.primary).expect("the primary");
        assert_eq!(
            primary.with_payment_id(payment_id).expect("a legacy address").to_string(),
            integrated.address,
            "the vector is the primary address carrying the payment id"
        );

        let recipient = Recipient::parse(&integrated.address).expect("an integrated address");
        assert_eq!(recipient.payment_id(), Some(payment_id));
        assert_eq!(recipient.canonical(), integrated.address);
    }

    #[test]
    fn whitespace_is_trimmed_and_the_canonical_form_is_what_travels() {
        let account = account();
        let wanted = account.address(0, 7).expect("an address");
        let parsed = Recipient::parse(&format!("  {wanted}\n")).expect("a subaddress");
        assert_eq!(parsed.canonical(), wanted);
        assert_eq!(parsed.address().to_string(), wanted);
    }

    #[test]
    fn a_refused_address_never_reaches_the_message() {
        let garbage = "4not_a_real_address_at_all_but_quite_long_indeed";
        let error = Recipient::parse(garbage).expect_err("garbage");
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains("not_a_real_address"), "{rendered}");
        assert!(matches!(error, MoneroError::Address(_)), "{error}");
    }
}
