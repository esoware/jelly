use jelly_core::{ChainId, Transport};
use jelly_net::{Endpoint, EndpointOrigin, NetError, UrlRejection};

#[test]
fn nonsense_is_refused_without_echoing_it_back() {
    for (input, reason) in [
        ("", UrlRejection::Empty),
        ("   ", UrlRejection::Empty),
        ("example.com", UrlRejection::Unparsable),
        ("ftp://example.com", UrlRejection::UnsupportedScheme),
        ("https://", UrlRejection::Unparsable),
    ] {
        let error =
            Endpoint::parse(ChainId::Ethereum, Transport::HttpJsonrpc, input).expect_err(input);
        assert!(
            matches!(error, NetError::InvalidUrl { reason: found } if found == reason),
            "{input}"
        );
        assert!(!error.to_string().contains("example.com"), "{input}");
    }
}

/// Matching every variant means a new rejection doesn't compile until it's placed here,
/// and the test below checks the placement against `UrlRejection::ALL`.
const fn position(reason: UrlRejection) -> usize {
    match reason {
        UrlRejection::Empty => 0,
        UrlRejection::Unparsable => 1,
        UrlRejection::MissingHost => 2,
        UrlRejection::UnsupportedScheme => 3,
        UrlRejection::PlaintextNotAllowed => 4,
        UrlRejection::MissingPort => 5,
        UrlRejection::UnusedTransport => 6,
    }
}

#[test]
fn every_rejection_is_listed_once_and_has_its_own_sentence() {
    let positions: Vec<usize> = UrlRejection::ALL.into_iter().map(position).collect();
    assert_eq!(positions, (0..UrlRejection::ALL.len()).collect::<Vec<_>>());
    for (index, reason) in UrlRejection::ALL.into_iter().enumerate() {
        assert!(!reason.message().is_empty());
        for other in UrlRejection::ALL.into_iter().skip(index.saturating_add(1)) {
            assert_ne!(reason.message(), other.message());
        }
    }
}

#[test]
fn the_input_keeps_what_the_user_typed() {
    let endpoint =
        Endpoint::parse(ChainId::Ethereum, Transport::HttpJsonrpc, "  HTTPS://Example.com/eth/  ")
            .expect("a user address")
            .with_origin(EndpointOrigin::User);

    assert_eq!(endpoint.url(), "https://example.com/eth");
    assert_eq!(endpoint.input(), "HTTPS://Example.com/eth/");
    assert_eq!(endpoint.origin(), EndpointOrigin::User);
    assert_eq!(endpoint.note(), None);
}

#[test]
fn declaration_order_is_preference_order_among_origins() {
    assert!(EndpointOrigin::Pinned < EndpointOrigin::User);
    assert!(EndpointOrigin::User < EndpointOrigin::Default);
}

/// A DNS name that merely starts with `127.` can resolve anywhere, so plaintext to it
/// needs the user's explicit permission like any other remote host.
#[test]
fn a_hostname_that_starts_with_127_is_not_loopback() {
    for input in ["http://127.attacker.example", "http://127.0.0.1.attacker.example:8545"] {
        let parsed = Endpoint::parse(ChainId::Ethereum, Transport::HttpJsonrpc, input);
        assert!(
            matches!(
                parsed,
                Err(NetError::InvalidUrl { reason: UrlRejection::PlaintextNotAllowed })
            ),
            "{input} was admitted as plaintext loopback: {parsed:?}"
        );
    }
}
