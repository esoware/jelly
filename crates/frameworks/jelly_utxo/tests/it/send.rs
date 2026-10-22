use std::slice::from_ref;

use bitcoin::absolute::LockTime;
use bitcoin::consensus::serialize;
use bitcoin::transaction::Version;
use bitcoin::{OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness};
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, FeeSelection, ScriptKind, TxWarningKind,
    WalletEvent,
};
use jelly_electrum::{Method, test_util::Answer};
use jelly_utxo::{
    UtxoParams,
    test_util::{test_key, tx_paying, tx_spending},
};

use crate::common::{
    BITCOIN, BITCOIN_CASH, BLOCK_UNIX_SECONDS, CHAINS, CHANGE_VOUT, DOGECOIN, Harness, LITECOIN,
    Ledger, MINED_AT, RECIPIENT_VOUT, Setup, TIP, Wallet, coinbase_paying, drain, kind_of, payload,
    quoted, sats, synced, watched,
};

const FUNDING_SAT: u64 = 500_000;
const PAYMENT_SAT: u64 = 100_000;

#[tokio::test]
async fn every_spendable_script_kind_builds_a_transaction_with_change() {
    for params in CHAINS {
        for &kind in params.script_kinds {
            let (harness, wallet) = synced(Setup::on(&params), kind, &[FUNDING_SAT]).await.unwrap();
            let unsigned = harness
                .adapter
                .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "2"))
                .await
                .unwrap();

            assert_eq!(unsigned.chain(), params.chain);
            assert_eq!(unsigned.preview.amount, sats(PAYMENT_SAT));
            assert_eq!(unsigned.preview.from, [wallet.ours.address()]);
            assert_eq!(unsigned.preview.to, wallet.theirs.address());

            let payload = payload(&unsigned).unwrap();
            assert_eq!(payload.input_kinds.first().copied(), Some(kind));
            assert_eq!(payload.strategy, "lowest_waste");

            let tx = &payload.tx;
            assert_eq!(tx.lock_time.to_consensus_u32(), TIP, "the locktime is the tip a pass saw");
            for txin in &tx.input {
                assert_eq!(
                    txin.sequence,
                    params.replacement.input_sequence(),
                    "{kind:?} takes the chain's sequence"
                );
            }
            let recipient = params.script_pubkey(wallet.theirs.address()).unwrap();
            let own = params.script_pubkey(wallet.ours.address()).unwrap();
            let outputs = &tx.output;
            assert_eq!(
                outputs
                    .get(usize::try_from(RECIPIENT_VOUT).unwrap())
                    .map(|output| output.script_pubkey.clone()),
                Some(recipient)
            );
            assert_eq!(
                outputs
                    .get(usize::try_from(CHANGE_VOUT).unwrap())
                    .map(|output| output.script_pubkey.clone()),
                Some(own),
                "{}: {kind:?} pays its change back to the sending address",
                params.chain
            );
        }
    }
}

#[tokio::test]
async fn a_send_max_drains_the_address_and_leaves_no_change() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[150_000, 90_000]).await.unwrap();
        let everything =
            drain(&params, wallet.theirs.address(), FeeSelection::Rate { rate: "2".to_owned() });

        let unsigned = harness.adapter.build_tx(&everything).await.unwrap();
        let payload = payload(&unsigned).unwrap();
        assert_eq!(payload.strategy, "drain");
        assert_eq!(payload.tx.output.len(), 1, "a drain has nothing to pay back");
        assert_eq!(payload.input_kinds.len(), 2, "everything spendable leaves the address");
        assert_eq!(
            unsigned.preview.amount.checked_add(unsigned.preview.fee),
            Some(sats(wallet.total()))
        );
    }
}

/// Bitcoin's pay-to-witness-public-key-hash dust threshold is 294, and
/// Litecoin's is ten times that, so a thousand base units of leftover is change
/// worth keeping on one chain and dust on the other.
#[tokio::test]
async fn change_below_the_chains_own_dust_threshold_goes_to_the_miner() {
    let p2wpkh = test_key(&BITCOIN, ScriptKind::P2wpkh, 1).unwrap().script_pubkey();
    assert!(BITCOIN.dust_threshold(&p2wpkh) < sats(1_000));
    assert!(LITECOIN.dust_threshold(&p2wpkh) > sats(1_000));
    for (params, leftover) in [(BITCOIN, 100), (LITECOIN, 1_000)] {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[200_000]).await.unwrap();
        let dust = params.dust_threshold(&wallet.ours.script_pubkey());
        assert!(sats(leftover) < dust, "{}", params.chain);

        // A first build shows the change a payment of this shape leaves. The second
        // asks for all of it but the leftover, below the dust threshold a change
        // output would have to clear.
        let probe = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "2"))
            .await
            .unwrap();
        let change = payload(&probe)
            .unwrap()
            .tx
            .output
            .get(usize::try_from(CHANGE_VOUT).unwrap())
            .map(|output| output.value.to_sat())
            .expect("the probe has change");
        assert!(sats(change) > dust);
        harness
            .adapter
            .pending_tracker()
            .expect("the adapter tracks pending sends")
            .discard(&probe)
            .await
            .unwrap();

        let target = PAYMENT_SAT.saturating_add(change).saturating_sub(leftover);
        let unsigned = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), target, "2"))
            .await
            .unwrap();
        let payload = payload(&unsigned).unwrap();
        assert_eq!(payload.tx.output.len(), 1, "no change output survives");
        assert!(!payload.donated.is_zero(), "the leftover went to the miner");
        assert!(
            unsigned
                .preview
                .warnings
                .iter()
                .any(|warning| warning.kind == TxWarningKind::DroppedChange),
            "{:?}",
            unsigned.preview.warnings
        );
    }
}

#[tokio::test]
async fn an_output_below_the_chains_own_dust_threshold_is_refused() {
    for (params, expected, copy) in [
        (
            BITCOIN,
            294,
            "The output of 0.000001 BTC is below the network minimum of 0.00000294 BTC.",
        ),
        (
            LITECOIN,
            2_940,
            "The output of 0.000001 LTC is below the network minimum of 0.0000294 LTC.",
        ),
    ] {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let dust = params.dust_threshold(&wallet.theirs.script_pubkey());
        assert_eq!(dust, sats(expected), "{}", params.chain);
        for value in [100, expected.saturating_sub(1)] {
            let error = harness
                .adapter
                .build_tx(&harness.request(wallet.theirs.address(), value, "2"))
                .await
                .err()
                .ok_or_else(|| format!("{value} is dust on {}", params.chain))
                .unwrap();
            assert!(
                error.to_string().contains("output")
                    && error.to_string().contains("network minimum"),
                "{error}"
            );
            if value == 100 {
                assert_eq!(error.to_string(), copy);
            }
        }
        harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), expected, "2"))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_rate_below_the_relay_floor_is_refused_in_the_chains_own_unit() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        // The fixture server reports a floor of one base unit per virtual byte, so
        // half of one cannot relay anywhere.
        let error = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "0.5"))
            .await
            .expect_err("a rate below the floor is refused");
        assert_eq!(
            error.to_string(),
            format!(
                "The fee rate of 0.5 {unit} is below the network minimum of 1 {unit}.",
                unit = params.fee_rate_unit
            )
        );
    }
}

#[tokio::test]
async fn a_fee_rate_that_does_not_parse_is_refused_in_the_chains_own_unit() {
    for (params, copy) in [
        (BITCOIN, "Enter a fee rate in sat/vB."),
        (LITECOIN, "Enter a fee rate in lit/vB."),
        (BITCOIN_CASH, "Enter a fee rate in sat/B."),
        (DOGECOIN, "Enter a fee rate in koinu/B."),
    ] {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let error = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "fast"))
            .await
            .expect_err("a rate that is not a number is refused");
        assert!(matches!(error, ChainError::InvalidInput(..)), "{error}");
        assert_eq!(error.to_string(), copy);
    }
}

#[tokio::test]
async fn a_payment_the_address_cannot_cover_is_refused_with_the_shortfall() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[50_000]).await.unwrap();
        let error = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), 500_000, "2"))
            .await
            .expect_err("the balance cannot cover it");
        let ChainError::InsufficientFunds { needed, available, .. } = error else {
            panic!("{error} is not a shortfall");
        };
        assert_eq!(available, sats(50_000), "{}", params.chain);
        assert!(needed > sats(500_000), "{needed} covers the payment and a fee");
    }
}

#[tokio::test]
async fn an_absurd_fee_is_refused_by_the_ceiling_unless_the_table_raises_it() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[20_000_000]).await.unwrap();
        let error = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "20000"))
            .await
            .expect_err("the ceiling refuses it");
        let limit = format!("exceeds Jelly's limit of 0.02 {}.", params.chain.ticker());
        assert!(error.to_string().contains(&limit), "{error}");
        assert!(!error.to_string().contains("base units"), "{error}");

        let raised = UtxoParams { fee_ceiling: Amount::MAX, ..params };
        let (harness, wallet) =
            synced(Setup::on(&raised), kind_of(&params), &[20_000_000]).await.unwrap();
        harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "20000"))
            .await
            .unwrap();
    }
}

/// Both chains follow Bitcoin Core's trusted-coin rule.
///
/// Change from the wallet's own unconfirmed transaction is spendable, and someone
/// else's unconfirmed payment waits for a confirmation, because its sender can
/// still replace it.
#[tokio::test]
async fn only_the_wallets_own_unconfirmed_change_is_spent_before_it_confirms() {
    for params in [BITCOIN, LITECOIN] {
        let wallet = Wallet::new(&params, kind_of(&params), &[200_000]).unwrap();
        let incoming = Ledger::new().known(&wallet.funding).mempool(&wallet.payment).unconfirmed(
            &wallet.payment,
            0,
            200_000,
        );
        let change = 150_000;
        let own = tx_spending(
            &wallet.payment,
            0,
            &[(wallet.theirs.script_pubkey(), 40_000), (wallet.ours.script_pubkey(), change)],
        );
        let sent = Ledger::new()
            .known(&wallet.funding)
            .confirmed(&wallet.payment, MINED_AT)
            .mempool(&own)
            .unconfirmed(&own, 1, change);
        for (ledger, is_spent) in [(incoming, false), (sent, true)] {
            let harness = Harness::new(Setup::on(&params)).unwrap();
            ledger.serve(&harness);
            harness.watch(&[wallet.address()]).await.unwrap();
            harness.sync().await.unwrap();
            let built = harness
                .adapter
                .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "2"))
                .await;
            assert_eq!(built.is_ok(), is_spent, "{}: {built:?}", params.chain);
            if let Ok(unsigned) = built {
                assert_eq!(payload(&unsigned).unwrap().input_kinds.len(), 1);
            }
        }
    }
}

/// A coinbase output cannot be spent until it is 100 blocks deep, and a node
/// refuses a transaction that spends one sooner.
#[tokio::test]
async fn an_immature_coinbase_output_is_never_selected() {
    for params in CHAINS {
        let harness = Harness::new(Setup::on(&params)).unwrap();
        let ours = harness.key(kind_of(&params), 1).unwrap();
        let theirs = harness.key(kind_of(&params), 2).unwrap();
        let coinbase = coinbase_paying(ours.script_pubkey(), FUNDING_SAT, Version::TWO);
        assert!(coinbase.is_coinbase());
        // Eleven confirmations, far short of the hundred a coinbase needs.
        let mined = TIP.saturating_sub(10);
        Ledger::new()
            .confirmed(&coinbase, mined)
            .unspent(&coinbase, 0, FUNDING_SAT, mined)
            .serve(&harness);
        harness.watch(&[watched(&params, 7, ours.address())]).await.unwrap();
        harness.sync().await.unwrap();
        assert!(
            harness.drain().iter().any(|event| matches!(
                *event,
                WalletEvent::BalanceChanged(ref update)
                    if update.address_id == 7 && update.balance == sats(FUNDING_SAT)
            )),
            "{}: the sync counted the immature coinbase",
            params.chain
        );

        let error = harness
            .adapter
            .build_tx(&harness.request(theirs.address(), PAYMENT_SAT, "2"))
            .await
            .expect_err("the only coin is immature");
        assert!(
            matches!(error, ChainError::InsufficientFunds { available, .. } if available == Amount::ZERO),
            "{}: {error}",
            params.chain
        );
    }
}

/// Litecoin's `PEGOUT_MATURITY` holds an output a `HogEx` pays out of MWEB for six
/// blocks, and Litecoin Core refuses a spend of one sooner.
#[tokio::test]
async fn an_immature_litecoin_pegout_output_is_never_selected() {
    let harness = Harness::new(Setup::on(&LITECOIN)).unwrap();
    let ours = harness.key(ScriptKind::P2wpkh, 1).unwrap();
    let theirs = harness.key(ScriptKind::P2wpkh, 2).unwrap();
    let mut hog_bytes = vec![0x58, 0x20];
    hog_bytes.extend_from_slice(&[0x11; 32]);
    let hog_address = ScriptBuf::from(hog_bytes);
    let previous = tx_paying(&[(hog_address.clone(), 1_000_000_000)]);
    let hogex = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint { txid: previous.compute_txid(), vout: 0 },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![
            TxOut {
                value: bitcoin::Amount::from_sat(1_000_000_000 - FUNDING_SAT),
                script_pubkey: hog_address,
            },
            TxOut {
                value: bitcoin::Amount::from_sat(FUNDING_SAT),
                script_pubkey: ours.script_pubkey(),
            },
        ],
    };
    // Litecoin Core serves a `HogEx` with the MWEB flag and an empty MWEB body.
    let legacy = serialize(&hogex);
    let (head, tail) = legacy.split_at(4);
    let (body, lock_time) = tail.split_at(tail.len() - 4);
    let mut served = head.to_vec();
    served.extend_from_slice(&[0x00, 0x08]);
    served.extend_from_slice(body);
    served.push(0x00);
    served.extend_from_slice(lock_time);

    // Three confirmations, short of the six a pegout needs.
    let mined = TIP.saturating_sub(2);
    harness.chain.tip(TIP, BLOCK_UNIX_SECONDS);
    harness.chain.header(BLOCK_UNIX_SECONDS);
    harness.chain.history(&[(hogex.compute_txid(), i32::try_from(mined).unwrap())]);
    harness.chain.unspent(&[(hogex.compute_txid(), 1, FUNDING_SAT, mined)]);
    harness
        .chain
        .server
        .on(Method::GetTransaction, Answer::Each(vec![quoted(&hex::encode(&served))]));
    harness.queue_transactions(from_ref(&previous));
    harness.watch(&[watched(&LITECOIN, 7, ours.address())]).await.unwrap();
    harness.sync().await.unwrap();
    assert!(
        harness.drain().iter().any(|event| matches!(
            *event,
            WalletEvent::BalanceChanged(ref update)
                if update.address_id == 7 && update.balance == sats(FUNDING_SAT)
        )),
        "the sync counted the immature pegout"
    );

    let error = harness
        .adapter
        .build_tx(&harness.request(theirs.address(), PAYMENT_SAT, "2"))
        .await
        .expect_err("the only coin is immature");
    assert!(
        matches!(error, ChainError::InsufficientFunds { available, .. } if available == Amount::ZERO),
        "{error}"
    );
}

#[tokio::test]
async fn the_preview_describes_the_payload_in_the_chains_own_unit() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let unsigned = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "2"))
            .await
            .unwrap();
        let payload = payload(&unsigned).unwrap();

        assert_eq!(unsigned.preview.chain, params.chain);
        assert_eq!(unsigned.preview.asset, AssetId::Native(params.chain));
        assert_eq!(unsigned.preview.fee, payload.fee);
        assert_eq!(unsigned.preview.from, payload.sources);
        assert_eq!(unsigned.preview.to, payload.to);
        let detail = |label: &str| {
            unsigned
                .preview
                .details
                .iter()
                .find(|row| row.label == label)
                .map(|row| row.value.clone())
        };
        assert_eq!(
            detail("Fee"),
            Some(format!(
                "{} {}",
                payload.fee.format(params.chain.native_decimals()),
                params.chain.ticker()
            ))
        );
        assert!(
            detail("Fee rate")
                .is_some_and(|rate| rate.ends_with(&format!(" {}", params.fee_rate_unit))),
            "{:?}",
            detail("Fee rate")
        );
    }
}
