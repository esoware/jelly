use std::collections::BTreeMap;

use jelly_core::{
    Amount, AssetId, ChainId, CoreError, TxDetection, TxDirection, TxOutcome, confirmations,
};
use jelly_store::{PageCursor, TxFilter, TxGroup, TxGroupFilter, TxMember, TxRow};
use serde::{Deserialize, Serialize};

use crate::WalletEngine;

/// One page request against the wallet's history.
///
/// A request that names an address lists that address's own share of each transaction
/// it took part in. One that names none lists each transaction once, as the wallet's
/// addresses together saw it.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxQuery {
    /// `None` lists every chain the wallet has switched on.
    pub chain: Option<ChainId>,
    pub asset: Option<AssetId>,
    pub address_id: Option<i64>,
    pub txid: Option<String>,
    pub direction: Option<TxDirection>,
    pub cursor: Option<String>,
    /// Zero means the shipped page size.
    pub limit: u32,
}

/// One transaction of one asset in the history.
///
/// An outgoing amount is what reached others, without the fee. An incoming one is what
/// reached the wallet's addresses, and a self-transfer's is what moved between them, or
/// what came back when it all returned to one address.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TxEntry {
    pub txid: String,
    pub asset: AssetId,
    /// The wallet's addresses whose own share of the transaction is outgoing or a
    /// self-transfer.
    pub sender_ids: Vec<i64>,
    /// The wallet's addresses whose own share of it is incoming.
    pub receiver_ids: Vec<i64>,
    pub direction: TxDirection,
    pub outcome: TxOutcome,
    pub amount: Amount,
    pub fee: Option<Amount>,
    /// What the wallet's addresses paid besides the fee to open an account the transfer
    /// needed, such as a recipient's new token account, in the native asset. `None` when
    /// they paid none.
    pub rent: Option<Amount>,
    /// The other side, or for a self-transfer the address it went to, when known.
    pub counterparty: Option<String>,
    /// The unconfirmed transaction this one pays the fee of, for a speed-up that
    /// spends that transaction's change and moves nothing else.
    pub pays_for: Option<String>,
    pub height: Option<u64>,
    /// Unix seconds of the block, or of the first sighting while unmined.
    pub timestamp: Option<i64>,
    /// Derived from the current tip.
    pub confirmations: u32,
    pub detection: TxDetection,
}

/// One page of history.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TxPage {
    pub rows: Vec<TxEntry>,
    pub next_cursor: Option<String>,
}

impl WalletEngine {
    pub async fn list_txs(&self, query: TxQuery) -> Result<TxPage, CoreError> {
        let session = self.require_session().await?;
        let cursor = query
            .cursor
            .as_deref()
            .map(|token| {
                PageCursor::parse(token).ok_or_else(|| {
                    CoreError::invalid_input(
                        "cursor",
                        "Could not read this history page. Reload the list.",
                    )
                })
            })
            .transpose()?;
        let limit = if query.limit == 0 { TxFilter::DEFAULT_LIMIT } else { query.limit };
        let wallet_id = session.wallet_id();
        let (entries, next_cursor) = if let Some(address_id) = query.address_id {
            let filter = TxFilter {
                wallet_id,
                chain: query.chain,
                asset: query.asset,
                address_id: Some(address_id),
                txid: query.txid,
                direction: query.direction,
                cursor,
                limit,
            };
            let page = self.inner.store.list_txs(filter).await?;
            let entries: Vec<Entry> = page.rows.into_iter().map(Entry::Row).collect();
            (entries, page.next_cursor)
        } else {
            let filter = TxGroupFilter {
                wallet_id,
                chain: query.chain,
                asset: query.asset,
                txid: query.txid,
                direction: query.direction,
                cursor,
                limit,
            };
            let page = self.inner.store.list_tx_groups(filter).await?;
            let entries: Vec<Entry> = page.groups.into_iter().map(Entry::Group).collect();
            (entries, page.next_cursor)
        };
        let mut tips = self.inner.chains.status.tips();
        // The session is published before its adapters are built, and the board has
        // no heights until then, so a read in that gap counts from the stored ones.
        let is_mined_without_tip = entries.iter().any(|entry| {
            entry.lead().is_some_and(|row| row.height.is_some() && !tips.contains_key(&row.chain))
        });
        if is_mined_without_tip {
            for state in self.inner.store.list_chain_states(wallet_id).await? {
                if let Some(height) = state.synced_height {
                    tips.entry(state.chain).or_insert(height);
                }
            }
        }
        Ok(TxPage {
            rows: entries.into_iter().filter_map(|entry| entry.into_entry(&tips)).collect(),
            next_cursor: next_cursor.map(PageCursor::token),
        })
    }
}

/// A page item before the tip is applied.
enum Entry {
    Row(TxRow),
    Group(TxGroup),
}

impl Entry {
    fn lead(&self) -> Option<&TxRow> {
        match *self {
            Self::Row(ref row) => Some(row),
            Self::Group(ref group) => group.members.first().map(|member| &member.row),
        }
    }

    fn into_entry(self, tips: &BTreeMap<ChainId, u64>) -> Option<TxEntry> {
        match self {
            Self::Row(row) => Some(entry_of_row(row, tips)),
            Self::Group(group) => entry_of_group(group, tips),
        }
    }
}

/// One address's own row as an entry.
fn entry_of_row(row: TxRow, tips: &BTreeMap<ChainId, u64>) -> TxEntry {
    let (sender_ids, receiver_ids) = match row.direction {
        TxDirection::Incoming => (Vec::new(), vec![row.address_id]),
        TxDirection::Outgoing | TxDirection::SelfTransfer => (vec![row.address_id], Vec::new()),
    };
    let confirmations = depth(row.chain, row.height, tips);
    TxEntry {
        txid: row.txid,
        asset: row.asset,
        sender_ids,
        receiver_ids,
        direction: row.direction,
        outcome: row.outcome,
        amount: row.amount,
        fee: row.fee,
        rent: row.rent.filter(|rent| !rent.is_zero()),
        counterparty: row.counterparty,
        pays_for: row.pays_for,
        height: row.height,
        timestamp: row.timestamp_unix_secs,
        confirmations,
        detection: row.detection,
    }
}

/// A transaction as the wallet's rows of it add up.
///
/// Every row that spent records what it sent away and every row that only received
/// records what arrived, so what reached others is the first total less the second.
fn entry_of_group(group: TxGroup, tips: &BTreeMap<ChainId, u64>) -> Option<TxEntry> {
    let TxGroup { direction, members } = group;
    let lead = &members.first()?.row;
    let is = |member: &&TxMember, wanted: TxDirection| member.row.direction == wanted;
    let total = |wanted: TxDirection| {
        members
            .iter()
            .filter(|member| is(member, wanted))
            .fold(Amount::ZERO, |sum, member| sum.saturating_add(member.row.amount))
    };
    let sent = total(TxDirection::Outgoing);
    let arrived = total(TxDirection::Incoming);
    let returned = total(TxDirection::SelfTransfer);
    let amount = match direction {
        TxDirection::SelfTransfer if arrived.is_zero() => sent.saturating_add(returned),
        TxDirection::Outgoing => sent.saturating_sub(arrived),
        TxDirection::Incoming | TxDirection::SelfTransfer => arrived,
    };
    let first_party = |wanted: TxDirection, is_own: Option<bool>| {
        members
            .iter()
            .filter(|member| is(member, wanted))
            .filter(|member| is_own.is_none_or(|own| member.is_counterparty_own == own))
            .find_map(|member| member.row.counterparty.clone())
    };
    let counterparty = match direction {
        TxDirection::Incoming => first_party(TxDirection::Incoming, None),
        TxDirection::Outgoing => first_party(TxDirection::Outgoing, Some(false)),
        TxDirection::SelfTransfer => first_party(TxDirection::Outgoing, Some(true)),
    };
    let ids = |keep: fn(TxDirection) -> bool| -> Vec<i64> {
        members
            .iter()
            .filter(|member| keep(member.row.direction))
            .map(|member| member.row.address_id)
            .collect()
    };
    let has = |wanted: TxOutcome| members.iter().any(|member| member.row.outcome == wanted);
    let outcome = if has(TxOutcome::Success) {
        TxOutcome::Success
    } else if has(TxOutcome::Unknown) {
        TxOutcome::Unknown
    } else if has(TxOutcome::Failed) {
        TxOutcome::Failed
    } else {
        TxOutcome::Replaced
    };
    let detection = if members.iter().any(|member| member.row.detection == TxDetection::Full) {
        TxDetection::Full
    } else {
        TxDetection::BalanceDelta
    };
    let height = members.iter().filter_map(|member| member.row.height).max();
    let timestamp = members.iter().filter_map(|member| member.row.timestamp_unix_secs).min();
    let fee = members.iter().filter_map(|member| member.row.fee).max();
    // Each row's rent is what its own address paid, so they add up.
    let rent = members
        .iter()
        .filter_map(|member| member.row.rent)
        .fold(Amount::ZERO, Amount::saturating_add);
    let pays_for = members.iter().find_map(|member| member.row.pays_for.clone());
    Some(TxEntry {
        txid: lead.txid.clone(),
        asset: lead.asset,
        sender_ids: ids(|direction| {
            matches!(direction, TxDirection::Outgoing | TxDirection::SelfTransfer)
        }),
        receiver_ids: ids(|direction| direction == TxDirection::Incoming),
        direction,
        outcome,
        amount,
        fee,
        rent: (!rent.is_zero()).then_some(rent),
        counterparty,
        pays_for,
        height,
        timestamp,
        confirmations: depth(lead.chain, height, tips),
        detection,
    })
}

/// The confirmation count the cached tip implies for a row mined at `height`.
fn depth(chain: ChainId, height: Option<u64>, tips: &BTreeMap<ChainId, u64>) -> u32 {
    let tip = height.map(|height| tips.get(&chain).copied().unwrap_or(height));
    tip.map_or(0, |tip| confirmations(height, tip))
}

#[cfg(test)]
mod tests {
    use jelly_core::TokenId;

    use super::*;

    fn row(address_id: i64, direction: TxDirection, rent: Option<u128>) -> TxRow {
        TxRow {
            id: address_id,
            address_id,
            chain: ChainId::Solana,
            asset: AssetId::Token(TokenId::UsdtSpl),
            txid: String::from("opening"),
            direction,
            outcome: TxOutcome::Success,
            detection: TxDetection::Full,
            amount: Amount::from_base_units(10),
            fee: None,
            rent: rent.map(Amount::from_base_units),
            counterparty: None,
            pays_for: None,
            height: None,
            timestamp_unix_secs: None,
        }
    }

    /// A walk records the rent of every token movement, so most are zero.
    #[test]
    fn a_rent_of_nothing_is_no_rent_and_the_rents_of_a_group_add_up() {
        let tips = BTreeMap::new();
        let rent_of_row =
            |rent: Option<u128>| entry_of_row(row(1, TxDirection::Outgoing, rent), &tips).rent;
        assert_eq!(rent_of_row(None), None);
        assert_eq!(rent_of_row(Some(0)), None);
        assert_eq!(rent_of_row(Some(7)), Some(Amount::from_base_units(7)));

        let rent_of_group = |rents: [Option<u128>; 2]| {
            let members = rents
                .into_iter()
                .zip([1, 2])
                .map(|(rent, address_id)| TxMember {
                    row: row(address_id, TxDirection::Outgoing, rent),
                    is_counterparty_own: false,
                })
                .collect();
            let group = TxGroup { direction: TxDirection::Outgoing, members };
            entry_of_group(group, &tips)?.rent
        };
        assert_eq!(rent_of_group([Some(0), None]), None);
        assert_eq!(rent_of_group([Some(3), Some(4)]), Some(Amount::from_base_units(7)));
    }

    fn entry() -> TxEntry {
        TxEntry {
            txid: String::from("abcd"),
            asset: AssetId::Native(ChainId::Bitcoin),
            sender_ids: vec![7, 8],
            receiver_ids: Vec::new(),
            direction: TxDirection::Outgoing,
            outcome: TxOutcome::Success,
            amount: Amount::MAX,
            fee: Some(Amount::from_base_units(21_000)),
            rent: None,
            counterparty: Some(String::from("bc1qother")),
            pays_for: None,
            height: Some(100),
            timestamp: Some(1_700_000_000),
            confirmations: 1,
            detection: TxDetection::Full,
        }
    }

    #[test]
    fn an_entry_is_camel_case_and_carries_amounts_as_decimal_strings() {
        let value = serde_json::to_value(entry()).unwrap();
        let mut members: Vec<&str> =
            value.as_object().unwrap().keys().map(String::as_str).collect();
        members.sort_unstable();
        assert_eq!(
            members,
            [
                "amount",
                "asset",
                "confirmations",
                "counterparty",
                "detection",
                "direction",
                "fee",
                "height",
                "outcome",
                "paysFor",
                "receiverIds",
                "rent",
                "senderIds",
                "timestamp",
                "txid",
            ]
        );
        assert_eq!(value["amount"], "340282366920938463463374607431768211455");
        assert_eq!(value["fee"], "21000");
        assert_eq!(value["senderIds"], serde_json::json!([7, 8]));
    }
}
