use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::Arc;

use jelly_core::{
    Amount, AssetId, ChainError, ChainId, CoreError, DerivationPath, EndpointAttempt,
    EndpointHealth, EndpointHealthRecord, EndpointLabel, ErrorCode, FailureKind, FeeSelection,
    PendingTxId, PhraseWords, Rejection, RejectionKind, RetryPolicy, SystemClock, TxId,
    WalletColor,
};
use jelly_engine::{
    EngineDeps, PassphraseInput, StartupError, TxQuery, WalletEngine, test_util::NullAdapterBuilder,
};
use jelly_lib::{IpcError, IpcErrorCode, IpcErrorDetail, ShellErrorCode};
use jelly_vault::test_util::MemoryKeyStore;
use tokio::task::spawn_blocking;

use crate::common::{Fallible, PASSWORD, offline_config, secret};

async fn engine_on(directory: &Path) -> Fallible<WalletEngine> {
    let deps = EngineDeps {
        data_dir: directory.to_path_buf(),
        clock: Arc::new(SystemClock),
        keystore: Arc::new(MemoryKeyStore::new()),
        builder: Arc::new(NullAdapterBuilder),
        table: jelly_chains::chains(),
    };
    let started = WalletEngine::start(offline_config().engine, deps)
        .await
        .map_err(|failure| failure.error)?;
    Ok(started.engine)
}

#[test]
fn the_interface_code_is_the_domain_code_plus_the_three_shell_states() {
    assert_eq!(ShellErrorCode::COUNT, 3);
    for code in ErrorCode::ALL {
        let widened = IpcErrorCode::from(code);
        assert_eq!(
            serde_json::to_value(widened).unwrap(),
            serde_json::to_value(code).unwrap(),
            "the interface's spelling of {code:?} differs from the domain's"
        );
    }
    let shell_codes: Vec<String> = ShellErrorCode::ALL
        .into_iter()
        .filter(|code| ErrorCode::parse(code.as_str()).is_none())
        .map(|code| {
            let widened = IpcErrorCode::Shell(code);
            assert_eq!(serde_json::to_value(widened).unwrap(), code.as_str());
            widened.to_string()
        })
        .collect();
    assert_eq!(
        shell_codes,
        ["starting", "prices_unavailable", "panic"],
        "only these three are the shell's own"
    );
}

#[tokio::test]
async fn every_error_code_has_a_producer() {
    let directory = tempfile::tempdir().unwrap();
    let engine = engine_on(directory.path()).await.unwrap();
    let locked = engine.list_addresses(None).await.expect_err("no wallet is open");
    let mut seen_codes = HashSet::from([IpcError::starting().code, IpcError::from(locked).code]);

    let created = engine
        .create_seed_wallet(
            "first",
            WalletColor::default(),
            secret(PASSWORD),
            PhraseWords::Twelve,
            PassphraseInput::None,
        )
        .await
        .unwrap();

    let path = DerivationPath::new("m/84'/0'/0'/0/0");
    engine.add_address_at(ChainId::Bitcoin, &path, None, None).await.unwrap();

    let refusals: Vec<CoreError> = vec![
        engine.get_xpub_for_address(4242).await.expect_err("no such address"),
        engine
            .add_address_at(ChainId::Bitcoin, &path, None, None)
            .await
            .expect_err("the path is taken"),
        engine
            .add_address_at(ChainId::Monero, &path, None, None)
            .await
            .expect_err("monero has no derivation paths"),
        engine
            .list_txs(TxQuery {
                cursor: Some(String::from("not a page token")),
                ..TxQuery::default()
            })
            .await
            .expect_err("the page token was not issued by this build"),
        engine
            .bump_fee(&TxId::new(ChainId::Bitcoin, "deadbeef"), &FeeSelection::NORMAL)
            .await
            .expect_err("no such transaction"),
        engine.sync_now(Some(ChainId::Bitcoin)).await.expect_err("no adapter is built"),
        engine
            .delete_pending_tx(PendingTxId::from_bytes([1u8; 16]))
            .await
            .expect_err("no such pending transaction"),
        engine
            .delete_wallet(created.wallet.id, secret(PASSWORD), "not the name")
            .await
            .expect_err("the confirmation is wrong"),
        engine
            .export_seed_phrase(secret("a completely different password"))
            .await
            .expect_err("the password is wrong"),
        engine
            .put_endpoint_health(vec![EndpointHealthRecord {
                chain: ChainId::Bitcoin,
                url: String::from("ssl://node.example:50002"),
                health: EndpointHealth { score_permille: 1_001, ..EndpointHealth::default() },
            }])
            .await
            .expect_err("the database refuses a score above a thousand"),
    ];
    let mapped_errors = [
        CoreError::WrongPassphrase,
        CoreError::PassphraseRequired,
        CoreError::RebuildRequired,
        CoreError::AddressMismatch { address_id: 1 },
        CoreError::keychain(
            "reading the master key",
            io::Error::other("the credential store is unavailable"),
        ),
        CoreError::internal_without_source("something the shell could not classify"),
        CoreError::Chain(ChainError::invalid_address("not an address on this chain")),
        CoreError::Chain(ChainError::rpc("the node answered with an error")),
        CoreError::Chain(ChainError::signing("the signer refused")),
        CoreError::Chain(ChainError::InsufficientFunds {
            asset: AssetId::Native(ChainId::Bitcoin),
            needed: Amount::from_base_units(2),
            available: Amount::from_base_units(1),
        }),
        CoreError::Chain(ChainError::endpoints_down(ChainId::Bitcoin, "sync", Vec::new())),
        CoreError::Chain(ChainError::Rejected(Rejection::new(
            RejectionKind::BelowRelayFee,
            RetryPolicy::DidNotLand,
            "min relay fee not met",
        ))),
        CoreError::Chain(ChainError::protocol_too_new(ChainId::Bitcoin, "1.9")),
    ];
    let mapped = refusals.into_iter().chain(mapped_errors).map(IpcError::from);
    seen_codes.extend(mapped.chain([IpcError::prices_unavailable()]).map(|error| error.code));

    let panicked: Result<(), _> = spawn_blocking(|| panic!("a command's work failed")).await;
    seen_codes.insert(IpcError::from(panicked.expect_err("the task panicked")).code);

    let missing_codes: Vec<IpcErrorCode> = ErrorCode::ALL
        .into_iter()
        .map(IpcErrorCode::Domain)
        .chain(ShellErrorCode::ALL.into_iter().map(IpcErrorCode::Shell))
        .filter(|code| !seen_codes.contains(code))
        .collect();
    assert!(missing_codes.is_empty(), "nothing can produce {missing_codes:?}");
}

#[test]
fn a_failure_with_typed_facts_carries_them_in_its_detail() {
    let funds = IpcError::from(ChainError::InsufficientFunds {
        asset: AssetId::Native(ChainId::Bitcoin),
        needed: Amount::from_base_units(2),
        available: Amount::from_base_units(1),
    });
    assert_eq!(
        funds.detail,
        Some(IpcErrorDetail::InsufficientFunds {
            asset: AssetId::Native(ChainId::Bitcoin),
            needed: Amount::from_base_units(2),
            available: Amount::from_base_units(1),
        })
    );

    let down = IpcError::from(ChainError::endpoints_down(
        ChainId::Bitcoin,
        "sync",
        vec![EndpointAttempt::new(
            EndpointLabel::new("https", "node.example", Some(443)),
            FailureKind::Timeout,
            "the endpoint did not answer",
        )],
    ));
    let Some(IpcErrorDetail::EndpointsDown { chain, attempts }) = down.detail else {
        panic!("an endpoints-down failure carries its attempts");
    };
    assert_eq!(chain, ChainId::Bitcoin);
    assert_eq!(attempts.len(), 1);
    let attempt = attempts.first().expect("one attempt");
    assert_eq!(attempt.endpoint.host(), "node.example");
    assert_eq!(attempt.kind, FailureKind::Timeout);

    let rejected = IpcError::from(ChainError::Rejected(Rejection::new(
        RejectionKind::BelowRelayFee,
        RetryPolicy::DidNotLand,
        "min relay fee not met",
    )));
    assert_eq!(
        rejected.detail,
        Some(IpcErrorDetail::Rejected {
            rejection: RejectionKind::BelowRelayFee,
            retry: RetryPolicy::DidNotLand,
        })
    );

    let schema = IpcError::from(StartupError::SchemaTooNew { found: 9, supported: 1 });
    assert_eq!(schema.code, ErrorCode::Storage.into());
    assert_eq!(schema.detail, Some(IpcErrorDetail::SchemaTooNew { found: 9, supported: 1 }));

    let protocol = IpcError::from(ChainError::protocol_too_new(ChainId::Litecoin, "1.9"));
    assert_eq!(
        protocol.detail,
        Some(IpcErrorDetail::ProtocolTooNew {
            chain: ChainId::Litecoin,
            version: String::from("1.9"),
        })
    );

    let confirmation =
        IpcError::from(CoreError::ConfirmationRequired { expected: String::from("first") });
    assert_eq!(confirmation.detail, None);

    let adapter = IpcError::from(CoreError::NoAdapter(ChainId::Solana));
    assert_eq!(adapter.detail, Some(IpcErrorDetail::NoAdapter { chain: ChainId::Solana }));

    let invalid = IpcError::from(CoreError::invalid_input("cursor", "that page token is not ours"));
    assert_eq!(invalid.detail, None);
    assert!(invalid.message.contains("that page token is not ours"));
}

/// The frontend uses rejection kinds, not the node's diagnostic wording.
#[test]
fn a_nodes_own_words_reach_neither_the_detail_nor_the_message() {
    let error = IpcError::from(ChainError::Rejected(Rejection::new(
        RejectionKind::DoubleSpend,
        RetryPolicy::FinalEverywhere,
        "txn-mempool-conflict: 0000",
    )));
    let rendered = format!("{:?}", error.detail);
    assert!(!rendered.contains("txn-mempool-conflict"), "the wording reached the detail");
    assert!(!error.message.contains("txn-mempool-conflict"), "the wording reached the message");
    assert_eq!(error.message, "Jelly could not complete the request.");
}

#[test]
fn a_storage_failure_never_names_a_file() {
    let source = io::Error::other("C:\\Users\\me\\AppData\\Roaming\\jelly\\jelly.sqlite");
    let error = IpcError::from(CoreError::storage("the wallet database", source));
    assert_eq!(error.code, ErrorCode::Storage.into());
    for fragment in [".sqlite", ":\\", "/"] {
        assert!(!error.message.contains(fragment), "{fragment} reached the message");
    }
}
