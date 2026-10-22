use bitcoin::BlockHash;
use bitcoin::hashes::Hash as _;
use jelly_core::{ChainId, Transport, test_util::FakeClock};
use jelly_electrum::{
    Checkpoint, ElectrumChain, ElectrumError, Method,
    test_util::{
        Answer, ENDPOINT_URL, FakeElectrum, bitcoin_chain, bitcoin_genesis, bitcoin_genesis_header,
        foreign_genesis, foreign_genesis_header,
    },
};
use jelly_net::{Endpoint, Identity, ProbeReport, probe};

use crate::common::{AUXPOW_HEADER, header_hash, header_hex, number_at, quoted, text_at, vector};

#[tokio::test]
async fn a_server_on_the_right_chain_is_confirmed() {
    let fake = FakeElectrum::new();
    fake.serves_bitcoin();
    let client = fake.client().await.expect("the fake negotiates");
    let identity = client.verify_identity().await.expect("the check runs");
    let software = client.software().await.expect("a cached socket");
    assert_eq!(
        identity,
        Identity::confirmed(format!("{software} on genesis {}", bitcoin_genesis()))
    );
}

#[tokio::test]
async fn a_server_on_another_chain_is_a_mismatch_rather_than_an_error() {
    let fake = FakeElectrum::new();
    fake.always(Method::BlockHeader, Answer::Result(quoted(&foreign_genesis_header())));
    let client = fake.client().await.expect("the fake negotiates");
    assert_eq!(
        client.verify_identity().await.expect("the check runs"),
        Identity::Mismatch {
            expected: bitcoin_genesis().to_string(),
            found: foreign_genesis().to_string(),
        }
    );
}

#[tokio::test]
async fn a_read_proves_the_chain_without_an_explicit_identity_check() {
    let fake = FakeElectrum::new();
    fake.always(Method::BlockHeader, Answer::Result(quoted(&foreign_genesis_header())));
    fake.always(Method::FeeHistogram, Answer::Result("[]".to_owned()));
    let client = fake.client().await.expect("the fake negotiates");

    assert!(matches!(client.fee_histogram().await, Err(ElectrumError::WrongChain { .. })));
    assert!(!fake.has_been_called(Method::FeeHistogram));
}

#[tokio::test]
async fn a_failed_genesis_proof_discards_the_socket_before_the_next_use() {
    let fake = FakeElectrum::new();
    fake.always(Method::BlockHeader, Answer::Result(quoted(&foreign_genesis_header())));
    let client = fake.client().await.expect("the fake negotiates");
    assert!(matches!(client.verify_identity().await.unwrap(), Identity::Mismatch { .. }));
    assert_eq!(fake.sockets_opened(), 1);

    assert!(matches!(client.fee_histogram().await, Err(ElectrumError::WrongChain { .. })));
    assert_eq!(fake.sockets_opened(), 2, "the refused live socket was not reused");
    assert!(!fake.has_been_called(Method::FeeHistogram));

    fake.always(Method::BlockHeader, Answer::Result(quoted(&bitcoin_genesis_header())));
    fake.always(Method::FeeHistogram, Answer::Result("[]".to_owned()));
    assert!(client.fee_histogram().await.unwrap().is_empty());
    assert_eq!(fake.sockets_opened(), 3, "a corrected endpoint is proved on a fresh socket");
}

/// Both hashes reach the probe's log line and the pool's `WrongNetwork` error, so
/// they must be in the reversed order explorers print for a reader to look them
/// up.
///
/// The recorded header and its recorded hash are the reference, and the genesis
/// the client expects is a fixture of its own.
#[tokio::test]
async fn a_mismatch_names_block_zero_in_the_order_explorers_print() {
    let expected = BlockHash::all_zeros();
    let fake = FakeElectrum::new();
    fake.always(Method::BlockHeader, Answer::Result(quoted(&header_hex(0))));
    let chain = ElectrumChain { genesis: expected, ..bitcoin_chain() };
    let client = fake.client_expecting(chain).await.expect("the fake negotiates");
    assert_eq!(
        client.verify_identity().await.expect("the check runs"),
        Identity::Mismatch { expected: expected.to_string(), found: header_hash(0) }
    );
}

#[tokio::test]
async fn an_uppercase_advertised_genesis_still_proves_its_chain() {
    let fake = FakeElectrum::new();
    fake.serves_bitcoin();
    fake.always(
        Method::ServerFeatures,
        Answer::Result(format!(
            r#"{{"genesis_hash":" {} "}}"#,
            bitcoin_genesis().to_string().to_ascii_uppercase()
        )),
    );
    let client = fake.client().await.expect("the fake negotiates");
    let identity = client.verify_identity().await.expect("block zero agrees with the claim");
    assert!(matches!(identity, Identity::Confirmed { .. }), "{identity:?}");
}

#[tokio::test]
async fn an_advertised_genesis_that_block_zero_disagrees_with_is_refused() {
    let fake = FakeElectrum::new();
    fake.always(
        Method::ServerFeatures,
        Answer::Result(format!(r#"{{"genesis_hash":"{}"}}"#, foreign_genesis())),
    );
    fake.always(Method::BlockHeader, Answer::Result(quoted(&bitcoin_genesis_header())));
    let client = fake.client().await.expect("the fake negotiates");
    let error = client.verify_identity().await.expect_err("the claim and the proof disagree");
    assert!(
        matches!(error, ElectrumError::Protocol { method: Method::ServerFeatures, .. }),
        "{error}"
    );
}

#[tokio::test]
async fn a_server_without_features_still_proves_its_chain() {
    let fake = FakeElectrum::new();
    fake.always(
        Method::ServerFeatures,
        Answer::Error { code: -32601, message: "unknown method".to_owned() },
    );
    fake.always(Method::BlockHeader, Answer::Result(quoted(&bitcoin_genesis_header())));
    let client = fake.client().await.expect("the fake negotiates");
    let identity = client.verify_identity().await.expect("block zero proves it");
    assert!(matches!(identity, Identity::Confirmed { .. }), "{identity:?}");
}

#[tokio::test]
async fn a_reconnected_socket_that_serves_another_chain_is_refused() {
    let fake = FakeElectrum::new();
    fake.always(Method::ServerFeatures, Answer::Result("{}".to_owned()));
    fake.always(Method::BlockHeader, Answer::Result(quoted(&bitcoin_genesis_header())));
    let client = fake.client().await.expect("the fake negotiates");
    assert!(matches!(
        client.verify_identity().await.expect("the check runs"),
        Identity::Confirmed { .. }
    ));

    // The host starts serving another chain, and the socket goes away.
    fake.always(Method::BlockHeader, Answer::Result(quoted(&foreign_genesis_header())));
    fake.always(Method::FeeHistogram, Answer::HangUp);
    let error = client.fee_histogram().await.expect_err("the new socket is refused");
    assert!(matches!(error, ElectrumError::WrongChain { .. }), "{error}");
}

/// Opening a client only negotiates, so the probe reaches the genesis check and
/// reports a server on another chain as the wrong network, not as unreachable.
#[tokio::test(start_paused = true)]
async fn a_probe_of_a_server_on_another_chain_reports_the_wrong_network() {
    let fake = FakeElectrum::new();
    fake.always(Method::BlockHeader, Answer::Result(quoted(&foreign_genesis_header())));
    let endpoint = Endpoint::parse(ChainId::Bitcoin, Transport::ElectrumSsl, ENDPOINT_URL)
        .expect("a canonical url");

    let report = probe(&fake.connector(bitcoin_chain()), &FakeClock::default(), &endpoint).await;
    assert_eq!(report, ProbeReport::WrongNetwork);
}

/// The recorded header at `index` as the checkpoint of a chain whose block zero is
/// Bitcoin's, which is how Bitcoin and Bitcoin Cash tell their servers apart.
fn checkpointed(index: usize) -> Option<ElectrumChain> {
    let hash = header_hash(index).parse().ok()?;
    Some(ElectrumChain {
        checkpoint: Some(Checkpoint { height: 840_000, hash }),
        ..bitcoin_chain()
    })
}

#[tokio::test]
async fn a_server_proves_block_zero_and_the_checkpoint_in_one_batch() {
    let fake = FakeElectrum::new();
    fake.always(Method::ServerFeatures, Answer::Result("{}".to_owned()));
    fake.always(
        Method::BlockHeader,
        Answer::Each(vec![quoted(&bitcoin_genesis_header()), quoted(&header_hex(1))]),
    );
    let client =
        fake.client_expecting(checkpointed(1).unwrap()).await.expect("the fake negotiates");
    let identity = client.verify_identity().await.expect("the check runs");
    assert!(matches!(identity, Identity::Confirmed { .. }), "{identity:?}");
    let asked: Vec<String> =
        fake.calls_to(Method::BlockHeader).into_iter().map(|call| call.params).collect();
    assert_eq!(asked, ["[0]", "[840000]"]);
}

/// Two chains that share block zero split at a later block, and a server of the
/// other one serves that block differently.
#[tokio::test]
async fn a_server_of_a_chain_that_shares_block_zero_is_refused_at_the_checkpoint() {
    let fake = FakeElectrum::new();
    fake.always(Method::ServerFeatures, Answer::Result("{}".to_owned()));
    fake.always(
        Method::BlockHeader,
        Answer::Each(vec![quoted(&bitcoin_genesis_header()), quoted(&foreign_genesis_header())]),
    );
    let client =
        fake.client_expecting(checkpointed(1).unwrap()).await.expect("the fake negotiates");
    assert_eq!(
        client.verify_identity().await.expect("the check runs"),
        Identity::Mismatch { expected: header_hash(1), found: foreign_genesis().to_string() }
    );
    let error = client.fee_histogram().await.expect_err("the socket is refused");
    assert!(matches!(error, ElectrumError::WrongChain { height: 840_000, .. }), "{error}");
}

/// A merge-mined chain's tip carries its parent chain's proof of work, which only
/// that chain's framing accepts.
#[tokio::test]
async fn a_merge_mined_tip_reads_only_on_a_merge_mined_chain() {
    let recorded = vector(AUXPOW_HEADER);
    let height = number_at(&recorded, "/height");
    let tip = format!(r#"{{"height":{height},"hex":"{}"}}"#, text_at(&recorded, "/hex"));
    let fake = FakeElectrum::new();
    fake.always(Method::HeadersSubscribe, Answer::Result(tip));

    let merge_mined = ElectrumChain { is_merge_mined: true, ..bitcoin_chain() };
    let client = fake.client_expecting(merge_mined).await.expect("the fake negotiates");
    let tip_height = client.tip_height().await.expect("the tip reads");
    assert_eq!(u64::from(tip_height), height);

    let plain = fake.client().await.expect("the fake negotiates");
    assert!(matches!(
        plain.tip_height().await,
        Err(ElectrumError::Protocol { method: Method::HeadersSubscribe, .. })
    ));
}
