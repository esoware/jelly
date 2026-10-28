use std::collections::{BTreeMap, HashSet};

use bitcoin::{OutPoint, Txid};
use parking_lot::Mutex;

use crate::coin::Utxo;
use crate::error::UtxoError;

/// Claims selected outpoints under the unsigned transaction's txid.
///
/// Rebuilding the same payload reuses its claims, but a payload at a different
/// fee has a different txid and needs an explicit discard. A claim lasts until a
/// discard, a replacement, or a sync that sees the spend.
#[derive(Debug, Default)]
pub(crate) struct OutpointClaims {
    held: Mutex<BTreeMap<OutPoint, Hold>>,
}

#[derive(Debug, Clone)]
struct Hold {
    owner: Txid,
    /// A mempool transaction already spent the coin when it was claimed, as the
    /// original does for every input a replacement takes over.
    is_spent_at_claim: bool,
    /// The hold a replacement took the coin over from. The original still spends the
    /// coin until the replacement is broadcast, so releasing the replacement hands
    /// the hold back.
    displaced: Option<Box<Self>>,
}

impl OutpointClaims {
    /// Claims every outpoint for `owner`, or claims none of them.
    pub(crate) fn claim(&self, owner: Txid, outpoints: &[OutPoint]) -> Result<(), UtxoError> {
        let mut held = self.held.lock();
        for outpoint in outpoints {
            if held.get(outpoint).is_some_and(|current| current.owner != owner) {
                return Err(UtxoError::OutpointClaimed { outpoint: *outpoint });
            }
        }
        for outpoint in outpoints {
            held.insert(*outpoint, Hold { owner, is_spent_at_claim: false, displaced: None });
        }
        Ok(())
    }

    /// Claims every outpoint for the replacement `owner`, taking over any claim that
    /// holds one of them. `already_spent` lists the ones the replaced transaction
    /// spends in the mempool.
    ///
    /// A claim taken over is released whole. Its hold on each coin the replacement
    /// spends is kept beneath the replacement's, for [`Self::release`] to hand back.
    pub(crate) fn take_over(
        &self,
        owner: Txid,
        outpoints: &[OutPoint],
        already_spent: &[OutPoint],
    ) {
        let mut held = self.held.lock();
        let taken: HashSet<Txid> = outpoints
            .iter()
            .filter_map(|outpoint| held.get(outpoint))
            .map(|hold| hold.owner)
            .filter(|holder| *holder != owner)
            .collect();
        let mut displaced: BTreeMap<OutPoint, Hold> =
            held.extract_if(.., |_, hold| taken.contains(&hold.owner)).collect();
        for outpoint in outpoints {
            held.entry(*outpoint).or_insert_with(|| Hold {
                owner,
                is_spent_at_claim: already_spent.contains(outpoint),
                displaced: displaced.remove(outpoint).map(Box::new),
            });
        }
    }

    /// Releases every claim of `owner`, handing each coin a replacement took over
    /// back to the hold it took it from.
    pub(crate) fn release(&self, owner: Txid) {
        let mut held = self.held.lock();
        for hold in held.values_mut() {
            if hold.owner == owner
                && let Some(displaced) = hold.displaced.take()
            {
                *hold = *displaced;
            }
        }
        held.retain(|_, hold| hold.owner != owner);
    }

    #[cfg(any(test, feature = "test-util"))]
    pub(crate) fn owner(&self, outpoint: OutPoint) -> Option<Txid> {
        self.held.lock().get(&outpoint).map(|hold| hold.owner)
    }

    /// Every claimed outpoint, in a stable order.
    #[cfg(any(test, feature = "test-util"))]
    pub(crate) fn claimed(&self) -> Vec<OutPoint> {
        self.held.lock().keys().copied().collect()
    }

    pub(crate) fn len(&self) -> usize {
        self.held.lock().len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.held.lock().is_empty()
    }

    /// `coins` minus every claimed one.
    pub(crate) fn spendable(&self, coins: &[Utxo]) -> Vec<Utxo> {
        let held = self.held.lock();
        coins.iter().filter(|coin| !held.contains_key(&coin.outpoint())).cloned().collect()
    }

    /// Releases every claim that lost a coin which was unspent when claimed, since its
    /// transaction confirmed or was replaced. A coin already spent at claim time says
    /// nothing by being missing. A claim with no coin in `live` protects nothing and
    /// is released too.
    pub(crate) fn retain_live(&self, live: &[OutPoint]) {
        let live: HashSet<OutPoint> = live.iter().copied().collect();
        let mut held = self.held.lock();
        let mut holding_live: HashSet<Txid> = HashSet::new();
        let mut stale: HashSet<Txid> = HashSet::new();
        for (outpoint, hold) in held.iter() {
            if live.contains(outpoint) {
                holding_live.insert(hold.owner);
            } else if !hold.is_spent_at_claim {
                stale.insert(hold.owner);
            }
        }
        held.retain(|_, hold| holding_live.contains(&hold.owner) && !stale.contains(&hold.owner));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::bitcoin_scene;
    use crate::test_util::fixture_txid;

    #[test]
    fn an_outpoint_committed_to_a_pending_transaction_is_not_offered_again() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[60_000, 60_000]);
        let owner = fixture_txid(7);
        let claims = OutpointClaims::default();

        let first = coins.first().map(Utxo::outpoint).expect("a coin");
        claims.claim(owner, &[first]).unwrap();
        assert_eq!(claims.owner(first), Some(owner));
        assert_eq!(claims.len(), 1);

        let spendable = claims.spendable(&coins);
        assert_eq!(spendable.len(), 1);
        assert!(spendable.iter().all(|coin| coin.outpoint() != first));

        // The same owner can reclaim, and a different one conflicts.
        claims.claim(owner, &[first]).unwrap();
        let other = fixture_txid(8);
        assert!(matches!(claims.claim(other, &[first]), Err(UtxoError::OutpointClaimed { .. })));

        claims.release(owner);
        assert!(claims.is_empty());
        assert_eq!(claims.spendable(&coins).len(), 2);
    }

    #[test]
    fn a_claim_that_conflicts_leaves_every_other_outpoint_untouched() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[10_000, 20_000, 30_000]);
        let outpoints: Vec<_> = coins.iter().map(Utxo::outpoint).collect();
        let held = fixture_txid(1);
        let wanted = fixture_txid(2);
        let claims = OutpointClaims::default();

        claims.claim(held, &outpoints[..1]).unwrap();
        assert!(claims.claim(wanted, &outpoints).is_err());
        assert_eq!(claims.claimed(), outpoints[..1].to_vec(), "a refused claim takes nothing");
    }

    #[test]
    fn a_claim_whose_coins_are_no_longer_unspent_is_released() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[10_000, 20_000]);
        let outpoints: Vec<_> = coins.iter().map(Utxo::outpoint).collect();
        let owner = fixture_txid(3);
        let claims = OutpointClaims::default();
        claims.claim(owner, &outpoints).unwrap();

        claims.retain_live(&outpoints);
        assert_eq!(claims.len(), 2, "nothing moved, so nothing is released");

        claims.retain_live(&outpoints[..1]);
        assert!(claims.is_empty(), "one input gone releases the whole claim");

        claims.claim(owner, &outpoints).unwrap();
        claims.retain_live(&[]);
        assert!(claims.is_empty(), "a pass that sees none of them releases it too");
    }

    #[test]
    fn a_replacement_takes_over_the_claim_the_transaction_it_replaces_holds() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[10_000, 20_000]);
        let outpoints: Vec<_> = coins.iter().map(Utxo::outpoint).collect();
        let original = fixture_txid(4);
        let replacement = fixture_txid(5);
        let claims = OutpointClaims::default();
        claims.claim(original, &outpoints).unwrap();

        assert!(claims.claim(replacement, &outpoints).is_err(), "not without taking it over");
        claims.take_over(replacement, &outpoints[..1], &outpoints[..1]);
        assert_eq!(
            claims.claimed(),
            outpoints[..1].to_vec(),
            "one coin taken over releases the whole claim"
        );
        assert_eq!(claims.owner(outpoints[0]), Some(replacement));
    }

    /// A replacement also claims the inputs its original spends in the mempool,
    /// which no pass lists as unspent.
    #[test]
    fn a_coin_already_spent_when_claimed_does_not_release_the_claim() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[10_000, 20_000]);
        let outpoints: Vec<_> = coins.iter().map(Utxo::outpoint).collect();
        let (replaced, added) = outpoints.split_at(1);
        let claims = OutpointClaims::default();
        claims.take_over(fixture_txid(6), &outpoints, replaced);

        claims.retain_live(added);
        assert_eq!(claims.len(), 2, "the coin it added is still unspent");
        claims.retain_live(&[]);
        assert!(claims.is_empty(), "the coin it added is gone");

        claims.take_over(fixture_txid(6), replaced, replaced);
        claims.retain_live(&[]);
        assert!(claims.is_empty(), "a claim on nothing unspent protects nothing");
    }

    /// Backing out of a replacement hands the coins back to the transaction it would
    /// have replaced, which still spends them in the mempool.
    #[test]
    fn releasing_a_replacement_hands_back_the_coins_it_took_over() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[10_000, 20_000]);
        let outpoints: Vec<_> = coins.iter().map(Utxo::outpoint).collect();
        let (replaced, added) = outpoints.split_at(1);
        let (original, first, second) = (fixture_txid(9), fixture_txid(10), fixture_txid(11));
        let claims = OutpointClaims::default();
        claims.claim(original, replaced).unwrap();

        claims.take_over(first, &outpoints, replaced);
        claims.take_over(second, &outpoints, replaced);
        assert_eq!(claims.owner(outpoints[0]), Some(second));
        claims.release(second);
        assert_eq!(claims.owner(outpoints[0]), Some(first), "the replacement it replaced");
        assert_eq!(claims.owner(outpoints[1]), Some(first));

        claims.release(first);
        assert_eq!(claims.claimed(), replaced.to_vec(), "only the original's own coin");
        assert_eq!(claims.owner(outpoints[0]), Some(original));
        claims.retain_live(added);
        assert!(claims.is_empty(), "a pass that sees its coin spent releases it as before");
    }
}
