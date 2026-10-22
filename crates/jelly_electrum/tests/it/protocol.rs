use bitcoin::consensus::deserialize;
use bitcoin::{Transaction, Txid};
use jelly_core::{ChainError, ChainId, RejectionKind, RetryPolicy};
use jelly_electrum::{
    ElectrumError, HistoryDigest, HistoryEntry, Method, ProtocolVersion,
    test_util::{Answer, FakeElectrum},
};

use crate::common::{
    BLOCK_HEADER, HISTORY, LISTUNSPENT, SERVER_VERSION, SERVER_VERSION_FAIL, answer_of,
    header_hash, header_hex, number_at, numbers_at, quoted, raw, rows, script, strings_at, text_at,
    vector,
};

/// The Bitcoin genesis coinbase, a real transaction with a well-known txid.
const GENESIS_COINBASE: &str = "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff4d04ffff001d0104455468652054696d65732030332f4a616e2f32303039204368616e63656c6c6f72206f6e206272696e6b206f66207365636f6e64206261696c6f757420666f722062616e6b73ffffffff0100f2052a01000000434104678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5fac00000000";
const GENESIS_COINBASE_TXID: &str =
    "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b";

/// A Litecoin mainnet `HogEx` as Litecoin Core serves it at its default
/// `-rpcserialversion=2`.
///
/// A `HogEx` is the integrating transaction that carries every peg-out from the
/// MWEB. This serialization has the MWEB flag bit 0x08 and one zero byte for the
/// absent MWEB body, and its txid hashes the serialization without either.
const LITECOIN_HOGEX: &str = "020000000008025d4ced0e013362e9fd61f564ed8fbfb7e87ec4d7bde765dfb1d00a947bc609610000000000ffffffff089df252a764e1759cad007c353e4bfe723589f999574c0f2e587c92fbc35b5e0000000000ffffffff01d9c77186cc290000225820ec36b7d01e94af8029e9429978d9a45c27e334d18c26ab2518f8b3c7fb3e79cd0000000000";
const LITECOIN_HOGEX_TXID: &str =
    "4a0fcff630b8c758ba9591cd20a50be1bdd372c8c23d26bb6f1bf3b27f3edbeb";

/// What Fulcrum answers a successful `blockchain.transaction.broadcast` with,
/// instead of the txid, when the client name parses as a version below 3.3.4.
const FULCRUM_OLD_CLIENT_WARNING: &str = "<br/><br/>Your transaction was successfully broadcast.<br/><br/>However, you are using a VULNERABLE version of Electrum.<br/>Download the latest version from this web site ONLY:<br/>https://electrum.org/<br/><br/>";

fn genesis_coinbase() -> Option<Transaction> {
    let bytes = hex::decode(GENESIS_COINBASE).ok()?;
    deserialize(&bytes).ok()
}

mod negotiation {
    use super::*;

    #[tokio::test]
    async fn every_recorded_answer_negotiates() {
        for case in rows(&vector(SERVER_VERSION), "/cases") {
            let fake = FakeElectrum::new();
            fake.always(Method::ServerVersion, answer_of(&case));
            let client = fake.client().await.expect("the answer negotiates");
            assert_eq!(client.software().await, Some(text_at(&case, "/software")));
            assert_eq!(
                client.protocol().await.map(|protocol| protocol.to_string()),
                Some(text_at(&case, "/protocol"))
            );
        }
    }

    #[tokio::test]
    async fn every_recorded_failure_is_refused_by_the_right_variant() {
        for case in rows(&vector(SERVER_VERSION_FAIL), "/cases") {
            let fake = FakeElectrum::new();
            fake.always(Method::ServerVersion, answer_of(&case));
            let error = fake.client().await.expect_err("the handshake fails");
            let name = text_at(&case, "/expect");
            let is_matched = match name.as_str() {
                "rpc" => matches!(error, ElectrumError::Rpc { .. }),
                "version_too_old" => matches!(error, ElectrumError::VersionTooOld { .. }),
                "version_too_new" => matches!(error, ElectrumError::VersionTooNew { .. }),
                "unreadable_version" => matches!(error, ElectrumError::UnreadableVersion { .. }),
                other => panic!("the vector names an unknown outcome: {other}"),
            };
            assert!(is_matched, "{name}: {error}");
        }
    }

    #[tokio::test]
    async fn a_version_newer_than_this_build_becomes_protocol_too_new() {
        let fake = FakeElectrum::new();
        fake.always(Method::ServerVersion, Answer::Result(r#"["Fulcrum 2.0.0","1.9"]"#.to_owned()));
        let error = fake.client().await.expect_err("the handshake fails");
        let chain = error.into_chain_error(ChainId::Bitcoin);
        assert!(
            matches!(chain, ChainError::ProtocolTooNew { ref version, .. } if version == "1.9"),
            "{chain}"
        );
    }

    /// Offered `["1.4", "1.6"]`, `ElectrumX` 1.16 answers its own maximum 1.4.2,
    /// 1.17 and 1.18 answer 1.4.3, and Fulcrum 1.9 to 1.12 answer 1.5.2 or 1.5.3.
    #[tokio::test]
    async fn a_three_part_protocol_version_negotiates() {
        for (software, protocol) in [
            ("ElectrumX 1.16.0", "1.4.2"),
            ("ElectrumX 1.18.0", "1.4.3"),
            ("Fulcrum 1.12.0", "1.5.3"),
        ] {
            let fake = FakeElectrum::new();
            fake.always(
                Method::ServerVersion,
                Answer::Result(format!(r#"["{software}","{protocol}"]"#)),
            );
            let client = fake
                .client()
                .await
                .unwrap_or_else(|error| panic!("{software} answering {protocol}: {error}"));
            let negotiated = client.protocol().await.expect("a cached socket");
            assert!(negotiated >= ProtocolVersion::MINIMUM, "{protocol}");
            assert!(negotiated < ProtocolVersion::V1_6, "{protocol}");
        }
    }
}

mod scripts {
    use super::*;

    #[tokio::test]
    async fn history_decodes_through_the_wire_path() {
        let fake = FakeElectrum::new();
        fake.always(Method::GetHistory, Answer::Result(raw(HISTORY, "/result")));
        let client = fake.client().await.expect("the fake negotiates");
        let answered = client.histories(&[script()]).await.expect("a history");
        let entries = answered.first().expect("one script, one history");
        let want = vector(HISTORY);

        assert_eq!(
            entries.len(),
            usize::try_from(number_at(&want, "/expect/entries")).expect("an entry count")
        );
        let confirmed = entries.iter().filter(|entry| entry.confirmation.is_confirmed()).count();
        assert_eq!(
            confirmed,
            usize::try_from(number_at(&want, "/expect/confirmed")).expect("a confirmed count")
        );
        assert_eq!(
            entries.first().and_then(|entry| entry.confirmation.height()),
            Some(
                u32::try_from(number_at(&want, "/expect/first_confirmed_height"))
                    .expect("a block height")
            )
        );
        let fees: Vec<u64> =
            entries.iter().filter_map(|entry| entry.fee.map(bitcoin::Amount::to_sat)).collect();
        assert_eq!(fees, numbers_at(&want, "/expect/mempool_fees"));
    }

    #[tokio::test]
    async fn the_scripthash_the_server_is_asked_for_is_the_published_one() {
        let fake = FakeElectrum::new();
        fake.always(Method::GetHistory, Answer::Result(raw(HISTORY, "/result")));
        let client = fake.client().await.expect("the fake negotiates");
        client.histories(&[script()]).await.expect("a history");

        let published = text_at(&vector(HISTORY), "/scripthash");
        let asked = fake.calls_to(Method::GetHistory);
        assert_eq!(
            asked.first().map(|call| call.params.clone()),
            Some(format!("[\"{published}\"]"))
        );
    }

    #[tokio::test]
    async fn the_digest_survives_the_order_a_server_chose() {
        let recorded = rows(&vector(HISTORY), "/result");
        let mut reversed = recorded.clone();
        reversed.reverse();

        let fake = FakeElectrum::new();
        fake.on(Method::GetHistory, Answer::Result(serde_json::Value::Array(recorded).to_string()));
        fake.on(Method::GetHistory, Answer::Result(serde_json::Value::Array(reversed).to_string()));
        let client = fake.client().await.expect("the fake negotiates");

        let first = client.histories(&[script()]).await.expect("a history");
        let second = client.histories(&[script()]).await.expect("a history");
        let digest = |answered: &[Vec<HistoryEntry>]| {
            HistoryDigest::of(answered.first().map(Vec::as_slice).unwrap_or_default())
        };
        assert_eq!(digest(&first), digest(&second));
        assert!(digest(&first).is_some());
    }

    #[tokio::test]
    async fn the_digest_moves_when_a_confirmation_lands() {
        let fake = FakeElectrum::new();
        let pending = format!(r#"[{{"tx_hash":"{GENESIS_COINBASE_TXID}","height":0}}]"#);
        let mined = format!(r#"[{{"tx_hash":"{GENESIS_COINBASE_TXID}","height":2473}}]"#);
        fake.on(Method::GetHistory, Answer::Result(pending));
        fake.on(Method::GetHistory, Answer::Result(mined));
        let client = fake.client().await.expect("the fake negotiates");

        let before = client.histories(&[script()]).await.expect("a history");
        let after = client.histories(&[script()]).await.expect("a history");
        assert_ne!(
            HistoryDigest::of(before.first().map(Vec::as_slice).unwrap_or_default()),
            HistoryDigest::of(after.first().map(Vec::as_slice).unwrap_or_default())
        );
    }

    #[tokio::test]
    async fn unspent_outputs_decode() {
        let fake = FakeElectrum::new();
        fake.always(Method::ListUnspent, Answer::Result(raw(LISTUNSPENT, "/result")));
        let client = fake.client().await.expect("the fake negotiates");
        let answered = client.unspents(&[script()]).await.expect("unspents");
        let unspents = answered.first().expect("one script, one list");
        let want = vector(LISTUNSPENT);

        let outpoints: Vec<String> =
            unspents.iter().map(|unspent| unspent.outpoint.to_string()).collect();
        assert_eq!(outpoints, strings_at(&want, "/expect/outpoints"));
        let values: Vec<u64> = unspents.iter().map(|unspent| unspent.value.to_sat()).collect();
        assert_eq!(values, numbers_at(&want, "/expect/values"));
        let heights: Vec<serde_json::Value> =
            unspents.iter().map(|unspent| unspent.height.into()).collect();
        assert_eq!(heights, rows(&want, "/expect/heights"));
    }
}

mod headers {
    use super::*;

    #[tokio::test]
    async fn the_tip_decodes_into_its_height() {
        let fake = FakeElectrum::new();
        fake.always(Method::HeadersSubscribe, Answer::Result(raw(BLOCK_HEADER, "/tip")));
        let client = fake.client().await.expect("the fake negotiates");
        assert_eq!(client.tip_height().await.expect("a tip"), 1);
    }

    #[tokio::test]
    async fn a_tip_whose_header_does_not_decode_is_refused() {
        let fake = FakeElectrum::new();
        fake.always(
            Method::HeadersSubscribe,
            Answer::Result(r#"{"height":1,"hex":"0100"}"#.to_owned()),
        );
        let client = fake.client().await.expect("the fake negotiates");
        let error = client.tip_height().await.expect_err("a refusal");
        assert!(
            matches!(error, ElectrumError::Protocol { method: Method::HeadersSubscribe, .. }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_batch_of_headers_keeps_the_order_it_asked_for() {
        let fake = FakeElectrum::new();
        fake.always(
            Method::BlockHeader,
            Answer::Each(vec![quoted(&header_hex(0)), quoted(&header_hex(1))]),
        );
        let client = fake.client().await.expect("the fake negotiates");
        let headers = client.block_headers(&[0, 1]).await.expect("two headers");
        let hashes: Vec<String> =
            headers.iter().map(|header| header.block_hash().to_string()).collect();
        assert_eq!(hashes, vec![header_hash(0), header_hash(1)]);
    }

    #[tokio::test]
    async fn consecutive_headers_that_do_not_chain_are_refused() {
        let fake = FakeElectrum::new();
        fake.serves_bitcoin();
        fake.always(
            Method::BlockHeader,
            Answer::Each(vec![quoted(&header_hex(1)), quoted(&header_hex(0))]),
        );
        let client = fake.client().await.expect("the fake negotiates");
        let error = client.block_headers(&[0, 1]).await.expect_err("a refusal");
        assert!(
            matches!(
                error,
                ElectrumError::Protocol {
                    method: Method::BlockHeader,
                    detail: "two consecutive headers do not chain",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_header_that_is_not_eighty_bytes_is_refused() {
        let fake = FakeElectrum::new();
        let client = fake.client().await.expect("the fake negotiates");
        client.verify_identity().await.expect("the socket is proved");
        for header in ["", "0100", "zz", &"00".repeat(79), &"00".repeat(81)] {
            fake.always(Method::BlockHeader, Answer::Result(quoted(header)));
            let error = client.block_headers(&[0]).await.expect_err("a refusal");
            assert!(
                matches!(
                    error,
                    ElectrumError::Protocol {
                        method: Method::BlockHeader,
                        detail: "a header is not eighty bytes of hex and the chain's own framing",
                        ..
                    }
                ),
                "{header}: {error}"
            );
        }
    }
}

mod txs {
    use super::*;

    #[tokio::test]
    async fn a_transaction_decodes_and_keeps_its_id() {
        let fake = FakeElectrum::new();
        fake.always(Method::GetTransaction, Answer::Result(quoted(GENESIS_COINBASE)));
        let client = fake.client().await.expect("the fake negotiates");
        let txid: Txid = GENESIS_COINBASE_TXID.parse().expect("a txid");
        let fetched = client.txs(&[txid]).await.expect("one transaction");
        let served = fetched.first().expect("one txid, one transaction");
        assert_eq!(served.tx.compute_txid(), txid);
        assert!(!served.is_hogex, "a canonical serialization is not a HogEx");
    }

    #[tokio::test]
    async fn a_transaction_that_is_not_the_one_asked_for_is_refused() {
        let fake = FakeElectrum::new();
        fake.always(Method::GetTransaction, Answer::Result(quoted(GENESIS_COINBASE)));
        let client = fake.client().await.expect("the fake negotiates");
        let other: Txid = "0000000000000000000000000000000000000000000000000000000000000001"
            .parse()
            .expect("a txid");
        let error = client.txs(&[other]).await.expect_err("a refusal");
        assert!(
            matches!(
                error,
                ElectrumError::Protocol {
                    method: Method::GetTransaction,
                    detail: "the server answered with a different transaction",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_broadcast_the_server_accepts_returns_the_id_we_computed() {
        let fake = FakeElectrum::new();
        fake.always(Method::Broadcast, Answer::Result(quoted(GENESIS_COINBASE_TXID)));
        let client = fake.client().await.expect("the fake negotiates");
        let tx = genesis_coinbase().expect("a transaction");
        let txid = client.broadcast(&tx).await.expect("the broadcast lands");
        assert_eq!(txid.to_string(), GENESIS_COINBASE_TXID);
    }

    #[tokio::test]
    async fn a_broadcast_answered_with_a_different_id_is_refused() {
        let fake = FakeElectrum::new();
        fake.always(
            Method::Broadcast,
            Answer::Result(quoted(
                "0000000000000000000000000000000000000000000000000000000000000001",
            )),
        );
        let client = fake.client().await.expect("the fake negotiates");
        let error = client
            .broadcast(&genesis_coinbase().expect("a transaction"))
            .await
            .expect_err("the answer is refused");
        assert!(
            matches!(
                error,
                ElectrumError::Protocol {
                    method: Method::Broadcast,
                    detail: "the server answered with a different transaction id",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_refused_broadcast_becomes_a_classified_rejection() {
        let fake = FakeElectrum::new();
        fake.always(
            Method::Broadcast,
            Answer::Error {
                code: -26,
                message: "the transaction was rejected by network rules.\n\nmin relay fee not \
                          met, 84 < 141"
                    .to_owned(),
            },
        );
        let client = fake.client().await.expect("the fake negotiates");
        let error = client
            .broadcast(&genesis_coinbase().expect("a transaction"))
            .await
            .expect_err("a rejection");
        let rejection = error.rejection().expect("the failure carries a rejection");
        assert_eq!(rejection.kind, RejectionKind::BelowRelayFee);
        assert_eq!(rejection.retry, RetryPolicy::DidNotLand);
    }

    #[tokio::test]
    async fn a_litecoin_hogex_decodes_keeps_its_id_and_is_marked_as_one() {
        let fake = FakeElectrum::new();
        fake.always(Method::GetTransaction, Answer::Result(quoted(LITECOIN_HOGEX)));
        let client = fake.client().await.expect("the fake negotiates");
        let txid: Txid = LITECOIN_HOGEX_TXID.parse().expect("a txid");
        let fetched = client.txs(&[txid]).await.expect("a HogEx is a transaction");
        let served = fetched.first().expect("one txid, one transaction");
        assert_eq!(served.tx.compute_txid(), txid);
        assert_eq!((served.tx.input.len(), served.tx.output.len()), (2, 1));
        assert!(served.is_hogex, "the MWEB serialization marks a HogEx");
    }

    /// The fake plays Fulcrum, which answers with its warning instead of the
    /// txid for any client name it reads as a version below 3.3.4.
    ///
    /// Its `Version(QString)` drops everything up to the first slash and a
    /// leading `v`, and stops at the first dot-separated part that is not a
    /// number.
    #[tokio::test]
    async fn fulcrum_does_not_read_the_client_name_as_an_outdated_electrum() {
        let fake = FakeElectrum::new();
        let client = fake.client().await.expect("the fake negotiates");
        let hello = fake.calls_to(Method::ServerVersion);
        let params: serde_json::Value =
            serde_json::from_str(&hello.first().expect("the handshake").params).expect("json");
        let name = params.get(0).and_then(serde_json::Value::as_str).expect("a client name");

        let trimmed = name.trim();
        let after_slash = trimmed.split_once('/').map_or(trimmed, |(_, rest)| rest);
        let text = after_slash.strip_prefix(['v', 'V']).unwrap_or(after_slash);
        let parts: Vec<&str> = text.split('.').take(3).collect();
        let number = |index: usize| {
            let part = parts.get(index)?;
            part.trim().parse::<u32>().ok()
        };
        let major = number(0);
        let minor = major.and_then(|_| number(1));
        let revision = minor.and_then(|_| parts.get(2)).map(|part| {
            let digits: String = part.trim().chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u32>().unwrap_or_default()
        });
        let version =
            (major.unwrap_or_default(), minor.unwrap_or_default(), revision.unwrap_or_default());
        let is_flagged = version != (0, 0, 0) && version < (3, 3, 4);
        let answer = if is_flagged { FULCRUM_OLD_CLIENT_WARNING } else { GENESIS_COINBASE_TXID };
        fake.always(
            Method::Broadcast,
            Answer::Result(serde_json::to_string(answer).expect("a json string")),
        );

        let tx = genesis_coinbase().expect("a transaction");
        let txid = client
            .broadcast(&tx)
            .await
            .unwrap_or_else(|error| panic!("Fulcrum relayed it for client name {name:?}: {error}"));
        assert_eq!(txid, tx.compute_txid());
    }
}
