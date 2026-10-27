use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::Address;
use async_trait::async_trait;
use jelly_core::{SecretText, wire_enum};
use jelly_net::{HttpEndpoint, HttpRequest, NetError, Outcome, Pool};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use zeroize::Zeroizing;

use crate::error::{EvmError, HistoryRefusal};
use crate::history::{HistoryApi, HistoryPage, InternalRow, NativeRow, TokenRow};
use crate::params::EvmParams;

const TARGET: &str = "jelly::evm::history";

/// Rows per page. The free tier refuses `page * offset` above 10000.
const PAGE_SIZE: u32 = 1_000;

/// Pages per read before the answer is declared truncated.
const MAX_PAGES: u32 = 10;

/// A thousand rows are around 600 KB. The cap stops a hostile or broken service
/// from forcing unbounded allocation.
const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;

/// The Etherscan-family envelope.
///
/// A `status` of `0` is not always a failure. An empty history comes back as an
/// empty array or as a string result with a no-transactions message, and a
/// refused key comes back as a string result.
#[derive(Debug, Deserialize)]
struct Envelope<'a> {
    #[serde(default)]
    status: String,
    #[serde(default)]
    message: String,
    #[serde(borrow, default)]
    result: Option<&'a RawValue>,
}

/// One page of an answer.
#[derive(Debug)]
pub(crate) struct Page<T> {
    /// The rows this build could read.
    pub(crate) rows: Vec<T>,
    /// Every row the service sent, readable or not, which is what says whether
    /// another page follows.
    pub(crate) served: usize,
}

/// Rows are decoded one at a time, so an unreadable row costs that row and a
/// warning rather than the page.
pub(crate) fn read_page<T: DeserializeOwned>(body: &str) -> Result<Page<T>, EvmError> {
    let envelope: Envelope<'_> = serde_json::from_str(body).map_err(|source| {
        EvmError::HistoryShape { line: source.line(), column: source.column() }
    })?;
    let Some(result) = envelope.result else {
        return Err(EvmError::HistoryRefused(HistoryRefusal::classify(&envelope.message)));
    };
    if let Ok(raw_rows) = serde_json::from_str::<Vec<&RawValue>>(result.get()) {
        let served = raw_rows.len();
        let mut rows = Vec::with_capacity(served);
        for raw in raw_rows {
            if let Ok(row) = serde_json::from_str::<T>(raw.get()) {
                rows.push(row);
            }
        }
        let skipped = served.saturating_sub(rows.len());
        if skipped > 0 {
            tracing::warn!(
                target: TARGET,
                skipped,
                "history rows this build cannot read were left out"
            );
        }
        return Ok(Page { rows, served });
    }
    // A string result with an empty-history message is an answer, not a refusal.
    let detail: String = serde_json::from_str(result.get()).map_err(|source| {
        EvmError::HistoryShape { line: source.line(), column: source.column() }
    })?;
    if envelope.status == "0" && envelope.message.to_ascii_lowercase().contains("no transactions") {
        return Ok(Page { rows: Vec::new(), served: 0 });
    }
    Err(EvmError::HistoryRefused(HistoryRefusal::classify(&format!(
        "{} {detail}",
        envelope.message
    ))))
}

impl HistoryRefusal {
    /// Classifies the Etherscan-family `NOTOK` result text, which is read only
    /// here and never kept.
    ///
    /// Key checks come first, because a message that names the key is about the
    /// key even when it also mentions a rate.
    fn classify(detail: &str) -> Self {
        let text = detail.to_ascii_lowercase();
        if text.contains("invalid api key") || text.contains("missing api key") {
            Self::InvalidKey
        } else if text.contains("not supported for this chain") {
            Self::ChainNotCovered
        } else if text.contains("rate limit") || text.contains("too many") {
            Self::RateLimited
        } else if text.contains("invalid") || text.contains("error!") {
            Self::BadQuery
        } else {
            Self::Unclassified
        }
    }
}

wire_enum! {
    /// Every Etherscan action this crate asks for, so each name is spelled only
    /// here.
    enum Action {
        TxList => "txlist",
        TxListInternal => "txlistinternal",
        TokenTx => "tokentx",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageStep {
    /// A short page is the end of the history.
    Done,
    More,
    /// A full page at the page ceiling, so the service has more than it will
    /// give.
    Truncated,
}

fn page_step(rows: usize, page: u32) -> PageStep {
    let is_full = u32::try_from(rows).map_or(true, |count| count >= PAGE_SIZE);
    match (is_full, page >= MAX_PAGES) {
        (false, _) => PageStep::Done,
        (true, true) => PageStep::Truncated,
        (true, false) => PageStep::More,
    }
}

/// An Etherscan-compatible history service.
///
/// The Etherscan v2 multichain service takes a key and serves every chain from one
/// host, selected by `chainid`. A keyless one, such as Routescan's, serves one
/// network per URL and ignores `chainid`.
///
/// The key rides in the query string, so the URL is built here and only the
/// redacted label reaches an error, log or event.
pub struct EtherscanHistory {
    pool: Arc<Pool<HttpEndpoint>>,
    api_key: Option<SecretText>,
    chain_id: u64,
    timeout: Duration,
}

/// The key is never printed.
impl fmt::Debug for EtherscanHistory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EtherscanHistory")
            .field("chain_id", &self.chain_id)
            .field("has_key", &self.api_key.is_some())
            .finish_non_exhaustive()
    }
}

impl EtherscanHistory {
    /// The v2 API refuses every request without a key, so a blank key fails here
    /// as a configuration mistake.
    pub fn new(
        pool: Arc<Pool<HttpEndpoint>>,
        api_key: &str,
        params: &EvmParams,
    ) -> Result<Self, EvmError> {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(EvmError::HistoryKeyMissing);
        }
        Ok(Self {
            pool,
            api_key: Some(SecretText::new(api_key.to_owned())),
            chain_id: params.chain_id,
            timeout: params.request_timeout,
        })
    }

    /// A service that answers without a key, which is sent none.
    pub const fn keyless(pool: Arc<Pool<HttpEndpoint>>, params: &EvmParams) -> Self {
        Self { pool, api_key: None, chain_id: params.chain_id, timeout: params.request_timeout }
    }

    async fn paged<T: DeserializeOwned>(
        &self,
        action: Action,
        address: Address,
        start_block: u64,
    ) -> Result<HistoryPage<T>, EvmError> {
        let mut rows: Vec<T> = Vec::new();
        let mut is_truncated = false;
        let mut page = 1u32;
        loop {
            let body = self.fetch(action, address, start_block, page).await?;
            let found: Page<T> = read_page(&body)?;
            let step = page_step(found.served, page);
            rows.extend(found.rows);
            match step {
                PageStep::Done => break,
                PageStep::Truncated => {
                    is_truncated = true;
                    break;
                }
                PageStep::More => page = page.saturating_add(1),
            }
        }
        Ok(HistoryPage::of(rows, is_truncated))
    }

    async fn fetch(
        &self,
        action: Action,
        address: Address,
        start_block: u64,
        page: u32,
    ) -> Result<String, EvmError> {
        // No `endblock`, which the service treats as optional, because any fixed
        // ceiling is one a fast chain eventually passes.
        let mut query = Zeroizing::new(format!(
            "chainid={}&module=account&action={action}&address={address}&startblock={start_block}\
             &page={page}&offset={PAGE_SIZE}&sort=asc",
            self.chain_id
        ));
        if let Some(api_key) = &self.api_key {
            query.push_str("&apikey=");
            api_key.with_str(|key| query.push_str(key));
        }
        let timeout = self.timeout;
        self.pool
            .call(action.as_str(), classify, move |endpoint| {
                let query = query.clone();
                async move {
                    endpoint
                        .send_query(
                            "",
                            &query,
                            HttpRequest::get(timeout).with_max_bytes(MAX_PAGE_BYTES),
                        )
                        .await?
                        .into_body()
                }
            })
            .await
            .map_err(EvmError::Net)
    }
}

#[async_trait]
impl HistoryApi for EtherscanHistory {
    async fn native(
        &self,
        address: Address,
        start_block: u64,
    ) -> Result<HistoryPage<NativeRow>, EvmError> {
        self.paged(Action::TxList, address, start_block).await
    }

    async fn tokens(
        &self,
        address: Address,
        start_block: u64,
    ) -> Result<HistoryPage<TokenRow>, EvmError> {
        self.paged(Action::TokenTx, address, start_block).await
    }

    async fn internal(
        &self,
        address: Address,
        start_block: u64,
    ) -> Result<HistoryPage<InternalRow>, EvmError> {
        self.paged(Action::TxListInternal, address, start_block).await
    }
}

/// A refused key or a malformed query gets the same answer everywhere, so it
/// stops the walk. Anything else rotates.
const fn classify(error: &NetError) -> Outcome {
    match *error {
        NetError::Status { status: 400 | 401 | 403 | 404, .. } => Outcome::Fail,
        NetError::Endpoint { .. }
        | NetError::Timeout { .. }
        | NetError::Protocol { .. }
        | NetError::TooLarge { .. }
        | NetError::Status { .. }
        | NetError::RateLimited { .. }
        | NetError::Rpc { .. }
        | NetError::WrongNetwork { .. }
        | NetError::NoEndpoints { .. }
        | NetError::NoneReady { .. }
        | NetError::AllFailed { .. }
        | NetError::InvalidUrl { .. }
        | NetError::Defaults { .. }
        | NetError::Config { .. } => Outcome::Rotate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::endpoint;

    const RECORDED: &str = include_str!("../tests/fixtures/etherscan.json");
    const ETH: u128 = 1_000_000_000_000_000_000;
    const OWNER: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
    const USDT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";

    fn body(key: &str) -> String {
        let file: serde_json::Value = serde_json::from_str(RECORDED).expect("the fixture is json");
        file.get(key).expect("the fixture has that key").to_string()
    }

    fn address(text: &str) -> Address {
        text.parse().expect("an address")
    }

    #[test]
    fn only_the_statuses_every_host_would_repeat_stop_the_rotation() {
        let label = endpoint();
        for status in [400, 401, 403, 404] {
            let error = NetError::Status { endpoint: label.clone(), status };
            assert_eq!(classify(&error), Outcome::Fail, "{status}");
        }
        assert_eq!(classify(&NetError::Status { endpoint: label, status: 500 }), Outcome::Rotate);
    }

    #[test]
    fn a_page_shorter_than_the_offset_ends_the_walk_and_a_full_one_at_the_ceiling_truncates() {
        assert_eq!(page_step(999, 1), PageStep::Done);
        assert_eq!(page_step(1_000, 1), PageStep::More);
        assert_eq!(page_step(1_000, 10), PageStep::Truncated);
        assert_eq!(page_step(0, 1), PageStep::Done);
        assert_eq!(
            page_step(usize::MAX, 1),
            PageStep::More,
            "a count wider than the page size is a full page, not an empty one"
        );
    }

    #[test]
    fn a_recorded_native_page_decodes_field_for_field() {
        let rows: Vec<NativeRow> = read_page(&body("txlist")).unwrap().rows;
        assert_eq!(rows.len(), 2);
        let first = rows.first().expect("a first row");
        assert_eq!(first.block_number, 18_000_000);
        assert_eq!(first.timestamp, 1_692_000_000);
        assert_eq!(first.value, ETH);
        assert_eq!(first.gas_used, Some(21_000));
        assert_eq!(first.gas_price, Some(20_000_000_000));
        assert!(!first.is_failed);
        assert_eq!(first.from, address("0x5041ed759Dd4aFc3a72b8192C143F72f4724081A"));
        assert_eq!(first.to, Some(address(OWNER)));

        let failed = rows.get(1).expect("the failed row");
        assert!(failed.is_failed, "the service marks it with isError 1");
        assert_eq!(failed.block_number, 18_000_005);

        let page = HistoryPage::of(rows, true);
        assert_eq!(page.highest_block(), Some(18_000_005), "the highest row the read returned");
        assert!(page.is_truncated());
        assert_eq!(HistoryPage::<NativeRow>::of(Vec::new(), false).highest_block(), None);
    }

    #[test]
    fn a_recorded_token_page_drops_only_what_it_cannot_read() {
        let rows: Vec<TokenRow> = read_page(&body("tokentx")).unwrap().rows;
        assert_eq!(rows.len(), 3, "the row whose value is not a number is dropped");
        let first = rows.first().expect("a first row");
        assert_eq!(first.value, 2_500_000);
        assert_eq!(first.contract_address.to_string(), USDT);
        assert_eq!(first.gas_used, Some(63_000));
    }

    #[test]
    fn a_row_this_build_cannot_read_still_counts_toward_a_full_page() {
        let page = read_page::<TokenRow>(&body("tokentx")).unwrap();
        assert_eq!(page.rows.len(), 3);
        assert_eq!(page.served, 4, "the service sent the unreadable row, so the page is not short");
    }

    #[test]
    fn a_contract_creation_has_no_recipient() {
        let rows: Vec<NativeRow> = read_page(&body("contract_creation")).unwrap().rows;
        assert_eq!(rows.first().and_then(|row| row.to), None);
    }

    #[test]
    fn an_empty_history_is_an_answer_in_either_shape() {
        for key in ["empty", "empty_as_string"] {
            let rows: Vec<NativeRow> = read_page(&body(key)).unwrap().rows;
            assert!(rows.is_empty(), "{key}");
        }
    }

    #[test]
    fn a_refusal_becomes_a_closed_vocabulary_and_keeps_none_of_the_services_text() {
        let refused = read_page::<NativeRow>(&body("bad_key")).expect_err("a refused key");
        assert!(
            matches!(refused, EvmError::HistoryRefused(HistoryRefusal::InvalidKey)),
            "{refused:?}"
        );
        let rendered = format!("{refused} {refused:?}");
        assert!(!rendered.contains("NOTOK"), "{rendered}");
        assert!(!rendered.contains("Invalid API Key"), "{rendered}");

        let limited = read_page::<NativeRow>(&body("rate_limited")).expect_err("a rate limit");
        assert!(matches!(limited, EvmError::HistoryRefused(HistoryRefusal::RateLimited)));
        assert!(
            limited.to_string().contains("rate limit"),
            "a person is told to wait rather than to fix a key: {limited}"
        );
    }

    #[test]
    fn every_history_refusal_wording_classifies_and_says_which_it_is() {
        let recorded = [
            ("Invalid API Key", HistoryRefusal::InvalidKey),
            ("Missing API Key", HistoryRefusal::InvalidKey),
            // Names both the key and the attempts, and the key is what to fix.
            (
                "Too many invalid api key attempts, please try again later",
                HistoryRefusal::InvalidKey,
            ),
            ("Max rate limit reached, please use API Key", HistoryRefusal::RateLimited),
            ("Max calls per sec rate limit reached (5/sec)", HistoryRefusal::RateLimited),
            // Etherscan's documented answer to a free key on a paid-only chain.
            (
                "NOTOK Free API access is not supported for this chain. Please upgrade your api \
                 plan for full chain coverage.",
                HistoryRefusal::ChainNotCovered,
            ),
            ("Error! Invalid address format", HistoryRefusal::BadQuery),
            ("something new", HistoryRefusal::Unclassified),
        ];
        for (detail, expected) in recorded {
            assert_eq!(HistoryRefusal::classify(detail), expected, "{detail}");
        }
        for refusal in [
            HistoryRefusal::InvalidKey,
            HistoryRefusal::ChainNotCovered,
            HistoryRefusal::RateLimited,
            HistoryRefusal::BadQuery,
            HistoryRefusal::Unclassified,
        ] {
            assert!(!refusal.to_string().is_empty(), "{refusal:?} says what happened");
        }
    }

    /// The free plan stopped covering BNB Smart Chain, and that answer is about the
    /// plan and the chain, not a broken key or a busy service.
    #[test]
    fn a_free_key_asked_about_a_paid_chain_is_told_the_plan_does_not_cover_it() {
        let refused =
            read_page::<NativeRow>(&body("free_tier_chain")).expect_err("a chain the plan lacks");
        assert!(
            matches!(refused, EvmError::HistoryRefused(HistoryRefusal::ChainNotCovered)),
            "{refused:?}"
        );
        let rendered = format!("{refused} {refused:?}");
        assert!(!rendered.contains("Free API access"), "{rendered}");
    }

    #[test]
    fn a_body_that_is_not_the_envelope_reports_a_position_and_no_text() {
        let error = read_page::<NativeRow>("<html>502 bad gateway</html>").expect_err("not json");
        assert!(matches!(error, EvmError::HistoryShape { .. }), "{error:?}");
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains("html"), "{rendered}");
        assert!(!rendered.contains("gateway"), "{rendered}");
    }

    #[test]
    fn a_recorded_internal_page_decodes_field_for_field() {
        let rows: Vec<InternalRow> = read_page(&body("txlistinternal")).unwrap().rows;
        assert_eq!(rows.len(), 4);
        let refund = rows.first().expect("the refund");
        let refunded: NativeRow = read_page(&body("txlist_refund"))
            .unwrap()
            .rows
            .into_iter()
            .next()
            .expect("the transaction the refund ran in");
        assert_eq!(refund.hash, refunded.hash, "a call shares its transaction's hash");
        assert_eq!(refund.to, Some(address(OWNER)));
        assert_eq!(refund.value, 300_000_000_000_000_000);
        assert!(!refund.is_context_call && !refund.is_failed);
        let created = rows.get(2).expect("the creation");
        assert_eq!(created.to, None, "a created contract is no named recipient");
        let failed = rows.get(3).expect("the failed call");
        assert!(failed.is_failed, "the service marks it with isError 1");
    }

    #[test]
    fn a_call_in_its_callers_context_moves_no_coin() {
        let rows: Vec<InternalRow> = read_page(&body("context_calls")).unwrap().rows;
        let borrowed: Vec<bool> = rows.iter().map(|row| row.is_context_call).collect();
        assert_eq!(borrowed, vec![true, true, true, false], "{rows:?}");
    }
}
