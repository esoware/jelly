use std::fmt::Debug;

use jelly_core::{
    Amount, AssetId, ChainId, DerivationPath, EndpointAttempt, EndpointLabel, FailureKind,
    FeeLevel, FeeSelection, KeySource, PendingTxId, SendAmount, TokenId, TxDetail, TxId, TxRequest,
    TxWarning, TxWarningKind, WalletColor,
};
use serde::{Serialize, de::DeserializeOwned};

use crate::common::json;

#[expect(clippy::expect_used, reason = "test helper")]
fn round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let text = json(value);
    let back: T = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(&back, value, "round trip through {text}");
}

mod field_names {
    use super::{
        AssetId, ChainId, EndpointAttempt, EndpointLabel, FailureKind, FeeSelection, SendAmount,
        TokenId, TxRequest, json, round_trip,
    };

    fn request_from(from_address_ids: Vec<i64>) -> TxRequest {
        TxRequest {
            from_address_ids,
            to: String::from("bc1qrecipient"),
            destination_tag: None,
            comment: None,
            should_delete_account: false,
            asset: AssetId::Native(ChainId::Bitcoin),
            amount: SendAmount::Max,
            fee: FeeSelection::NORMAL,
            nonce: None,
            data: None,
        }
    }

    #[test]
    fn a_transfer_request_names_its_sources_in_order() {
        let request = request_from(vec![7, 3]);
        let text = json(&request);
        assert!(text.contains("\"fromAddressIds\":[7,3]"), "{text}");
        assert!(!text.contains("fromAddressId\""), "no single-source field rides along: {text}");
        round_trip(&request);
    }

    #[test]
    fn only_a_request_from_exactly_one_address_has_a_sole_source() {
        assert_eq!(request_from(vec![7]).sole_source(), Some(7));
        assert_eq!(request_from(vec![7, 3]).sole_source(), None);
        assert_eq!(request_from(Vec::new()).sole_source(), None);
    }

    #[test]
    fn an_asset_is_adjacently_tagged_on_kind() {
        assert_eq!(
            json(&AssetId::Native(ChainId::Bitcoin)),
            "{\"kind\":\"native\",\"value\":\"bitcoin\"}"
        );
        assert_eq!(
            json(&AssetId::Token(TokenId::UsdtErc20)),
            "{\"kind\":\"token\",\"value\":\"usdt_erc20\"}"
        );
        for asset in AssetId::all() {
            round_trip(&asset);
        }
    }

    #[test]
    fn an_endpoint_attempt_keeps_its_message_out_of_the_wire_form() {
        let attempt = EndpointAttempt::new(
            EndpointLabel::new("https", "api.trongrid.io", None),
            FailureKind::Timeout,
            "no answer in 15s",
        );
        assert_eq!(
            json(&attempt),
            "{\"endpoint\":\"https://api.trongrid.io\",\"kind\":\"timeout\"}"
        );
    }
}

mod tagged_unions {
    use super::{
        Amount, DerivationPath, FeeLevel, FeeSelection, KeySource, SendAmount, json, round_trip,
    };

    #[test]
    fn a_send_amount_is_tagged_on_kind() {
        assert_eq!(json(&SendAmount::Max), "{\"kind\":\"max\"}");
        assert_eq!(
            json(&SendAmount::Exact { value: Amount::from_base_units(5) }),
            "{\"kind\":\"exact\",\"value\":\"5\"}"
        );
        for amount in [SendAmount::Max, SendAmount::Exact { value: Amount::MAX }] {
            round_trip(&amount);
        }
    }

    #[test]
    fn a_key_source_is_tagged_on_kind() {
        assert_eq!(
            json(&KeySource::Derived { path: DerivationPath::new("m/86'/0'/0'/0/0") }),
            "{\"kind\":\"derived\",\"path\":\"m/86'/0'/0'/0/0\"}"
        );
        assert_eq!(
            json(&KeySource::Imported { vault_entry: String::from("e1") }),
            "{\"kind\":\"imported\",\"vaultEntry\":\"e1\"}"
        );
        assert_eq!(
            json(&KeySource::Subaddress { major: 0, minor: 3 }),
            "{\"kind\":\"subaddress\",\"major\":0,\"minor\":3}"
        );
    }

    #[test]
    fn every_fee_selection_is_tagged_on_kind_with_camel_case_fields() {
        assert_eq!(json(&FeeSelection::NORMAL), "{\"kind\":\"level\",\"level\":\"normal\"}");
        assert_eq!(
            json(&FeeSelection::Rate { rate: String::from("12.5") }),
            "{\"kind\":\"rate\",\"rate\":\"12.5\"}"
        );
        assert_eq!(
            json(&FeeSelection::Evm {
                gas_limit: Some(String::from("21000")),
                max_fee_per_gas: None,
                max_priority_fee_per_gas: None,
            }),
            "{\"kind\":\"evm\",\"gasLimit\":\"21000\",\"maxFeePerGas\":null,\
             \"maxPriorityFeePerGas\":null}"
        );
        assert_eq!(
            json(&FeeSelection::Solana { priority_fee_lamports: String::from("5000") }),
            "{\"kind\":\"solana\",\"priorityFeeLamports\":\"5000\"}"
        );
        assert_eq!(
            json(&FeeSelection::Total { fee: Amount::from_base_units(12) }),
            "{\"kind\":\"total\",\"fee\":\"12\"}"
        );
    }

    #[test]
    fn every_fee_selection_round_trips() {
        let selections = [
            FeeSelection::Level { level: FeeLevel::Slow },
            FeeSelection::NORMAL,
            FeeSelection::Level { level: FeeLevel::Fast },
            FeeSelection::Rate { rate: String::from("1") },
            FeeSelection::Evm {
                gas_limit: None,
                max_fee_per_gas: Some(String::from("1")),
                max_priority_fee_per_gas: Some(String::from("2")),
            },
            FeeSelection::Solana { priority_fee_lamports: String::from("0") },
            FeeSelection::Total { fee: Amount::from_base_units(10) },
        ];
        for selection in selections {
            round_trip(&selection);
        }
    }
}

mod ids {
    use super::{ChainId, PendingTxId, TxId, json, round_trip};

    #[test]
    fn a_pending_id_is_thirty_two_lowercase_hex_digits() {
        let id = PendingTxId::from_bytes([0xab; PendingTxId::LEN]);
        assert_eq!(id.to_string(), "abababababababababababababababab");
        assert_eq!(json(&id), "\"abababababababababababababababab\"");
        assert_eq!(
            serde_json::from_str::<PendingTxId>("\"ABABABABABABABABABABABABABABABAB\"").ok(),
            Some(id),
            "parsing accepts either case"
        );
        round_trip(&id);
    }

    #[test]
    fn a_pending_id_refuses_anything_that_is_not_its_own_shape() {
        for text in [String::from("ab"), String::new(), String::from("zz"), "ab".repeat(17)] {
            let encoded = json(&text);
            assert!(serde_json::from_str::<PendingTxId>(&encoded).is_err(), "{text}");
        }
    }

    #[test]
    fn a_transaction_id_carries_the_chain_it_belongs_to() {
        let id = TxId::new(ChainId::Litecoin, "d4f1");
        assert_eq!(id.chain(), ChainId::Litecoin);
        assert_eq!(id.value(), "d4f1");
        assert_eq!(id.to_string(), "d4f1");
        assert_eq!(json(&id), "{\"chain\":\"litecoin\",\"value\":\"d4f1\"}");
        round_trip(&id);
    }
}

mod colors {
    use super::{WalletColor, json};

    #[test]
    fn colors_round_trip_as_lowercase_hex_without_display_balancing() {
        for (input, expected) in [
            ("#A0b1C2", "#a0b1c2"),
            ("#000000", "#000000"),
            ("#ffffff", "#ffffff"),
            ("#ff0000", "#ff0000"),
        ] {
            let color = WalletColor::parse(input).expect("a six-digit hex color");
            assert_eq!(color.as_str(), expected);
            let encoded = json(&color);
            assert_eq!(encoded, format!("\"{expected}\""));
            assert_eq!(
                serde_json::from_str::<WalletColor>(&encoded).expect("it deserializes"),
                color
            );
        }
        assert_eq!(WalletColor::default().as_str(), "#71717a");
    }

    #[test]
    fn only_opaque_six_digit_hex_is_accepted() {
        for input in [
            "",
            "red",
            "#abc",
            "123456",
            "#12345678",
            "#gggggg",
            " #123456",
            "#123456 ",
            "#\u{ff11}\u{ff12}\u{ff13}",
        ] {
            assert!(WalletColor::parse(input).is_none(), "{input}");
            let encoded = json(&input);
            assert!(serde_json::from_str::<WalletColor>(&encoded).is_err(), "{input}");
        }
    }
}

mod details {
    use super::{TxDetail, TxWarning, TxWarningKind, json};

    #[test]
    fn a_detail_row_is_a_label_and_a_value() {
        let detail = TxDetail::new("Txid", "d4f1");
        assert_eq!(detail.label, "Txid");
        assert_eq!(detail.value, "d4f1");
        assert_eq!(json(&detail), "{\"label\":\"Txid\",\"value\":\"d4f1\"}");
    }

    #[test]
    fn a_warning_carries_a_typed_kind_beside_its_words() {
        let warning =
            TxWarning::new(TxWarningKind::DustOutput, "This output is below the dust limit.");
        assert_eq!(warning.kind, TxWarningKind::DustOutput);
        assert_eq!(warning.message, "This output is below the dust limit.");
        assert_eq!(
            json(&warning),
            "{\"kind\":\"dust_output\",\"message\":\"This output is below the dust limit.\"}"
        );
    }
}
