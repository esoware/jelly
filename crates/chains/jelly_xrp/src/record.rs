use jelly_core::{Amount, AssetId, TxDirection, TxId, TxOutcome, TxRecord};
use serde_json::Value;

use crate::client::parse_drops;
use crate::config::{CHAIN, RIPPLE_EPOCH_UNIX_SECONDS};
use crate::error::XrpError;

enum Change {
    Created,
    Modified,
    Deleted,
}

fn balance_delta(metadata: &Value, owner: &str) -> Result<Option<i128>, XrpError> {
    let mut delta_drops = None;
    let nodes =
        metadata.get("AffectedNodes").and_then(Value::as_array).ok_or(XrpError::Protocol)?;
    for node in nodes {
        let (entry, change) = if let Some(entry) = node.get("ModifiedNode") {
            (entry, Change::Modified)
        } else if let Some(entry) = node.get("CreatedNode") {
            (entry, Change::Created)
        } else if let Some(entry) = node.get("DeletedNode") {
            (entry, Change::Deleted)
        } else {
            return Err(XrpError::Protocol);
        };
        if entry.get("LedgerEntryType").and_then(Value::as_str) != Some("AccountRoot") {
            continue;
        }
        let Some(fields) = entry.get(match change {
            Change::Created => "NewFields",
            Change::Modified | Change::Deleted => "FinalFields",
        }) else {
            // rippled writes no fields for an account the transaction only threads, whose
            // balance is unchanged.
            if matches!(change, Change::Modified) && entry.get("PreviousFields").is_none() {
                continue;
            }
            return Err(XrpError::Protocol);
        };
        if fields.get("Account").and_then(Value::as_str) != Some(owner) {
            continue;
        }
        let final_drops =
            parse_drops(fields.get("Balance").and_then(Value::as_str).ok_or(XrpError::Protocol)?)?;
        let previous_drops = || {
            entry
                .pointer("/PreviousFields/Balance")
                .and_then(Value::as_str)
                .map(parse_drops)
                .transpose()
                .map(|previous| previous.unwrap_or(final_drops))
        };
        let (before_drops, after_drops) = match change {
            Change::Created => (0, final_drops),
            Change::Modified => (previous_drops()?, final_drops),
            Change::Deleted => (previous_drops()?, 0),
        };
        delta_drops = Some(
            i128::from(after_drops)
                .checked_sub(i128::from(before_drops))
                .ok_or(XrpError::Protocol)?,
        );
    }
    Ok(delta_drops)
}

pub(crate) fn record(
    row: &Value,
    address_id: i64,
    owner: &str,
    tip: u32,
) -> Result<Option<TxRecord>, XrpError> {
    if row.get("validated").and_then(Value::as_bool) != Some(true) {
        return Err(XrpError::Protocol);
    }
    let tx = tx_fields(row);
    let metadata = row.get("meta").ok_or(XrpError::Protocol)?;
    let hash = row
        .get("hash")
        .or_else(|| tx.get("hash"))
        .and_then(Value::as_str)
        .ok_or(XrpError::Protocol)?;
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(XrpError::Protocol);
    }
    let sender = tx.get("Account").and_then(Value::as_str).ok_or(XrpError::Protocol)?;
    let is_sent_by_owner = sender == owner;
    let destination = tx.get("Destination").and_then(Value::as_str);
    let fee_drops = parse_drops(tx.get("Fee").and_then(Value::as_str).ok_or(XrpError::Protocol)?)?;
    let outcome =
        if metadata.get("TransactionResult").and_then(Value::as_str).ok_or(XrpError::Protocol)?
            == "tesSUCCESS"
        {
            TxOutcome::Success
        } else {
            TxOutcome::Failed
        };
    // The amount is the owner's balance change, not the payment's `Amount`, because a partial
    // payment can deliver less. A sender's fee is added back so the amount leaves it out.
    let Some(mut delta_drops) = balance_delta(metadata, owner)? else { return Ok(None) };
    if is_sent_by_owner {
        delta_drops = delta_drops.checked_add(i128::from(fee_drops)).ok_or(XrpError::Protocol)?;
    }
    if delta_drops == 0 && !is_sent_by_owner {
        return Ok(None);
    }
    let direction = if is_sent_by_owner && destination == Some(owner) {
        TxDirection::SelfTransfer
    } else if delta_drops > 0 {
        TxDirection::Incoming
    } else {
        TxDirection::Outgoing
    };
    let amount = if outcome == TxOutcome::Failed {
        Amount::ZERO
    } else {
        Amount::from_base_units(delta_drops.unsigned_abs())
    };
    let mut record = TxRecord::builder(
        AssetId::Native(CHAIN),
        address_id,
        TxId::new(CHAIN, hash.to_ascii_uppercase()),
        direction,
        amount,
    )
    .outcome(outcome);
    if is_sent_by_owner {
        record = record.fee(Amount::from(fee_drops));
        if let Some(destination) = destination {
            record = record.counterparty(destination);
        }
    } else {
        record = record.counterparty(sender);
    }
    let height = ledger_index(row)?;
    if height > tip {
        return Err(XrpError::Protocol);
    }
    let unix_secs = tx
        .get("date")
        .and_then(Value::as_i64)
        .and_then(|time| time.checked_add(RIPPLE_EPOCH_UNIX_SECONDS));
    Ok(Some(record.confirmed_at(u64::from(height), unix_secs, u64::from(tip)).build()))
}

/// The transaction fields of an `account_tx` row or a `tx` result.
///
/// Version 1 of `account_tx` nests them under `tx`, version 2 of either method nests them
/// under `tx_json`, and version 1 of `tx` returns them flat. No request pins `api_version`,
/// so a server answers in its default. `hash` and `ledger_index` sit on the row itself,
/// except in a version 1 `account_tx` row, which keeps them among the transaction fields.
fn tx_fields(row: &Value) -> &Value {
    row.get("tx").or_else(|| row.get("tx_json")).unwrap_or(row)
}

pub(crate) fn ledger_index(row: &Value) -> Result<u32, XrpError> {
    let tx = tx_fields(row);
    row.get("ledger_index")
        .or_else(|| tx.get("ledger_index"))
        .and_then(Value::as_u64)
        .and_then(|index| u32::try_from(index).ok())
        .ok_or(XrpError::Protocol)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const OWNER: &str = "rPEPPER7kfTD9w2To4CQk6UCfuHM9c6GDY";

    #[test]
    fn partial_payments_credit_the_ledger_change_not_the_requested_amount() {
        let metadata = json!({"TransactionResult":"tesSUCCESS","delivered_amount":"1","AffectedNodes":[{
            "ModifiedNode":{"LedgerEntryType":"AccountRoot","PreviousFields":{"Balance":"1000000"},
            "FinalFields":{"Account":OWNER,"Balance":"1000001"}}
        }]});
        assert_eq!(balance_delta(&metadata, OWNER).unwrap(), Some(1));
        let row = json!({"validated":true,"hash":"AB".repeat(32),"ledger_index":50000,
            "Account":"rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh","Destination":OWNER,"Fee":"10",
            "Flags":131_072,"Amount":"99999999999999999","meta":metadata});
        let record = record(&row, 1, OWNER, 50000).unwrap().unwrap();
        assert_eq!(record.amount.base_units(), 1);
        assert_eq!(record.direction, TxDirection::Incoming);
        assert_eq!(record.fee, None);
    }

    #[test]
    fn account_creation_and_deletion_use_zero_on_the_missing_side() {
        let created = json!({"AffectedNodes":[{"CreatedNode":{"LedgerEntryType":"AccountRoot",
            "NewFields":{"Account":OWNER,"Balance":"1000000"}}}]});
        let deleted = json!({"AffectedNodes":[{"DeletedNode":{"LedgerEntryType":"AccountRoot",
            "FinalFields":{"Account":OWNER,"Balance":"1000000"}}}]});
        assert_eq!(balance_delta(&created, OWNER).unwrap(), Some(1_000_000));
        assert_eq!(balance_delta(&deleted, OWNER).unwrap(), Some(-1_000_000));
    }

    #[test]
    fn a_threaded_only_account_root_is_skipped_but_other_missing_fields_are_not() {
        let threaded = json!({"ModifiedNode":{"LedgerEntryType":"AccountRoot",
            "LedgerIndex":"BF".repeat(32),"PreviousTxnID":"AB".repeat(32),"PreviousTxnLgrSeq":1}});
        let owner_node = json!({"ModifiedNode":{"LedgerEntryType":"AccountRoot",
            "PreviousFields":{"Balance":"1000000"},"FinalFields":{"Account":OWNER,"Balance":"999990"}}});
        let metadata = json!({"AffectedNodes":[owner_node, threaded]});
        assert_eq!(balance_delta(&metadata, OWNER).unwrap(), Some(-10));
        let only_threaded = json!({"AffectedNodes":[threaded]});
        assert_eq!(balance_delta(&only_threaded, OWNER).unwrap(), None);

        let without_final = json!({"AffectedNodes":[{"ModifiedNode":{"LedgerEntryType":"AccountRoot",
            "PreviousFields":{"Balance":"1000000"}}}]});
        let created_bare =
            json!({"AffectedNodes":[{"CreatedNode":{"LedgerEntryType":"AccountRoot"}}]});
        let deleted_bare =
            json!({"AffectedNodes":[{"DeletedNode":{"LedgerEntryType":"AccountRoot"}}]});
        for metadata in [without_final, created_bare, deleted_bare] {
            assert!(matches!(balance_delta(&metadata, OWNER), Err(XrpError::Protocol)));
        }
    }

    #[test]
    fn a_trust_set_that_threads_its_issuer_records_the_owners_fee() {
        let row = json!({"validated":true,"hash":"63".repeat(32),"ledger_index":40_010_838,
        "Account":OWNER,"TransactionType":"TrustSet","Fee":"12","meta":{
        "TransactionResult":"tesSUCCESS","AffectedNodes":[
            {"ModifiedNode":{"LedgerEntryType":"AccountRoot",
                "PreviousFields":{"Balance":"20000000","OwnerCount":0},
                "FinalFields":{"Account":OWNER,"Balance":"19999988","OwnerCount":1}}},
            {"ModifiedNode":{"LedgerEntryType":"AccountRoot","LedgerIndex":"BF".repeat(32),
                "PreviousTxnID":"AB".repeat(32),"PreviousTxnLgrSeq":40_000_000}},
        ]}});
        let record = record(&row, 1, OWNER, 40_010_838).unwrap().unwrap();
        assert_eq!(record.amount, Amount::ZERO);
        assert_eq!(record.fee, Some(Amount::from(12u64)));
    }
}
