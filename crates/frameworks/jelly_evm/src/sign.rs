use alloy::consensus::{SignableTransaction as _, Signed};
use alloy::eips::eip2718::Encodable2718 as _;
use alloy::primitives::Signature;
use jelly_core::{KeyKind, KeyMaterial, RawTx, SignedTx, TxId};
use jelly_keypair::Secp256k1Key;

use crate::address;
use crate::error::EvmError;
use crate::params::EvmParams;
use crate::payload::EvmPayload;

/// Makes no network call and keeps no key. Refuses a payload for another chain id
/// and a key that does not derive the payload's sender.
pub(crate) fn sign(
    params: &EvmParams,
    payload: &EvmPayload,
    key: &KeyMaterial,
) -> Result<SignedTx, EvmError> {
    if key.kind() != KeyKind::Secp256k1 {
        return Err(EvmError::WrongKeyKind { got: key.kind() });
    }
    if payload.chain_id() != params.chain_id {
        return Err(EvmError::ForeignChainId { chain_id: payload.chain_id() });
    }
    let digest = payload.signature_hash();

    key.with_bytes(|bytes| {
        if bytes.len() != 32 {
            return Err(EvmError::WrongKeyLength { got: bytes.len() });
        }
        let signing = Secp256k1Key::from_secret_bytes(bytes).map_err(EvmError::Signing)?;
        let owner = address::of_public_key(&signing.public_uncompressed())?;
        if owner != payload.from() {
            return Err(EvmError::KeyNotOwner);
        }

        let compact = signing.sign_prehash_recoverable(&digest.0).map_err(EvmError::Signing)?;
        let signature = signature_from_compact(&compact)?;
        let signed: Signed<_> = payload.tx().clone().into_signed(signature);
        Ok(SignedTx {
            raw: RawTx::from_bytes(signed.encoded_2718()),
            txid: TxId::new(params.chain, signed.hash().to_string()),
        })
    })
}

/// Splits `r || s || recovery id` into the y-parity a typed transaction carries.
///
/// Recovery ids two and three occur only when r exceeded the curve order, with
/// probability around 2^-128. They are refused rather than folded onto a parity
/// that would verify against the wrong key.
fn signature_from_compact(compact: &[u8; 65]) -> Result<Signature, EvmError> {
    let [body @ .., recovery] = *compact;
    let parity = match recovery {
        0 => false,
        1 => true,
        got => return Err(EvmError::UnencodableRecovery { got }),
    };
    Ok(Signature::from_bytes_and_parity(&body, parity))
}

#[cfg(test)]
mod tests {
    use alloy::consensus::TxEip1559;
    use alloy::eips::eip2930::AccessList;
    use alloy::hex;
    use alloy::primitives::{Address, Bytes, TxKind, U256};
    use jelly_core::{Amount, AssetId};
    use zeroize::Zeroizing;

    use super::*;
    use crate::fee::FeePair;
    use crate::fixtures::address;
    use crate::payload::{self, Intent, Shape};
    use crate::test_util::{ETHEREUM, POLYGON};

    #[test]
    fn a_recovery_id_above_one_is_refused_rather_than_folded() {
        let mut compact = [0u8; 65];
        compact[63] = 1;
        compact[31] = 1;
        for recovery in [0u8, 1] {
            compact[64] = recovery;
            let signature = signature_from_compact(&compact).expect("a signature");
            assert_eq!(signature.v(), recovery == 1);
        }
        for recovery in [2u8, 3, 27, 255] {
            compact[64] = recovery;
            assert!(
                matches!(
                    signature_from_compact(&compact),
                    Err(EvmError::UnencodableRecovery { got }) if got == recovery
                ),
                "{recovery}"
            );
        }
    }

    /// Trust Wallet Core's own Polygon signing vector, which Polygon mined in block
    /// 41,713,617 under the txid asserted here.
    #[test]
    fn a_polygon_transaction_signs_to_the_bytes_trust_wallet_core_broadcast() {
        let secret =
            hex::decode("4a160b803c4392ea54865d0c5286846e7ad5e68fbd78880547697472b22ea7ab")
                .expect("hex");
        let to: Address = "0xfd225c9e6601c9d38d8f98d8731bf59efcf8c0e3".parse().expect("address");
        let value = 1_000_000_000_000_000_000u128;
        let tx = TxEip1559 {
            chain_id: 137,
            nonce: 1,
            gas_limit: 116_000,
            max_fee_per_gas: 617_347_611_864,
            max_priority_fee_per_gas: 35_941_173_184,
            to: TxKind::Call(to),
            value: U256::from(value),
            access_list: AccessList::default(),
            input: Bytes::from(hex::decode("c78cf1a0").expect("hex")),
        };
        let intent = Intent {
            from: "0x3dbea3b36beaa4a23fd377acf83a58d39e46b403".parse().expect("address"),
            asset: AssetId::Native(POLYGON.chain),
            to,
            amount: Amount::from_base_units(value),
        };
        let key = KeyMaterial::new(KeyKind::Secp256k1, Zeroizing::new(secret));
        let signed = sign(&POLYGON, &EvmPayload::new(tx, intent), &key).expect("signed");
        assert_eq!(
            hex::encode(signed.raw.as_bytes()),
            "02f87a81890185085e42c7c0858fbcc8fcd88301c52094fd225c9e6601c9d38d8f98d8731bf59efcf8c0\
             e3880de0b6b3a764000084c78cf1a0c001a04bcf92394d53d4908130cc6d4f7b2491967f9d6c59292b84\
             c1f56adc49f6c458a073e09f45d64078c41a7946ffdb1dee8e604eb76f318088490f8f661bb7ddfc54"
        );
        assert_eq!(
            signed.txid.value(),
            "0x0f6c4f7a893c3f08be30d2ea24479d7ed4bdba40875d07cfd607cf97980b7cf0"
        );
        assert_eq!(signed.chain(), POLYGON.chain);
    }

    #[test]
    fn a_payload_for_another_chain_id_is_named_rather_than_signed() {
        let shape = Shape::of(None, address(2), 1, None);
        let fee = FeePair::new(30, 1).expect("a pair");
        let payload = EvmPayload::new(
            payload::transaction(&POLYGON, 7, 21_000, fee, &shape),
            Intent {
                from: Address::ZERO,
                asset: AssetId::Native(ETHEREUM.chain),
                to: address(2),
                amount: Amount::from_base_units(1),
            },
        );
        let key = KeyMaterial::new(KeyKind::Secp256k1, Zeroizing::new(vec![0x46; 32]));
        assert!(matches!(
            sign(&ETHEREUM, &payload, &key),
            Err(EvmError::ForeignChainId { chain_id: 137 })
        ));
    }
}
