use std::sync::atomic::Ordering;

use jelly_core::{
    ChainId, CoreError, DerivationPath, KeySource, ScriptKind, SecretText, WalletColor,
};
use jelly_engine::{PassphraseInput, test_util};

use crate::common::{Fallible, Harness, PASSWORD, harness, secret};

/// wallet-core's TWTheOpenNetwork.Address phrase, and the V4R2 wallet Trust Wallet shows
/// for it at TON's default path.
const TRUST_PHRASE: &str =
    "stuff diamond cycle federal scan spread pigeon people engage teach snack grain";
const TRUST_V4R2: &str = "UQDYW_1eScJVxtitoBRksvoV9cCYo4uKGWLVNIHB1JqRRyQx";

/// tonlib-core's `test_ton_wallet_create` mnemonic and its V4R2 wallet, written
/// non-bounceable as Jelly writes a wallet's own address.
const TON_SEED: &str = "fancy carpet hello mandate penalty trial consider property top vicious exit rebuild tragic profit urban major total month holiday sudden rib gather media vicious";
const TON_SEED_V4R2: &str = "UQCDM_QGggZ3qMa_f3lRPk4_qLDnLTqdi6OkMAV2NB9r5Wxy";

async fn ton_wallet(harness: &Harness) -> Fallible<()> {
    harness.builder.install(&[ChainId::Ton]);
    harness
        .engine
        .import_seed_wallet(
            "ton",
            WalletColor::default(),
            secret(PASSWORD),
            SecretText::new(TRUST_PHRASE.to_owned()),
            PassphraseInput::None,
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_ton_address_brings_in_the_wallet_versions_its_key_was_used_under() {
    let harness = harness().await.unwrap();
    ton_wallet(&harness).await.unwrap();
    let state = harness.builder.state(ChainId::Ton);
    state.used_addresses.lock().push(TRUST_V4R2.to_owned());

    let added = harness.engine.add_address(ChainId::Ton, None, None).await.unwrap();
    assert_eq!(added.script_kind, Some(ScriptKind::TonV5r1));
    let rows = harness.engine.list_addresses(Some(ChainId::Ton)).await.unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let found = rows.iter().find(|row| row.address == TRUST_V4R2).unwrap();
    assert_eq!(found.script_kind, Some(ScriptKind::TonV4r2));
    assert_eq!(found.source, added.source, "the same key at the same path");
    assert_eq!(found.derivation_index, Some(0));
    {
        let probes = state.probes.lock();
        assert_eq!(probes.len(), 1, "the key is checked once");
        assert_eq!(probes[0].len(), 2, "for the two versions the wallet does not hold yet");
        assert!(probes[0].iter().any(|address| address == TRUST_V4R2));
    }

    let next = harness.engine.add_address(ChainId::Ton, None, None).await.unwrap();
    assert_eq!(next.derivation_index, Some(1), "each version counts its own indices");
    assert_eq!(state.probes.lock().len(), 2, "the next key is checked once as well");
    assert_eq!(harness.engine.list_addresses(Some(ChainId::Ton)).await.unwrap().len(), 3);
}

#[tokio::test]
async fn a_version_added_by_hand_takes_its_own_first_index_and_a_present_one_is_not_asked_about() {
    let harness = harness().await.unwrap();
    ton_wallet(&harness).await.unwrap();
    let state = harness.builder.state(ChainId::Ton);
    harness.engine.add_address(ChainId::Ton, None, None).await.unwrap();
    let by_hand =
        harness.engine.add_address(ChainId::Ton, Some(ScriptKind::TonV4r2), None).await.unwrap();
    assert_eq!((by_hand.address.as_str(), by_hand.derivation_index), (TRUST_V4R2, Some(0)));
    let probes = state.probes.lock();
    assert_eq!(probes.len(), 2);
    assert_eq!(probes[1].len(), 1, "only the V3R2 wallet is still missing");
}

#[tokio::test]
async fn a_ledger_path_holds_a_v4r2_wallet_added_by_hand() {
    let harness = harness().await.unwrap();
    ton_wallet(&harness).await.unwrap();
    let path = DerivationPath::new("m/44'/607'/0'/0'/0'/0'");
    let added = harness
        .engine
        .add_address_at(ChainId::Ton, &path, Some(ScriptKind::TonV4r2), None)
        .await
        .unwrap();
    assert_eq!(added.script_kind, Some(ScriptKind::TonV4r2));
    assert_eq!(added.source, KeySource::Derived { path });
    let path = DerivationPath::new("m/44'/607'/0'/0'/1'/0'");
    let default = harness.engine.add_address_at(ChainId::Ton, &path, None, None).await.unwrap();
    assert_eq!(default.script_kind, Some(ScriptKind::TonV5r1), "no path names a version");
}

#[tokio::test]
async fn a_failed_probe_still_adds_the_address() {
    let harness = harness().await.unwrap();
    ton_wallet(&harness).await.unwrap();
    let state = harness.builder.state(ChainId::Ton);
    state.used_addresses.lock().push(TRUST_V4R2.to_owned());
    state.should_fail_probe.store(true, Ordering::SeqCst);
    let added = harness.engine.add_address(ChainId::Ton, None, None).await.unwrap();
    assert_eq!(harness.engine.list_addresses(Some(ChainId::Ton)).await.unwrap(), vec![added]);
}

#[tokio::test]
async fn a_chain_without_an_adapter_adds_only_the_address_asked_for() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("offline").await.unwrap();
    let added = harness.engine.add_address(ChainId::Ton, None, None).await.unwrap();
    assert_eq!(harness.engine.list_addresses(Some(ChainId::Ton)).await.unwrap(), vec![added]);
}

#[tokio::test]
async fn an_imported_ton_seed_brings_in_its_used_versions_and_its_key_goes_with_all_of_them() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("keys").await.unwrap();
    harness.builder.install(&[ChainId::Ton]);
    test_util::rebuild_adapters(&harness.engine).await.unwrap();
    let state = harness.builder.state(ChainId::Ton);
    state.used_addresses.lock().push(TON_SEED_V4R2.to_owned());

    let import = async || {
        harness
            .engine
            .import_private_key(
                ChainId::Ton,
                SecretText::new(TON_SEED.to_owned()),
                None,
                None,
                None,
            )
            .await
            .unwrap()
    };
    let imported = import().await;
    let rows = harness.engine.list_addresses(Some(ChainId::Ton)).await.unwrap();
    let [first, second] = rows.as_slice() else {
        panic!("expected the seed's two used versions, got {rows:?}");
    };
    assert_eq!(first, &imported);
    assert_eq!(
        (second.address.as_str(), second.script_kind),
        (TON_SEED_V4R2, Some(ScriptKind::TonV4r2))
    );
    assert_eq!(second.source, imported.source, "both addresses hold the one standalone key");

    harness.engine.delete_key(imported.id, secret(PASSWORD), "DELETE").await.unwrap();
    assert!(harness.engine.list_addresses(Some(ChainId::Ton)).await.unwrap().is_empty());

    let imported = import().await;
    let sibling = harness
        .engine
        .list_addresses(Some(ChainId::Ton))
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.id != imported.id)
        .unwrap();
    harness.engine.delete_address(sibling.id).await.unwrap();
    let refused = harness.engine.delete_address(imported.id).await;
    assert!(
        matches!(refused, Err(CoreError::Unsupported(_))),
        "the key's last address goes only with the key: {refused:?}"
    );
}
