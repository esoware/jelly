use std::collections::BTreeSet;
use std::error::Error as _;
use std::io;

use jelly_core::{
    Amount, AssetId, ChainError, ChainId, CoreError, ErrorCode, PendingTxId, Rejection,
    RejectionKind, RetryPolicy, TokenId,
};

fn chain_errors() -> Vec<(ChainError, ErrorCode)> {
    vec![
        (
            ChainError::endpoints_down(ChainId::Bitcoin, "sync", Vec::new()),
            ErrorCode::EndpointsDown,
        ),
        (
            ChainError::Rejected(Rejection::new(
                RejectionKind::Dust,
                RetryPolicy::FinalEverywhere,
                "dust",
            )),
            ErrorCode::Rejected,
        ),
        (ChainError::rpc(io::Error::other("the node hung up")), ErrorCode::Rpc),
        (ChainError::invalid_address(io::Error::other("not bech32")), ErrorCode::InvalidAddress),
        (
            ChainError::InsufficientFunds {
                asset: AssetId::Native(ChainId::Bitcoin),
                needed: Amount::from_base_units(2),
                available: Amount::from_base_units(1),
            },
            ErrorCode::InsufficientFunds,
        ),
        (ChainError::signing(io::Error::other("bad key")), ErrorCode::Signing),
        (ChainError::unsupported(io::Error::other("no fee bump here")), ErrorCode::Unsupported),
        (ChainError::invalid_input(io::Error::other("no amount")), ErrorCode::InvalidInput),
        (ChainError::protocol_too_new(ChainId::Monero, "17"), ErrorCode::ProtocolTooNew),
        (ChainError::internal_without_source("a bug"), ErrorCode::Internal),
        (ChainError::internal("a bug with a cause", io::Error::other("disk")), ErrorCode::Internal),
    ]
}

fn core_errors() -> Vec<(CoreError, ErrorCode)> {
    vec![
        (CoreError::Locked, ErrorCode::Locked),
        (CoreError::WrongPassword, ErrorCode::WrongPassword),
        (CoreError::WrongPassphrase, ErrorCode::WrongPassphrase),
        (CoreError::PassphraseRequired, ErrorCode::PassphraseRequired),
        (CoreError::not_found("wallet", "1"), ErrorCode::NotFound),
        (CoreError::AlreadyExists { entity: "address" }, ErrorCode::AlreadyExists),
        (CoreError::invalid_input("amount", "empty"), ErrorCode::InvalidInput),
        (CoreError::NoAdapter(ChainId::Solana), ErrorCode::NoAdapter),
        (CoreError::Unsupported("not on this chain"), ErrorCode::Unsupported),
        (
            CoreError::ConfirmationRequired { expected: String::from("DESTROY") },
            ErrorCode::ConfirmationRequired,
        ),
        (
            CoreError::UnknownPendingTx(PendingTxId::from_bytes([0; PendingTxId::LEN])),
            ErrorCode::UnknownPendingTx,
        ),
        (CoreError::RebuildRequired, ErrorCode::RebuildRequired),
        (CoreError::AddressMismatch { address_id: 3 }, ErrorCode::AddressMismatch),
        (CoreError::storage("open", io::Error::other("disk")), ErrorCode::Storage),
        (CoreError::keychain("read", io::Error::other("locked")), ErrorCode::Keychain),
        (CoreError::internal_without_source("a bug"), ErrorCode::Internal),
        (CoreError::internal("a bug with a cause", io::Error::other("disk")), ErrorCode::Internal),
    ]
}

#[test]
fn every_chain_error_maps_to_its_code() {
    for (error, expected) in chain_errors() {
        assert_eq!(error.code(), expected, "{error}");
        if matches!(
            error,
            ChainError::Rpc(_)
                | ChainError::InvalidAddress(_)
                | ChainError::Signing(_)
                | ChainError::Unsupported(_)
                | ChainError::InvalidInput(..)
        ) {
            assert!(error.source().expect("a cause").is::<io::Error>());
        }
    }
}

#[test]
fn every_core_error_maps_to_its_code() {
    for (error, expected) in core_errors() {
        assert_eq!(error.code(), expected, "{error}");
    }
}

#[test]
fn a_chain_error_keeps_its_code_when_it_becomes_a_core_error() {
    for (error, expected) in chain_errors() {
        let wrapped = CoreError::from(error);
        assert_eq!(wrapped.code(), expected, "{wrapped}");
    }
}

#[test]
fn every_error_code_is_reachable_from_some_variant() {
    let mut reached: BTreeSet<ErrorCode> =
        chain_errors().into_iter().map(|(_, code)| code).collect();
    reached.extend(core_errors().into_iter().map(|(_, code)| code));
    for code in ErrorCode::ALL {
        assert!(reached.contains(&code), "no error produces {}", code.as_str());
    }
}

#[test]
fn insufficient_funds_renders_base_units_and_leaves_formatting_to_the_interface() {
    let error = ChainError::InsufficientFunds {
        asset: AssetId::Token(TokenId::UsdtErc20),
        needed: Amount::from_base_units(2),
        available: Amount::from_base_units(1),
    };
    assert_eq!(error.to_string(), "insufficient funds: need 2, have 1");
}

#[test]
fn a_rejection_renders_its_kind_and_the_nodes_own_words() {
    let rejection = Rejection::new(
        RejectionKind::AlreadyKnown,
        RetryPolicy::AlreadyLanded,
        "txn-already-in-mempool",
    );
    assert_eq!(rejection.to_string(), "rejected (already_known): txn-already-in-mempool");
}

#[test]
fn errors_that_name_a_chain_name_it_the_way_a_person_reads_it() {
    let down = ChainError::endpoints_down(ChainId::Bsc, "balance", Vec::new());
    assert_eq!(down.to_string(), "no BNB Smart Chain endpoint answered for balance");
    assert_eq!(CoreError::NoAdapter(ChainId::Monero).to_string(), "no adapter for Monero");
    let too_new = ChainError::protocol_too_new(ChainId::Litecoin, "1.9");
    assert!(too_new.to_string().starts_with("Litecoin speaks protocol v1.9"), "{too_new}");
}

#[test]
fn a_storage_error_keeps_its_cause_in_its_source_and_out_of_its_message() {
    let error = CoreError::storage("open", io::Error::other("disk on fire"));
    let source = error.source().expect("a cause");
    assert_eq!(source.to_string(), "disk on fire");
    assert_eq!(error.to_string(), "storage: open", "the message does not repeat the cause");
}

#[test]
fn an_error_without_a_cause_has_no_source() {
    let error = CoreError::internal_without_source("nothing to say");
    assert!(error.source().is_none());
}
