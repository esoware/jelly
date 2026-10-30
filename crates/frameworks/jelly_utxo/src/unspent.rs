use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use bitcoin::{OutPoint, Script, Transaction, Txid};
use jelly_electrum::Unspent;

use crate::cursor::CursorUtxo;
use crate::params::UtxoParams;
use crate::prevout::PrevoutMap;
use crate::script::SpendableScript;
use crate::slp;
use crate::utxo::{Origin, Utxo, units, value_of};

/// Why an unspent entry a server listed wasn't believed.
///
/// A light client can't verify that an output is still unspent, but it verifies
/// everything else against the funding transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnspentRefusal {
    NoFundingTransaction,
    NoSuchOutput,
    WrongScript,
    WrongValue,
    Duplicate,
}

impl UnspentRefusal {
    const fn describe(self) -> &'static str {
        match self {
            Self::NoFundingTransaction => {
                "the server listed an unspent output whose transaction it didn't return"
            }
            Self::NoSuchOutput => "there's no such output in the funding transaction",
            Self::WrongScript => "the listed output doesn't pay this address",
            Self::WrongValue => "the listed value isn't what the funding transaction says",
            Self::Duplicate => "the same output was listed more than once",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RefusedUnspent {
    outpoint: OutPoint,
    refusal: UnspentRefusal,
}

impl fmt::Display for RefusedUnspent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.outpoint, self.refusal.describe())
    }
}

/// What a pass knows about the transactions that funded the listed coins.
pub(crate) struct Funding<'a> {
    pub(crate) params: &'a UtxoParams,
    pub(crate) txs: &'a BTreeMap<Txid, Transaction>,
    /// The Litecoin `HogEx` transactions among `txs`.
    pub(crate) hogexes: &'a BTreeSet<Txid>,
    pub(crate) prevouts: &'a PrevoutMap,
    /// Every watched address's script, which is what makes a transaction the
    /// wallet's own.
    pub(crate) own_scripts: Vec<&'a Script>,
}

impl Funding<'_> {
    /// Whether every input of `tx` spends one of the wallet's own outputs. An input
    /// whose prevout the pass couldn't read isn't assumed to be ours.
    fn is_own(&self, tx: &Transaction) -> bool {
        !tx.is_coinbase()
            && tx.input.iter().all(|txin| {
                self.prevouts.get(txin.previous_output).is_some_and(|txout| {
                    let locking = self.params.locking(txout.script_pubkey.as_script());
                    self.own_scripts.contains(&locking.script)
                })
            })
    }
}

/// Verifies listed values and scripts against the funding transactions, so an
/// inflated legacy prevout can't turn into excess fee.
///
/// Duplicates are dropped so a balance can't double and an input can't repeat.
/// Spentness is still the server's word. The funding transaction also tells whether
/// the coin has to mature first (a coinbase output, or a peg-out after a `HogEx`'s
/// first output), whether the wallet made it, and whether it carries tokens through a
/// `CashTokens` prefix or an SLP message. The server's own token report isn't trusted.
///
/// A coin in `verified` passed all that in an earlier pass, so only its value is
/// checked again and its funding transaction isn't needed.
pub(crate) fn verified_unspents(
    listed: &[Unspent],
    script: &SpendableScript,
    verified: &[CursorUtxo],
    funding: &Funding<'_>,
) -> (Vec<Utxo>, Vec<RefusedUnspent>) {
    let Funding { params, txs, hogexes, .. } = *funding;
    let mut coins = Vec::with_capacity(listed.len());
    let mut refused = Vec::new();
    let mut seen: BTreeSet<OutPoint> = BTreeSet::new();
    for entry in listed {
        let outpoint = entry.outpoint;
        let height = entry.height;
        let mut refuse = |refusal| refused.push(RefusedUnspent { outpoint, refusal });
        if !seen.insert(outpoint) {
            refuse(UnspentRefusal::Duplicate);
            continue;
        }
        if let Some(known) = verified.iter().find(|coin| coin.outpoint == outpoint) {
            if units(entry.value.to_sat()) == known.value {
                coins.push(
                    Utxo::new(outpoint, known.value, script.clone(), height, known.origin)
                        .holding_tokens(known.has_tokens),
                );
            } else {
                refuse(UnspentRefusal::WrongValue);
            }
            continue;
        }
        let Some(tx) = txs.get(&outpoint.txid) else {
            refuse(UnspentRefusal::NoFundingTransaction);
            continue;
        };
        let index = usize::try_from(outpoint.vout).unwrap_or(usize::MAX);
        let Some(txout) = tx.output.get(index) else {
            refuse(UnspentRefusal::NoSuchOutput);
            continue;
        };
        let locking = params.locking(txout.script_pubkey.as_script());
        if locking.script != script.script_pubkey() {
            refuse(UnspentRefusal::WrongScript);
            continue;
        }
        if txout.value != entry.value {
            refuse(UnspentRefusal::WrongValue);
            continue;
        }
        let origin = if tx.is_coinbase() {
            Origin::Coinbase
        } else if outpoint.vout > 0 && hogexes.contains(&outpoint.txid) {
            Origin::Pegout
        } else if funding.is_own(tx) {
            Origin::Own
        } else {
            Origin::Ordinary
        };
        let has_tokens =
            locking.has_tokens || (params.has_slp_tokens && slp::carries_tokens(tx, outpoint.vout));
        coins.push(
            Utxo::new(outpoint, value_of(txout), script.clone(), height, origin)
                .holding_tokens(has_tokens),
        );
    }
    coins.sort_by_key(Utxo::outpoint);
    (coins, refused)
}

#[cfg(test)]
mod tests {
    use std::slice::from_ref;

    use bitcoin::ScriptBuf;
    use bitcoin::consensus::deserialize;
    use serde::Deserialize;

    use super::*;
    use crate::cursor::SyncCursor;
    use crate::fixtures::{FUNDED_SAT, MINED_AT, RECEIVED_SAT, received};
    use crate::test_util::{BITCOIN, BITCOIN_CASH, TestKey, fixture_txid, tx_paying, tx_spending};

    const SENT_SAT: u64 = 50_000;
    const CHANGE_SAT: u64 = 120_000;

    fn funding<'a>(
        txs: &'a BTreeMap<Txid, Transaction>,
        hogexes: &'a BTreeSet<Txid>,
        prevouts: &'a PrevoutMap,
        ours: &'a TestKey,
    ) -> Funding<'a> {
        Funding {
            params: &BITCOIN,
            txs,
            hogexes,
            prevouts,
            own_scripts: vec![ours.script().script_pubkey()],
        }
    }

    #[test]
    fn a_server_that_lies_about_an_unspent_entry_is_caught() {
        let received = received().unwrap();
        let txid = received.payment.compute_txid();
        let mut txs = BTreeMap::new();
        txs.insert(txid, received.payment.clone());
        txs.insert(received.funding.compute_txid(), received.funding.clone());
        let no_hogex = BTreeSet::new();

        let honest = Unspent {
            outpoint: OutPoint { txid, vout: 0 },
            value: bitcoin::Amount::from_sat(RECEIVED_SAT),
            height: Some(MINED_AT),
        };
        let prevouts = PrevoutMap::index(txs.values());
        let funding = funding(&txs, &no_hogex, &prevouts, &received.ours);
        let (kept, refused) =
            verified_unspents(from_ref(&honest), received.ours.script(), &[], &funding);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept.first().map(Utxo::value), Some(units(RECEIVED_SAT)));
        assert!(refused.is_empty());

        let inflated = Unspent { value: bitcoin::Amount::from_sat(5_000_000), ..honest };
        let no_such_output = Unspent { outpoint: OutPoint { txid, vout: 9 }, ..honest };
        let not_ours = Unspent {
            outpoint: OutPoint { txid: received.funding.compute_txid(), vout: 0 },
            ..honest
        };
        let unknown = Unspent { outpoint: OutPoint { txid: fixture_txid(99), vout: 0 }, ..honest };
        let (kept, refused) = verified_unspents(
            &[inflated, no_such_output, not_ours, unknown],
            received.ours.script(),
            &[],
            &funding,
        );
        assert!(kept.is_empty(), "no unverifiable coin may reach selection");
        let reasons: Vec<UnspentRefusal> = refused.iter().map(|entry| entry.refusal).collect();
        assert_eq!(
            reasons,
            vec![
                UnspentRefusal::WrongValue,
                UnspentRefusal::NoSuchOutput,
                UnspentRefusal::WrongScript,
                UnspentRefusal::NoFundingTransaction,
            ]
        );
        for entry in &refused {
            assert!(entry.to_string().contains(entry.refusal.describe()));
        }
    }

    #[test]
    fn an_outpoint_listed_twice_is_counted_once() {
        let received = received().unwrap();
        let txid = received.payment.compute_txid();
        let mut txs = BTreeMap::new();
        txs.insert(txid, received.payment.clone());

        let entry = Unspent {
            outpoint: OutPoint { txid, vout: 0 },
            value: bitcoin::Amount::from_sat(RECEIVED_SAT),
            height: Some(MINED_AT),
        };
        let (no_hogex, prevouts) = (BTreeSet::new(), PrevoutMap::index(txs.values()));
        let (kept, refused) = verified_unspents(
            &[entry, entry],
            received.ours.script(),
            &[],
            &funding(&txs, &no_hogex, &prevouts, &received.ours),
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(
            Utxo::total(&kept).unwrap(),
            units(RECEIVED_SAT),
            "the balance is the coin, not two of it"
        );
        assert_eq!(
            refused.iter().map(|entry| entry.refusal).collect::<Vec<UnspentRefusal>>(),
            vec![UnspentRefusal::Duplicate]
        );
    }

    #[test]
    fn the_funding_transaction_says_which_coins_wait_to_mature() {
        let received = received().unwrap();
        let mut coinbase = tx_paying(&[(received.ours.script_pubkey(), RECEIVED_SAT)]);
        if let Some(txin) = coinbase.input.first_mut() {
            txin.previous_output = OutPoint::null();
        }
        assert!(coinbase.is_coinbase());
        let hogex = tx_paying(&[
            (received.theirs.script_pubkey(), FUNDED_SAT),
            (received.ours.script_pubkey(), RECEIVED_SAT),
        ]);
        let payment = received.payment.compute_txid();
        let txs: BTreeMap<Txid, Transaction> = [&coinbase, &hogex, &received.payment]
            .into_iter()
            .map(|tx| (tx.compute_txid(), tx.clone()))
            .collect();
        let hogexes = BTreeSet::from([hogex.compute_txid()]);
        let listed = |txid, vout| Unspent {
            outpoint: OutPoint { txid, vout },
            value: bitcoin::Amount::from_sat(RECEIVED_SAT),
            height: Some(MINED_AT),
        };

        let (kept, refused) = verified_unspents(
            &[
                listed(coinbase.compute_txid(), 0),
                listed(hogex.compute_txid(), 1),
                listed(payment, 0),
            ],
            received.ours.script(),
            &[],
            &funding(&txs, &hogexes, &PrevoutMap::index(txs.values()), &received.ours),
        );
        assert!(refused.is_empty(), "{refused:?}");
        let origin_of =
            |txid| kept.iter().find(|coin| coin.outpoint().txid == txid).map(Utxo::origin);
        assert_eq!(origin_of(coinbase.compute_txid()), Some(Origin::Coinbase));
        assert_eq!(origin_of(hogex.compute_txid()), Some(Origin::Pegout));
        assert_eq!(origin_of(payment), Some(Origin::Ordinary));
    }

    const TOKEN_TRANSACTION: &str =
        include_str!("../tests/vectors/bitcoin_cash/token_transaction.json");

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TokenTransaction {
        tx: String,
        token_vout: u32,
        value: u64,
        locking_script: String,
    }

    /// A mainnet output carrying tokens to a P2PKH script is that address's coin on
    /// Bitcoin Cash, held back from spending, and on a chain without tokens it pays
    /// some other script.
    #[test]
    fn a_mainnet_token_coin_is_counted_and_held_back_from_spending() {
        let file: TokenTransaction = serde_json::from_str(TOKEN_TRANSACTION).unwrap();
        let tx: Transaction = deserialize(&hex::decode(&file.tx).unwrap()).unwrap();
        let txid = tx.compute_txid();
        let script =
            SpendableScript::new(ScriptBuf::from(hex::decode(&file.locking_script).unwrap()))
                .unwrap();
        let listed = Unspent {
            outpoint: OutPoint { txid, vout: file.token_vout },
            value: bitcoin::Amount::from_sat(file.value),
            height: Some(971_424),
        };
        let txs = BTreeMap::from([(txid, tx)]);
        let (hogexes, prevouts) = (BTreeSet::new(), PrevoutMap::default());
        let own_scripts = vec![script.script_pubkey()];

        let on_bitcoin_cash = Funding {
            params: &BITCOIN_CASH,
            txs: &txs,
            hogexes: &hogexes,
            prevouts: &prevouts,
            own_scripts: own_scripts.clone(),
        };
        let (kept, refused) = verified_unspents(from_ref(&listed), &script, &[], &on_bitcoin_cash);
        assert!(refused.is_empty(), "{refused:?}");
        let coin = kept.first().expect("the coin");
        assert!(coin.has_tokens());
        assert_eq!(coin.value(), units(file.value));
        assert!(!coin.is_selectable(&BITCOIN_CASH, 980_000), "tokens are never spent");

        let cursor = SyncCursor::new(None, &kept, &[], Vec::new());
        let back = SyncCursor::decode(&cursor.encode().unwrap()).expect("it decodes");
        assert_eq!(back.restore(&script), kept, "the flag survives a restart");
        let known = back.coins().to_vec();
        let (again, _) = verified_unspents(from_ref(&listed), &script, &known, &on_bitcoin_cash);
        assert_eq!(again, kept, "a coin an earlier pass verified keeps its flag");

        let on_bitcoin = Funding { params: &BITCOIN, ..on_bitcoin_cash };
        let (kept, refused) = verified_unspents(from_ref(&listed), &script, &[], &on_bitcoin);
        assert!(kept.is_empty());
        assert_eq!(
            refused.iter().map(|entry| entry.refusal).collect::<Vec<_>>(),
            [UnspentRefusal::WrongScript]
        );
    }

    const SLP_TRANSACTIONS: &str =
        include_str!("../tests/vectors/bitcoin_cash/slp_transactions.json");

    #[derive(Deserialize)]
    struct SlpTransactions {
        transactions: Vec<SlpTransaction>,
    }

    #[derive(Deserialize)]
    struct SlpTransaction {
        txid: String,
        tx: String,
    }

    /// A mainnet SPICE send paid one address both a token output and its plain
    /// change.
    ///
    /// On Bitcoin Cash the token output is that address's coin, counted and held
    /// back, while the change stays spendable, and the flag survives a restart.
    #[test]
    fn a_mainnet_slp_output_is_held_and_the_change_beside_it_is_not() {
        let file: SlpTransactions = serde_json::from_str(SLP_TRANSACTIONS).unwrap();
        let send = file
            .transactions
            .iter()
            .find(|case| case.txid.starts_with("90b5bce5"))
            .expect("the SPICE send");
        let tx: Transaction = deserialize(&hex::decode(&send.tx).unwrap()).unwrap();
        let txid = tx.compute_txid();
        let script = SpendableScript::new(tx.output[2].script_pubkey.clone()).unwrap();
        assert_eq!(tx.output[3].script_pubkey, *script.script_buf(), "both pay one address");
        let listed: Vec<Unspent> = [2u32, 3]
            .into_iter()
            .map(|vout| Unspent {
                outpoint: OutPoint { txid, vout },
                value: tx.output[usize::try_from(vout).unwrap()].value,
                height: Some(600_000),
            })
            .collect();
        let txs = BTreeMap::from([(txid, tx)]);
        let (hogexes, prevouts) = (BTreeSet::new(), PrevoutMap::default());
        let on_bitcoin_cash = Funding {
            params: &BITCOIN_CASH,
            txs: &txs,
            hogexes: &hogexes,
            prevouts: &prevouts,
            own_scripts: vec![script.script_pubkey()],
        };

        let (kept, refused) = verified_unspents(&listed, &script, &[], &on_bitcoin_cash);
        assert!(refused.is_empty(), "{refused:?}");
        let held: Vec<(u32, bool)> =
            kept.iter().map(|coin| (coin.outpoint().vout, coin.has_tokens())).collect();
        assert_eq!(held, [(2, true), (3, false)]);
        assert_eq!(Utxo::total(&kept).unwrap(), units(546 + 699_093), "both count");
        assert!(
            kept.iter().all(|coin| coin.is_selectable(&BITCOIN_CASH, 700_000) != coin.has_tokens())
        );

        let cursor = SyncCursor::new(None, &kept, &[], Vec::new());
        let back = SyncCursor::decode(&cursor.encode().unwrap()).expect("it decodes");
        assert_eq!(back.restore(&script), kept, "the flag survives a restart");

        let without_slp = UtxoParams { has_slp_tokens: false, ..BITCOIN_CASH };
        let plain = Funding { params: &without_slp, ..on_bitcoin_cash };
        let (kept, _) = verified_unspents(&listed, &script, &[], &plain);
        assert!(kept.iter().all(|coin| !coin.has_tokens()));
    }

    #[test]
    fn a_mempool_output_is_unconfirmed_rather_than_at_height_zero() {
        let received = received().unwrap();
        let txid = received.payment.compute_txid();
        let mut txs = BTreeMap::new();
        txs.insert(txid, received.payment.clone());
        let entry = Unspent {
            outpoint: OutPoint { txid, vout: 0 },
            value: bitcoin::Amount::from_sat(RECEIVED_SAT),
            height: None,
        };
        let (no_hogex, prevouts) = (BTreeSet::new(), PrevoutMap::index(txs.values()));
        let (kept, refused) = verified_unspents(
            &[entry],
            received.ours.script(),
            &[],
            &funding(&txs, &no_hogex, &prevouts, &received.ours),
        );
        assert!(refused.is_empty());
        let coin = kept.first().expect("a coin");
        assert_eq!(coin.height(), None);
        assert!(!coin.is_confirmed());
    }

    /// Change from a transaction that spends only the wallet's own coins is
    /// trusted before it confirms. A payment whose input the wallet doesn't own, or
    /// whose input the pass couldn't read, isn't.
    #[test]
    fn only_a_transaction_spending_the_wallets_own_coins_makes_trusted_change() {
        let received = received().unwrap();
        let change = tx_spending(
            &received.payment,
            0,
            &[
                (received.theirs.script_pubkey(), SENT_SAT),
                (received.ours.script_pubkey(), CHANGE_SAT),
            ],
        );
        let txs: BTreeMap<Txid, Transaction> = [&received.funding, &received.payment, &change]
            .into_iter()
            .map(|tx| (tx.compute_txid(), tx.clone()))
            .collect();
        let no_hogex = BTreeSet::new();
        let prevouts = PrevoutMap::index(txs.values());
        let listed = |txid, vout, value| Unspent {
            outpoint: OutPoint { txid, vout },
            value: bitcoin::Amount::from_sat(value),
            height: None,
        };
        let (kept, refused) = verified_unspents(
            &[
                listed(received.payment.compute_txid(), 0, RECEIVED_SAT),
                listed(change.compute_txid(), 1, CHANGE_SAT),
            ],
            received.ours.script(),
            &[],
            &funding(&txs, &no_hogex, &prevouts, &received.ours),
        );
        assert!(refused.is_empty(), "{refused:?}");
        let origin_of =
            |txid| kept.iter().find(|coin| coin.outpoint().txid == txid).map(Utxo::origin);
        assert_eq!(origin_of(received.payment.compute_txid()), Some(Origin::Ordinary));
        assert_eq!(origin_of(change.compute_txid()), Some(Origin::Own));
        assert_eq!(kept.iter().filter(|coin| coin.is_trusted()).count(), 1);

        let unread = PrevoutMap::index([&change]);
        let (kept, _) = verified_unspents(
            &[listed(change.compute_txid(), 1, CHANGE_SAT)],
            received.ours.script(),
            &[],
            &funding(&txs, &no_hogex, &unread, &received.ours),
        );
        assert_eq!(kept.first().map(Utxo::origin), Some(Origin::Ordinary));
    }
}
