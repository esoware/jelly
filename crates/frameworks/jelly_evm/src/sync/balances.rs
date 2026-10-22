use std::collections::BTreeMap;
use std::ops::Range;

use alloy::primitives::U256;
use jelly_core::{Amount, AssetId, BalanceUpdate, WalletEvent};

use super::sends::Spend;
use super::{PassInputs, TARGET};
use crate::client::BalanceRead;
use crate::cursor::ScanCursor;
use crate::error::EvmError;
use crate::units;
use crate::watched::Watched;

/// How much of one address's native balance change this pass can account for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Explained {
    /// The wei the address's own sends took out since the balance last judged,
    /// their fees and whatever native value they paid to another address.
    Spent(u128),
    /// Something accounts for the change without saying by how much, so no
    /// remainder can be told apart from it.
    Wholly,
    /// The indexer did not finish looking at this address, so a remainder now may
    /// be a transfer it reports later. The change is judged on a later pass,
    /// against the balance the last judged pass kept.
    Deferred,
}

impl Explained {
    fn plus(self, other: Self) -> Self {
        match (self, other) {
            (Self::Deferred, _) | (_, Self::Deferred) => Self::Deferred,
            (Self::Wholly, _) | (_, Self::Wholly) => Self::Wholly,
            (Self::Spent(left), Self::Spent(right)) => {
                left.checked_add(right).map_or(Self::Wholly, Self::Spent)
            }
        }
    }
}

/// What the sends a balance newly holds took out of it, given the account nonce
/// the last judged balance was read at and the one this balance was read at.
///
/// Only one transaction per nonce can confirm, so a nonce some receipt answered
/// for counts that one and its rivals count nothing. A nonce whose sends have no
/// receipt yet defers the judgement, because the balance holds what it spent and
/// no node has said how much. A nonce this wallet never sent from explains
/// nothing, so a payment out from elsewhere is still recorded.
fn spent_between(spends: &[Spend], address_id: i64, nonces: Range<u64>) -> Explained {
    let mut by_nonce: BTreeMap<u64, Option<Explained>> = BTreeMap::new();
    for spend in spends {
        if spend.address_id != address_id || !nonces.contains(&spend.nonce) {
            continue;
        }
        let slot = by_nonce.entry(spend.nonce).or_default();
        if spend.spent.is_some() {
            *slot = spend.spent;
        }
    }
    by_nonce
        .into_values()
        .fold(Explained::Spent(0), |total, spent| total.plus(spent.unwrap_or(Explained::Deferred)))
}

/// One address's answers to the balance read. An entry the node would not
/// answer, or answered with a number too wide for its unit, is `None`, an unknown
/// balance rather than a zero one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Sheet {
    pub(super) native: Option<u128>,
    /// In registry order.
    tokens: Vec<Option<u128>>,
    /// Says which of the address's own sends the native balance holds.
    nonce: Option<u64>,
}

/// Every balance and every nonce, in one round trip.
pub(super) async fn read_sheets(
    inputs: &PassInputs<'_>,
    watched: &[Watched],
) -> Result<Vec<Sheet>, EvmError> {
    let table = inputs.tokens.all();
    let mut plan = Vec::with_capacity(watched.len().saturating_mul(table.len().saturating_add(2)));
    for entry in watched {
        plan.push(BalanceRead::Native { owner: entry.owner });
        for token in table {
            plan.push(BalanceRead::Token { owner: entry.owner, token: *token });
        }
        plan.push(BalanceRead::Nonce { owner: entry.owner });
    }
    let mut answers = inputs.client.balances(&plan).await?.into_iter();
    let mut next = || answers.next().flatten();
    Ok(watched
        .iter()
        .map(|_| Sheet {
            native: narrowed(next(), |word| units::to_u128("native balance", word)),
            tokens: table
                .iter()
                .map(|_| narrowed(next(), |word| units::to_u128("token balance", word)))
                .collect(),
            nonce: narrowed(next(), |word| units::to_u64("account nonce", word)),
        })
        .collect())
}

/// A contract can answer any word, so one too wide for its unit costs that one
/// entry, like an entry the node would not answer, and not the pass.
fn narrowed<T>(word: Option<U256>, narrow: impl FnOnce(U256) -> Result<T, EvmError>) -> Option<T> {
    narrow(word?)
        .inspect_err(|error| {
            tracing::warn!(target: TARGET, %error, "a balance read answer is left unknown");
        })
        .ok()
}

/// A native change that neither the address's own sends nor the indexer's rows
/// explain, which is recorded as a balance delta with an unknown outcome rather
/// than as a decoded transaction.
#[derive(Debug, Clone, Copy)]
pub(super) struct Unexplained {
    pub(super) entry: Watched,
    pub(super) previous: u128,
    pub(super) current: u128,
}

/// Reports every balance and returns the native changes nothing explains.
///
/// A deferred address keeps the balance and nonce it was last judged at, so the
/// change is judged whole once the indexer has looked and every send the balance
/// holds has a receipt.
///
/// Receipts and balances are read apart and may come from different nodes, so a
/// send counts against the first balance whose nonce shows it was mined, not
/// against the pass that first saw its receipt.
pub(super) fn balances(
    inputs: &PassInputs<'_>,
    watched: &[Watched],
    sheets: &[Sheet],
    cursors: &mut BTreeMap<i64, ScanCursor>,
    spends: &[Spend],
    explanations: &BTreeMap<i64, Explained>,
) -> Vec<Unexplained> {
    let table = inputs.tokens.all();
    let native_asset = inputs.params.native();
    let mut unexplained = Vec::new();

    for (entry, sheet) in watched.iter().zip(sheets) {
        let cursor = cursors.entry(entry.address_id).or_default();
        if let Some(balance) = sheet.native {
            inputs.events.try_emit(WalletEvent::BalanceChanged(BalanceUpdate {
                address_id: entry.address_id,
                asset: native_asset,
                balance: Amount::from_base_units(balance),
            }));
            let nonce = sheet.nonce;
            let own = match (cursor.native_nonce, nonce) {
                (_, None) => Explained::Deferred,
                // A cursor that kept no nonce has nothing to net the balance
                // against, so it is taken as it stands.
                (None, Some(_)) => Explained::Wholly,
                // A node behind the one the balance was last judged on.
                (Some(judged), Some(now)) if now < judged => Explained::Deferred,
                (Some(judged), Some(now)) => spent_between(spends, entry.address_id, judged..now),
            };
            let explanation =
                explanations.get(&entry.address_id).map_or(own, |indexed| own.plus(*indexed));
            if explanation != Explained::Deferred {
                if let Some(previous) = cursor.native_balance
                    && let Explained::Spent(spent) = explanation
                    && let Some(unspent) = balance.checked_add(spent)
                    && unspent != previous
                {
                    unexplained.push(Unexplained { entry: *entry, previous, current: unspent });
                }
                cursor.native_balance = Some(balance);
                cursor.native_nonce = nonce;
            }
        }
        for (token, slot) in table.iter().zip(sheet.tokens.iter().copied()) {
            let Some(balance) = slot else { continue };
            inputs.events.try_emit(WalletEvent::BalanceChanged(BalanceUpdate {
                address_id: entry.address_id,
                asset: AssetId::Token(token.id()),
                balance: Amount::from_base_units(balance),
            }));
        }
    }
    unexplained
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explanations_add_up_and_a_deferred_one_stays_deferred_whatever_else_explains_it() {
        assert_eq!(Explained::Spent(1).plus(Explained::Spent(2)), Explained::Spent(3));
        assert_eq!(Explained::Spent(u128::MAX).plus(Explained::Spent(1)), Explained::Wholly);
        assert_eq!(Explained::Spent(1).plus(Explained::Wholly), Explained::Wholly);
        for other in [Explained::Spent(5), Explained::Wholly, Explained::Deferred] {
            assert_eq!(Explained::Deferred.plus(other), Explained::Deferred, "{other:?}");
            assert_eq!(other.plus(Explained::Deferred), Explained::Deferred, "{other:?}");
        }
    }

    #[test]
    fn a_balance_is_explained_by_the_sends_its_nonce_newly_holds() {
        let spend = |nonce, spent| Spend { address_id: 7, nonce, spent };
        let spends = [
            spend(4, Some(Explained::Spent(10))),
            // A replacement at nonce 5 landed, and the send it replaced never will.
            spend(5, None),
            spend(5, Some(Explained::Spent(20))),
            spend(6, None),
            Spend { address_id: 8, nonce: 5, spent: Some(Explained::Spent(99)) },
        ];
        assert_eq!(spent_between(&spends, 7, 4..6), Explained::Spent(30));
        assert_eq!(spent_between(&spends, 7, 5..6), Explained::Spent(20), "one send per nonce");
        assert_eq!(
            spent_between(&spends, 7, 4..7),
            Explained::Deferred,
            "a mined send no receipt has answered for"
        );
        assert_eq!(spent_between(&spends, 7, 7..9), Explained::Spent(0), "sent from elsewhere");
        assert_eq!(spent_between(&spends, 7, 6..6), Explained::Spent(0), "nothing newly mined");
    }
}
