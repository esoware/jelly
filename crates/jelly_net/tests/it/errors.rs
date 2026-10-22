use std::io;
use std::time::Duration;

use jelly_core::{ChainError, ChainId, EndpointLabel, ErrorCode, FailureKind};
use jelly_net::{NetError, UrlRejection};

/// A configuration failure is this build's own, so it must not read to the user like a
/// node's answer.
#[test]
fn a_configuration_failure_is_internal_and_a_node_failure_is_an_rpc_failure() {
    let label = EndpointLabel::new("https", "node.example", None);
    let configuration = [
        NetError::InvalidUrl { reason: UrlRejection::Unparsable },
        NetError::Defaults { chain: ChainId::Bitcoin, detail: "the file does not parse" },
        NetError::config("a request could not be serialized", io::Error::other("broken")),
    ];
    for error in configuration {
        let chain = ChainError::from(error);
        assert!(matches!(chain, ChainError::Internal { .. }), "{chain:?}");
        assert_eq!(chain.code(), ErrorCode::Internal);
    }

    let answers = [
        NetError::endpoint(&label, FailureKind::Connect, io::Error::other("refused")),
        NetError::timed_out(&label, Duration::from_secs(1)),
        NetError::protocol(&label, "the answer is not a json-rpc response"),
        NetError::Status { endpoint: label.clone(), status: 502 },
        NetError::Rpc { endpoint: label.clone(), code: -32000, message: "busy".to_owned() },
    ];
    for error in answers {
        let chain = ChainError::from(error);
        assert_eq!(chain.code(), ErrorCode::Rpc, "{chain:?}");
    }
}

#[test]
fn a_typed_cause_survives_the_round_trip_through_a_failure() {
    #[derive(Debug, thiserror::Error)]
    #[error("the protocol crate's own failure")]
    struct Mine;

    let label = EndpointLabel::new("https", "node.example", None);
    let error = NetError::endpoint(&label, FailureKind::Protocol, Mine);
    assert!(error.source_of::<Mine>().is_some());
    assert!(error.source_of::<io::Error>().is_none());
    error.into_source_of::<Mine>().expect("the typed cause comes back");

    let plain = NetError::protocol(&label, "not a typed cause");
    assert!(matches!(
        plain.into_source_of::<Mine>(),
        Err(NetError::Protocol { detail: "not a typed cause", .. })
    ));
}
