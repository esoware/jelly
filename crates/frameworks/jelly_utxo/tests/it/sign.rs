use jelly_core::{
    ChainAdapter as _, ChainError, KeyKind, ScriptKind, TxPayload, TxPreview, UnsignedTx,
    test_util::key_material,
};

use crate::common::{BITCOIN, CHAINS, LITECOIN, Setup, decode, kind_of, synced};

const FUNDING_SAT: u64 = 500_000;
const PAYMENT_SAT: u64 = 100_000;

#[tokio::test]
async fn every_spendable_kind_signs_into_the_witness_or_script_the_network_expects() {
    for params in CHAINS {
        for &kind in params.script_kinds {
            let (harness, wallet) = synced(Setup::on(&params), kind, &[FUNDING_SAT]).await.unwrap();
            let unsigned = harness
                .adapter
                .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "2"))
                .await
                .unwrap();
            let signed = harness.adapter.sign_tx(&unsigned, &[wallet.ours.material()]).unwrap();
            assert_eq!(signed.chain(), params.chain);

            let decoded = decode(signed.raw.as_bytes()).unwrap();
            assert_eq!(decoded.compute_txid().to_string(), signed.txid.value(), "{kind:?}");
            let input = decoded.input.first().expect("one input");
            let raw = signed.raw.as_bytes();
            match kind {
                ScriptKind::P2tr => {
                    assert_eq!(input.witness.len(), 1, "a key-path spend is one signature");
                    assert!(input.script_sig.is_empty());
                    assert_eq!(raw.get(4..6), Some(&[0x00, 0x01][..]));
                }
                ScriptKind::P2wpkh => {
                    assert_eq!(input.witness.len(), 2, "signature and public key");
                    assert!(input.script_sig.is_empty());
                    assert_eq!(
                        raw.get(4..6),
                        Some(&[0x00, 0x01][..]),
                        "the segwit marker and flag"
                    );
                }
                ScriptKind::P2shP2wpkh => {
                    assert_eq!(input.witness.len(), 2);
                    assert_eq!(input.script_sig.len(), 23, "the redeem script is pushed");
                    assert_eq!(raw.get(4..6), Some(&[0x00, 0x01][..]));
                }
                ScriptKind::P2pkh => {
                    assert!(input.witness.is_empty(), "a legacy input carries no witness");
                    assert!(!input.script_sig.is_empty());
                    assert_ne!(raw.get(4..6), Some(&[0x00, 0x01][..]), "no segwit marker");
                }
                other @ (ScriptKind::TonV5r1 | ScriptKind::TonV4r2 | ScriptKind::TonV3r2) => {
                    panic!("{other} is no output script of {}", params.chain)
                }
            }

            let again = harness.adapter.sign_tx(&unsigned, &[wallet.ours.material()]).unwrap();
            assert_eq!(again.txid, signed.txid, "{}: {kind:?}", params.chain);
            if kind == ScriptKind::P2tr {
                assert_ne!(
                    again.raw.as_bytes(),
                    signed.raw.as_bytes(),
                    "fresh auxiliary randomness"
                );
            } else {
                assert_eq!(
                    again.raw.as_bytes(),
                    signed.raw.as_bytes(),
                    "{}: {kind:?} signs deterministically",
                    params.chain
                );
            }
        }
    }
}

#[tokio::test]
async fn a_key_that_does_not_control_the_address_is_refused() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let unsigned = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "2"))
            .await
            .unwrap();

        let stranger = harness.key(kind_of(&params), 42).unwrap();
        let error = harness
            .adapter
            .sign_tx(&unsigned, &[stranger.material()])
            .expect_err("the wrong key is refused");
        assert!(error.to_string().contains("different address"), "{error}");

        let ed25519 = key_material(KeyKind::Ed25519Seed, 7);
        assert!(harness.adapter.sign_tx(&unsigned, &[ed25519]).is_err(), "the curve is wrong");
    }
}

#[tokio::test]
async fn a_payload_from_another_chain_or_from_nowhere_is_refused() {
    for (params, other) in [(BITCOIN, LITECOIN), (LITECOIN, BITCOIN)] {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let unsigned = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "2"))
            .await
            .unwrap();
        let keys = [wallet.ours.material()];

        let foreign = UnsignedTx {
            preview: TxPreview { chain: other.chain, ..unsigned.preview.clone() },
            payload: TxPayload::from_bytes(unsigned.payload.as_bytes().to_vec()),
        };
        let error = harness.adapter.sign_tx(&foreign, &keys).expect_err("another chain's payload");
        assert!(matches!(error, ChainError::InvalidInput(..)), "{}: {error}", params.chain);

        let nonsense = UnsignedTx {
            preview: unsigned.preview,
            payload: TxPayload::from_bytes(b"not a payload".to_vec()),
        };
        let error = harness.adapter.sign_tx(&nonsense, &keys).expect_err("not a payload");
        assert!(matches!(error, ChainError::Signing(_)), "{}: {error}", params.chain);
    }
}
