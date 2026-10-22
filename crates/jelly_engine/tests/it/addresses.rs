use std::fs;
use std::sync::atomic::Ordering;

use jelly_core::{ChainId, CoreError, DerivationPath, KeySource, ScriptKind, SecretText};
use jelly_engine::{AddressRecord, test_util};

use crate::common::{Fallible, OTHER_PASSWORD, PASSWORD, harness, secret};

fn path_of(record: &AddressRecord) -> Fallible<String> {
    let KeySource::Derived { ref path } = record.source else {
        return Err("the address is not a derived one".into());
    };
    Ok(path.as_str().to_owned())
}

#[tokio::test]
async fn addresses_take_the_next_index_and_keep_their_script() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("indexed").await.unwrap();

    let first = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    let second = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    assert_ne!(first.address, second.address);
    assert_eq!(first.derivation_index, Some(0));
    assert_eq!(second.derivation_index, Some(1));
    assert_eq!(first.script_kind, Some(ScriptKind::P2wpkh));
    assert!(first.address.starts_with("bc1q"), "{}", first.address);
    assert_eq!(path_of(&first).unwrap(), "m/84'/0'/0'/0/0");

    let litecoin = harness.engine.add_address(ChainId::Litecoin, None, None).await.unwrap();
    assert_eq!(litecoin.script_kind, Some(ScriptKind::P2wpkh));
    assert!(litecoin.address.starts_with("ltc1"), "{}", litecoin.address);
    assert_eq!(path_of(&litecoin).unwrap(), "m/84'/2'/0'/0/0");
}

/// Neither chain has segwit, so each derives P2PKH under BIP-44, the paths Electron
/// Cash and Dogecoin Core restore from the same phrase, and refuses a segwit flavour.
#[tokio::test]
async fn the_chains_without_segwit_derive_bip44_pay_to_public_key_hash() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("legacy").await.unwrap();

    for (chain, path, leading) in [
        (ChainId::BitcoinCash, "m/44'/145'/0'/0/0", "bitcoincash:q"),
        (ChainId::Dogecoin, "m/44'/3'/0'/0/0", "D"),
    ] {
        let added = harness.engine.add_address(chain, None, None).await.unwrap();
        assert_eq!(added.script_kind, Some(ScriptKind::P2pkh), "{chain:?}");
        assert_eq!(path_of(&added).unwrap(), path, "{chain:?}");
        assert!(added.address.starts_with(leading), "{}", added.address);

        let refused = harness.engine.add_address(chain, Some(ScriptKind::P2wpkh), None).await;
        assert!(
            matches!(refused, Err(CoreError::InvalidInput { field: "key", .. })),
            "{chain:?} has no segwit family: {refused:?}"
        );
    }
}

/// Each script flavour counts its own indices under its own purpose. An address derived
/// under another flavour's purpose would be invisible to other wallets restoring the
/// same phrase.
#[tokio::test]
async fn a_script_flavour_allocates_from_its_own_family() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("flavoured").await.unwrap();
    harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();

    let wanted = [
        (ScriptKind::P2pkh, "m/44'/0'/0'/0/0", "1"),
        (ScriptKind::P2shP2wpkh, "m/49'/0'/0'/0/0", "3"),
        (ScriptKind::P2tr, "m/86'/0'/0'/0/0", "bc1p"),
    ];
    for (script, path, leading) in wanted {
        let added = harness.engine.add_address(ChainId::Bitcoin, Some(script), None).await.unwrap();
        assert_eq!(added.script_kind, Some(script));
        assert_eq!(path_of(&added).unwrap(), path, "{script:?}");
        assert!(added.address.starts_with(leading), "{}", added.address);
        assert_eq!(added.derivation_index, Some(0), "{script:?} starts its own count");
    }

    // The default native segwit family kept counting on its own the whole time.
    let native =
        harness.engine.add_address(ChainId::Bitcoin, Some(ScriptKind::P2wpkh), None).await.unwrap();
    assert_eq!(path_of(&native).unwrap(), "m/84'/0'/0'/0/1");
}

#[tokio::test]
async fn a_flavour_the_chain_does_not_have_is_refused() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("flavoured").await.unwrap();

    let refused = harness.engine.add_address(ChainId::Litecoin, Some(ScriptKind::P2tr), None).await;
    assert!(
        matches!(refused, Err(CoreError::InvalidInput { field: "key", .. })),
        "litecoin has no taproot family: {refused:?}"
    );
    let refused =
        harness.engine.add_address(ChainId::Ethereum, Some(ScriptKind::P2wpkh), None).await;
    assert!(
        matches!(refused, Err(CoreError::InvalidInput { field: "key", .. })),
        "an account chain has no script flavours: {refused:?}"
    );
    let refused = harness.engine.add_address(ChainId::Monero, Some(ScriptKind::P2tr), None).await;
    assert!(
        matches!(refused, Err(CoreError::InvalidInput { field: "key", .. })),
        "neither does monero: {refused:?}"
    );

    // Each chain accepts the same request without a flavour.
    harness.engine.add_address(ChainId::Litecoin, None, None).await.unwrap();
    harness.engine.add_address(ChainId::Ethereum, None, None).await.unwrap();
    harness.engine.add_address(ChainId::Monero, None, None).await.unwrap();
}

/// Solana's family varies its third component, `m/44'/501'/i'/0'`, so the last
/// component is the same constant for every address.
#[tokio::test]
async fn each_solana_address_reports_the_index_its_family_varies() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("solana").await.unwrap();
    let first = harness.engine.add_address(ChainId::Solana, None, None).await.unwrap();
    let second = harness.engine.add_address(ChainId::Solana, None, None).await.unwrap();
    assert_eq!(path_of(&second).unwrap(), "m/44'/501'/1'/0'");
    assert_eq!((first.derivation_index, second.derivation_index), (Some(0), Some(1)));
}

#[tokio::test]
async fn a_named_path_is_derived_as_asked() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("named").await.unwrap();
    let path = DerivationPath::new("m/84'/0'/0'/0/7");
    let added = harness.engine.add_address_at(ChainId::Bitcoin, &path, None, None).await.unwrap();
    assert_eq!(added.source, KeySource::Derived { path });
    assert_eq!(added.script_kind, Some(ScriptKind::P2wpkh));
    assert_eq!(added.derivation_index, Some(7));
}

#[tokio::test]
async fn a_removed_address_is_the_next_one_handed_out_again() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("removed").await.unwrap();
    let mut added = Vec::new();
    for _ in 0..3 {
        added.push(harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap());
    }
    let removed = added.get(1).expect("three addresses").clone();
    harness.engine.delete_address(removed.id).await.unwrap();
    assert_eq!(harness.engine.list_addresses(Some(ChainId::Bitcoin)).await.unwrap().len(), 2);

    let again = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    assert_eq!(again.address, removed.address);
    assert_eq!(again.derivation_index, Some(1));
    assert_ne!(again.id, removed.id, "a re-derived address is a new row");

    harness.engine.delete_address(again.id).await.unwrap();
    let path = DerivationPath::new(path_of(&removed).unwrap());
    let at_path = harness.engine.add_address_at(ChainId::Bitcoin, &path, None, None).await.unwrap();
    assert_eq!(at_path.address, removed.address);
}

#[tokio::test]
async fn adding_an_address_twice_is_a_conflict() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("dupes").await.unwrap();
    let path = DerivationPath::new("m/86'/0'/0'/0/3");
    harness.engine.add_address_at(ChainId::Bitcoin, &path, None, None).await.unwrap();
    let again = harness.engine.add_address_at(ChainId::Bitcoin, &path, None, None).await;
    assert!(matches!(again, Err(CoreError::AlreadyExists { .. })), "{again:?}");
}

#[tokio::test]
async fn monero_hands_out_the_account_address_then_subaddresses() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("monero").await.unwrap();

    let primary = harness.engine.add_address(ChainId::Monero, None, None).await.unwrap();
    assert_eq!(primary.source, KeySource::Subaddress { major: 0, minor: 0 });
    assert!(primary.address.starts_with('4'), "{}", primary.address);

    let second = harness
        .engine
        .add_address(ChainId::Monero, None, Some(String::from("tips")))
        .await
        .unwrap();
    assert_eq!(second.source, KeySource::Subaddress { major: 0, minor: 1 });
    assert!(second.address.starts_with('8'), "{}", second.address);
    assert_eq!(second.label.as_deref(), Some("tips"));

    let third = harness.engine.add_address(ChainId::Monero, None, None).await.unwrap();
    assert_eq!(third.source, KeySource::Subaddress { major: 0, minor: 2 });

    let refused = harness
        .engine
        .add_address_at(ChainId::Monero, &DerivationPath::new("m/44'/128'/0'"), None, None)
        .await;
    assert!(matches!(refused, Err(CoreError::Unsupported(_))), "{refused:?}");
}

#[tokio::test]
async fn a_new_subaddress_widens_the_scanner_window() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[ChainId::Monero]);
    harness.seed_wallet("scanning").await.unwrap();

    harness.engine.add_address(ChainId::Monero, None, None).await.unwrap();
    harness.engine.add_address(ChainId::Monero, None, None).await.unwrap();
    let state = harness.builder.state(ChainId::Monero);
    assert_eq!(state.windows.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_removed_subaddress_is_the_next_one_handed_out_again() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("monero").await.unwrap();
    let mut added = Vec::new();
    for _ in 0..3 {
        added.push(harness.engine.add_address(ChainId::Monero, None, None).await.unwrap());
    }
    let removed = added.get(1).expect("three subaddresses").clone();
    harness.engine.delete_address(removed.id).await.unwrap();

    let again = harness.engine.add_address(ChainId::Monero, None, None).await.unwrap();
    assert_eq!(again.source, KeySource::Subaddress { major: 0, minor: 1 });
    assert_eq!(again.address, removed.address);
    let next = harness.engine.add_address(ChainId::Monero, None, None).await.unwrap();
    assert_eq!(next.source, KeySource::Subaddress { major: 0, minor: 3 });
}

#[tokio::test]
async fn a_standalone_key_round_trips_and_can_be_deleted() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("keys").await.unwrap();

    let added = harness
        .engine
        .create_imported_address(ChainId::Bitcoin, Some(ScriptKind::P2wpkh), None)
        .await
        .unwrap();
    let id = added.id;
    assert!(matches!(added.source, KeySource::Imported { .. }));
    assert!(added.address.starts_with("bc1q"), "{}", added.address);

    let exported = harness.engine.export_private_key(id, secret(PASSWORD)).await.unwrap();
    let reimport = async || {
        harness
            .engine
            .import_private_key(
                ChainId::Bitcoin,
                SecretText::new(exported.as_str().to_owned()),
                None,
                Some(ScriptKind::P2wpkh),
                None,
            )
            .await
    };
    let refused = reimport().await;
    assert!(matches!(refused, Err(CoreError::AlreadyExists { .. })), "{refused:?}");

    let refused = harness.engine.delete_key(id, secret(PASSWORD), "please").await;
    assert!(matches!(refused, Err(CoreError::ConfirmationRequired { .. })), "{refused:?}");
    let refused = harness.engine.delete_key(id, secret(OTHER_PASSWORD), "DELETE").await;
    assert!(matches!(refused, Err(CoreError::WrongPassword)), "{refused:?}");

    harness.engine.delete_key(id, secret(PASSWORD), "DELETE").await.unwrap();
    assert!(harness.engine.list_addresses(None).await.unwrap().is_empty());

    let restored = reimport().await.unwrap();
    assert_eq!(restored.address, added.address);
}

#[tokio::test]
async fn a_failed_address_delete_preserves_the_standalone_key() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("keys").await.unwrap();
    let added =
        harness.engine.create_imported_address(ChainId::Ethereum, None, None).await.unwrap();
    harness.clock.set_broken(true);
    let refused = harness.engine.delete_key(added.id, secret(PASSWORD), "DELETE").await;
    assert!(refused.is_err(), "the database could not read the clock for the delete");
    let addresses = harness.engine.list_addresses(None).await.unwrap();
    assert_eq!(addresses, vec![added.clone()]);
    harness.engine.export_private_key(added.id, secret(PASSWORD)).await.unwrap();

    harness.clock.set_broken(false);
    harness.engine.delete_key(added.id, secret(PASSWORD), "DELETE").await.unwrap();
    assert!(harness.engine.list_addresses(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_vault_write_still_removes_the_standalone_address() {
    let harness = harness().await.unwrap();
    let wallet = harness.seed_wallet("keys").await.unwrap().wallet;
    let added =
        harness.engine.create_imported_address(ChainId::Ethereum, None, None).await.unwrap();
    let path = test_util::vault_path(harness.scratch.path(), wallet.id);
    let backup = path.with_extension("backup");
    fs::rename(&path, &backup).unwrap();
    fs::create_dir(&path).unwrap();

    let refused = harness.engine.delete_key(added.id, secret(PASSWORD), "DELETE").await;
    assert!(refused.is_err(), "the vault could not be rewritten");
    assert!(harness.engine.list_addresses(None).await.unwrap().is_empty());

    fs::remove_dir(&path).unwrap();
    fs::rename(&backup, &path).unwrap();
    harness.engine.lock_wallet().await;
    harness.engine.unlock_wallet(wallet.id, secret(PASSWORD), None).await.unwrap();
    assert!(harness.engine.list_addresses(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_re_import_is_refused_without_touching_the_vault() {
    let harness = harness().await.unwrap();
    let wallet = harness.seed_wallet("keys").await.unwrap().wallet;
    let added = harness.engine.create_imported_address(ChainId::Bitcoin, None, None).await.unwrap();
    let exported = harness.engine.export_private_key(added.id, secret(PASSWORD)).await.unwrap();
    let path = test_util::vault_path(harness.scratch.path(), wallet.id);
    let before = fs::read(&path).unwrap();

    let refused = harness
        .engine
        .import_private_key(
            ChainId::Bitcoin,
            SecretText::new(exported.as_str().to_owned()),
            None,
            None,
            None,
        )
        .await;
    assert!(matches!(refused, Err(CoreError::AlreadyExists { .. })), "{refused:?}");
    assert_eq!(fs::read(&path).unwrap(), before, "a refused import rewrote the vault");
}

#[tokio::test]
async fn a_standalone_key_is_only_removed_with_its_key() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("keys").await.unwrap();
    let added =
        harness.engine.create_imported_address(ChainId::Litecoin, None, None).await.unwrap();

    let refused = harness.engine.delete_address(added.id).await;
    assert!(matches!(refused, Err(CoreError::Unsupported(_))), "{refused:?}");
    assert_eq!(harness.engine.list_addresses(None).await.unwrap(), vec![added]);
}

#[tokio::test]
async fn only_a_standalone_key_can_be_deleted() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("derived").await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    let refused = harness.engine.delete_key(added.id, secret(PASSWORD), "DELETE").await;
    assert!(matches!(refused, Err(CoreError::Unsupported(_))), "{refused:?}");
}

#[tokio::test]
async fn a_row_whose_address_is_not_its_key_is_refused() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("tampered").await.unwrap();
    let id = test_util::insert_underived_address(
        &harness.engine,
        created.wallet.id,
        ChainId::Bitcoin,
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
        DerivationPath::new("m/86'/0'/0'/0/0"),
        ScriptKind::P2tr,
    )
    .await
    .unwrap();

    let refused = harness.engine.list_addresses(Some(ChainId::Bitcoin)).await;
    assert!(
        matches!(refused, Err(CoreError::AddressMismatch { address_id }) if address_id == id),
        "{refused:?}"
    );
}

#[tokio::test]
async fn an_address_of_another_wallet_is_not_found() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("first").await.unwrap();
    let theirs = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    harness.engine.lock_wallet().await;
    harness.seed_wallet("second").await.unwrap();

    let refused = harness.engine.get_xpub_for_address(theirs.id).await;
    assert!(matches!(refused, Err(CoreError::NotFound { .. })), "{refused:?}");
}

#[tokio::test]
async fn a_derived_address_reports_the_xpub_of_its_account() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("keys").await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();

    let xpub = harness.engine.get_xpub(&DerivationPath::new("m/84'/0'/0'")).await.unwrap();
    assert!(xpub.starts_with("xpub"), "{xpub}");
    let for_address = harness.engine.get_xpub_for_address(added.id).await.unwrap();
    assert_eq!(for_address.as_deref(), Some(xpub.as_str()));
}

#[tokio::test]
async fn a_standalone_address_has_no_xpub() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("keys").await.unwrap();
    let added = harness.engine.create_imported_address(ChainId::Bitcoin, None, None).await.unwrap();
    assert_eq!(harness.engine.get_xpub_for_address(added.id).await.unwrap(), None);
}

#[tokio::test]
async fn labels_are_set_and_cleared() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("labelled").await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    let id = added.id;

    let labelled =
        harness.engine.set_address_label(id, Some(String::from("savings"))).await.unwrap();
    assert_eq!(labelled.label.as_deref(), Some("savings"));
    let cleared = harness.engine.set_address_label(id, None).await.unwrap();
    assert_eq!(cleared.label, None);
}
