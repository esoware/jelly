use std::sync::Arc;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::config::MAX_DROPS;
use crate::error::XrpError;
use crate::method::Method;
use crate::node::{XrpNode, covers_range};

// AccountRoot flags, as the XRP Ledger defines them.
const LSF_REQUIRE_DEST_TAG: u32 = 0x0002_0000;
const LSF_DISABLE_MASTER: u32 = 0x0010_0000;
const LSF_DEPOSIT_AUTH: u32 = 0x0100_0000;
// The network's reference transaction cost, which no fee goes below.
const REFERENCE_FEE_DROPS: u64 = 10;

/// What a payment costs on the network right now, as rippled's `fee` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NetworkFee {
    /// The cost of the current open ledger, which the normal level pays.
    pub(crate) normal_drops: u64,
    /// The least a payment may pay and still be queued for a later ledger.
    pub(crate) minimum_drops: u64,
}
// rippled refuses to delete an account whose Sequence is within this many ledgers of the
// current one, so a recreated account cannot replay its old transactions.
const DELETION_SEQUENCE_GAP: u32 = 256;
// rippled's largest `account_objects` page. An account owning more than 1000 objects
// cannot be deleted at all, so three pages without the end in sight mean it cannot be.
const OBJECTS_PAGE_LIMIT: u32 = 400;
const MAX_OBJECT_PAGES: usize = 3;
const HISTORY_PAGE_LIMIT: u32 = 200;

#[derive(Debug)]
pub(crate) struct XrpClient {
    node: Arc<dyn XrpNode>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Ledger {
    pub(crate) seq: u32,
    hash: String,
    pub(crate) reserve_base: u64,
    pub(crate) reserve_inc: u64,
    pub(crate) close_time: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct AccountRoot {
    account: String,
    balance: String,
    pub(crate) sequence: u32,
    owner_count: u32,
    flags: u32,
    /// Present only on an account that has minted NFTs.
    #[serde(rename = "FirstNFTokenSequence")]
    first_nftoken_sequence: Option<u32>,
    #[serde(rename = "MintedNFTokens")]
    minted_nftokens: Option<u32>,
}

impl AccountRoot {
    pub(crate) const fn has_disabled_master_key(&self) -> bool {
        self.flags & LSF_DISABLE_MASTER != 0
    }

    pub(crate) const fn is_destination_tag_required(&self) -> bool {
        self.flags & LSF_REQUIRE_DEST_TAG != 0
    }

    pub(crate) const fn is_deposit_auth_required(&self) -> bool {
        self.flags & LSF_DEPOSIT_AUTH != 0
    }

    /// Whether rippled's `AccountDelete` could refuse this account with `tecTOO_SOON`.
    ///
    /// It refuses while the account's Sequence, or its first NFT sequence plus the NFTs it
    /// minted, is within [`DELETION_SEQUENCE_GAP`] ledgers of the ledger it is applied in.
    /// That ledger follows the validated `ledger`, so the gap is measured from `ledger`.
    pub(crate) fn is_too_new_to_delete(&self, ledger: &Ledger) -> bool {
        let nft_sequence = self
            .first_nftoken_sequence
            .unwrap_or(0)
            .saturating_add(self.minted_nftokens.unwrap_or(0));
        self.sequence.max(nft_sequence).saturating_add(DELETION_SEQUENCE_GAP) >= ledger.seq
    }

    pub(crate) fn balance_drops(&self) -> Result<u64, XrpError> {
        parse_drops(&self.balance)
    }

    pub(crate) fn reserve_drops(&self, ledger: &Ledger) -> Result<u64, XrpError> {
        ledger
            .reserve_inc
            .checked_mul(u64::from(self.owner_count))
            .and_then(|increment| ledger.reserve_base.checked_add(increment))
            .ok_or(XrpError::Protocol)
    }
}

/// One `account_tx` page, its rows oldest first.
#[derive(Debug)]
pub(crate) struct HistoryPage {
    pub(crate) rows: Vec<Value>,
    /// Where the next page of the same range starts, or `None` once the range is read.
    pub(crate) marker: Option<Value>,
}

/// What a `tx` lookup found.
#[derive(Debug)]
pub(crate) enum TxLookup {
    /// The transaction, in a validated ledger.
    Validated(Value),
    /// `txnNotFound`. `has_searched_all` is whether the server held every ledger it was
    /// asked to look in.
    NotFound { has_searched_all: bool },
    /// Any other answer, such as the transaction in a ledger not yet validated.
    Unvalidated,
}

/// How the network took a submitted payment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Submission {
    Accepted,
    Unresolved,
}

impl Submission {
    fn classify(engine_result: &str) -> Self {
        // These results mean the network took the payment. tesSUCCESS and tec results were
        // applied to the open ledger, a tec one charging only the fee, terQUEUED waits in the
        // queue, and tefALREADY means the same payment is already applied or queued. Any other
        // tel, ter, or tef result is provisional, so the send stays tracked as unresolved until
        // a validated ledger or the expiry proof settles it.
        if matches!(engine_result, "tesSUCCESS" | "terQUEUED" | "tefALREADY")
            || engine_result.starts_with("tec")
        {
            Self::Accepted
        } else {
            Self::Unresolved
        }
    }
}

pub(crate) fn parse_drops(text: &str) -> Result<u64, XrpError> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(XrpError::Protocol);
    }
    let amount = text.parse::<u64>().map_err(|_| XrpError::Protocol)?;
    if amount > MAX_DROPS {
        return Err(XrpError::Protocol);
    }
    Ok(amount)
}

fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, XrpError> {
    T::deserialize(value).map_err(|_| XrpError::Protocol)
}

fn drops_at(result: &Value, pointer: &str) -> Result<u64, XrpError> {
    parse_drops(result.pointer(pointer).and_then(Value::as_str).ok_or(XrpError::Protocol)?)
}

impl XrpClient {
    pub(crate) fn new(node: Arc<dyn XrpNode>) -> Self {
        Self { node }
    }

    pub(crate) async fn ledger(&self) -> Result<Ledger, XrpError> {
        let result = self.node.call(Method::ServerState, json!({})).await?;
        let ledger: Ledger =
            decode(result.pointer("/state/validated_ledger").ok_or(XrpError::Protocol)?)?;
        if ledger.seq == 0
            || ledger.reserve_base == 0
            || ledger.reserve_base > MAX_DROPS
            || ledger.reserve_inc > MAX_DROPS
            || ledger.hash.len() != 64
        {
            return Err(XrpError::Protocol);
        }
        Ok(ledger)
    }

    pub(crate) async fn account(
        &self,
        address: &str,
        ledger: &Ledger,
    ) -> Result<Option<AccountRoot>, XrpError> {
        let result = self
            .node
            .call(
                Method::AccountInfo,
                json!({"account": address, "ledger_hash": ledger.hash, "strict": true}),
            )
            .await?;
        if result.get("error").and_then(Value::as_str) == Some("actNotFound") {
            return Ok(None);
        }
        if result.get("validated").and_then(Value::as_bool) != Some(true) {
            return Err(XrpError::Protocol);
        }
        let account: AccountRoot = decode(result.get("account_data").ok_or(XrpError::Protocol)?)?;
        if account.account != address
            || account.sequence == 0
            || result.get("ledger_index").and_then(Value::as_u64) != Some(u64::from(ledger.seq))
        {
            return Err(XrpError::Protocol);
        }
        Ok(Some(account))
    }

    /// Whether `recipient`, an account that requires deposit authorization, accepts
    /// deposits from `from`.
    pub(crate) async fn is_deposit_authorized(
        &self,
        from: &str,
        recipient: &AccountRoot,
        ledger: &Ledger,
    ) -> Result<bool, XrpError> {
        let params = json!({
            "source_account": from, "destination_account": recipient.account,
            "ledger_hash": ledger.hash,
        });
        let authorization = self.node.call(Method::DepositAuthorized, params).await?;
        Ok(authorization.get("deposit_authorized").and_then(Value::as_bool) == Some(true))
    }

    /// Whether `address` owns any ledger object that blocks its deletion, such as a trust
    /// line, an escrow, a payment channel, or a check.
    pub(crate) async fn has_deletion_blockers(
        &self,
        address: &str,
        ledger: &Ledger,
    ) -> Result<bool, XrpError> {
        let mut marker: Option<Value> = None;
        for _ in 0..MAX_OBJECT_PAGES {
            let mut params = json!({
                "account": address, "ledger_hash": ledger.hash,
                "deletion_blockers_only": true, "limit": OBJECTS_PAGE_LIMIT,
            });
            if let Some(marker) = marker.take()
                && let Some(object) = params.as_object_mut()
            {
                object.insert(String::from("marker"), marker);
            }
            let mut result = self.node.call(Method::AccountObjects, params).await?;
            if result.get("validated").and_then(Value::as_bool) != Some(true)
                || result.get("account").and_then(Value::as_str) != Some(address)
            {
                return Err(XrpError::Protocol);
            }
            let objects = result
                .get("account_objects")
                .and_then(Value::as_array)
                .ok_or(XrpError::Protocol)?;
            if !objects.is_empty() {
                return Ok(true);
            }
            marker = result.get_mut("marker").map(Value::take).filter(|value| !value.is_null());
            if marker.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) async fn network_fee(&self) -> Result<NetworkFee, XrpError> {
        let result = self.node.call(Method::Fee, json!({})).await?;
        let minimum_drops = drops_at(&result, "/drops/minimum_fee")?.max(REFERENCE_FEE_DROPS);
        let normal_drops = drops_at(&result, "/drops/open_ledger_fee")?.max(minimum_drops);
        Ok(NetworkFee { normal_drops, minimum_drops })
    }

    /// A validated page of `address`'s history from a server holding all of
    /// `first_ledger..=last_ledger`, resumed from `marker`.
    pub(crate) async fn history(
        &self,
        address: &str,
        first_ledger: u32,
        last_ledger: u32,
        marker: Option<&Value>,
    ) -> Result<HistoryPage, XrpError> {
        let mut params = json!({
            "account": address, "ledger_index_min": first_ledger,
            "ledger_index_max": last_ledger, "forward": true, "binary": false,
            "limit": HISTORY_PAGE_LIMIT,
        });
        if let Some(marker) = marker
            && let Some(object) = params.as_object_mut()
        {
            object.insert(String::from("marker"), marker.clone());
        }
        let mut result = self.node.call(Method::AccountTx, params).await?;
        if !covers_range(&result, u64::from(first_ledger), u64::from(last_ledger))
            || result.get("account").and_then(Value::as_str) != Some(address)
        {
            return Err(XrpError::Protocol);
        }
        let Some(Value::Array(rows)) = result.get_mut("transactions").map(Value::take) else {
            return Err(XrpError::Protocol);
        };
        let marker = result.get_mut("marker").map(Value::take).filter(|value| !value.is_null());
        Ok(HistoryPage { rows, marker })
    }

    /// The transaction `hash`, looked for only in the ledgers that could hold it, so a
    /// `txnNotFound` answer's `searched_all` covers exactly those.
    pub(crate) async fn tx(
        &self,
        hash: &str,
        first_ledger: u32,
        last_ledger: u32,
    ) -> Result<TxLookup, XrpError> {
        let params = json!({
            "transaction": hash,
            "binary": false,
            "min_ledger": first_ledger,
            "max_ledger": last_ledger,
        });
        let result = self.node.call(Method::Tx, params).await?;
        if result.get("validated").and_then(Value::as_bool) == Some(true) {
            if result
                .get("hash")
                .and_then(Value::as_str)
                .is_none_or(|found| !found.eq_ignore_ascii_case(hash))
            {
                return Err(XrpError::Protocol);
            }
            return Ok(TxLookup::Validated(result));
        }
        if result.get("error").and_then(Value::as_str) == Some("txnNotFound") {
            let has_searched_all =
                result.get("searched_all").and_then(Value::as_bool) == Some(true);
            return Ok(TxLookup::NotFound { has_searched_all });
        }
        Ok(TxLookup::Unvalidated)
    }

    /// Submits the signed `blob` once.
    pub(crate) async fn submit(&self, blob: &str) -> Result<Submission, XrpError> {
        let result = self.node.broadcast(blob).await?;
        let engine_result =
            result.get("engine_result").and_then(Value::as_str).ok_or(XrpError::Protocol)?;
        Ok(Submission::classify(engine_result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> Ledger {
        Ledger {
            seq: 90_000_000,
            hash: "AB".repeat(32),
            reserve_base: 1_000_000,
            reserve_inc: 200_000,
            close_time: 0,
        }
    }

    fn account(fields: &Value) -> AccountRoot {
        let mut root = json!({"Account":"rfxdLwsZnoespnTDDb1Xhvbc8EFNdztaoq","Balance":"10000000",
            "Sequence":7,"OwnerCount":0,"Flags":0});
        if let (Some(root), Some(fields)) = (root.as_object_mut(), fields.as_object()) {
            root.extend(fields.clone());
        }
        decode(&root).unwrap()
    }

    /// rippled's `AccountDelete::preclaim` answers `tecTOO_SOON` while the Sequence, or
    /// `FirstNFTokenSequence` plus `MintedNFTokens`, is within 255 of the open ledger.
    #[test]
    fn an_account_is_too_new_to_delete_near_its_sequence_or_the_nfts_it_minted() {
        assert!(!account(&json!({})).is_too_new_to_delete(&ledger()));
        assert!(account(&json!({"Sequence": 89_999_744})).is_too_new_to_delete(&ledger()));
        assert!(!account(&json!({"Sequence": 89_999_743})).is_too_new_to_delete(&ledger()));
        let minter = json!({"FirstNFTokenSequence": 89_999_000, "MintedNFTokens": 744});
        assert!(account(&minter).is_too_new_to_delete(&ledger()));
        let older = json!({"FirstNFTokenSequence": 89_999_000, "MintedNFTokens": 743});
        assert!(!account(&older).is_too_new_to_delete(&ledger()));
    }

    #[test]
    fn a_submission_applied_queued_or_already_held_is_accepted_and_any_other_is_unresolved() {
        for engine_result in ["tesSUCCESS", "tecHAS_OBLIGATIONS", "terQUEUED", "tefALREADY"] {
            assert_eq!(
                Submission::classify(engine_result),
                Submission::Accepted,
                "{engine_result}"
            );
        }
        for engine_result in ["telINSUF_FEE_P", "terPRE_SEQ", "tefPAST_SEQ"] {
            assert_eq!(
                Submission::classify(engine_result),
                Submission::Unresolved,
                "{engine_result}"
            );
        }
    }
}
