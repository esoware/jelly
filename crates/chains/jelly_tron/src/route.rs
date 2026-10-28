//! TRON HTTP routes and request bodies. `TronNode` returns an unparsed `Answered`
//! body, so real and scripted nodes exercise the same decoders.

use jelly_core::wire_enum;
use serde::Serialize;

use crate::abi;
use crate::address;
use crate::cursor::Stream;
use crate::error::TronError;

wire_enum! {
    /// The operations we perform, spelled as the pool records them and as logs
    /// and errors name them.
    pub enum Route {
        NowBlock => "getblock",
        Account => "getaccount",
        AccountResource => "getaccountresource",
        ChainParameters => "getchainparameters",
        ConstantContract => "triggerconstantcontract",
        TransactionById => "gettransactionbyid",
        TransactionInfoById => "gettransactioninfobyid",
        BroadcastHex => "broadcasthex",
        BroadcastTransaction => "broadcasttransaction",
        NativeHistory => "transactions",
        TokenHistory => "transactions_trc20",
        InternalHistory => "internal_transactions",
    }
}

impl Route {
    /// True for the `/v1` routes, which only an indexer serves. A plain full node
    /// answers 404 for them.
    pub const fn is_indexer(self) -> bool {
        matches!(self, Self::NativeHistory | Self::TokenHistory | Self::InternalHistory)
    }

    /// A history page carries whole transactions and may be large. Everything
    /// else is a handful of fields, and a hostile or broken endpoint shouldn't
    /// force a bigger allocation.
    pub(crate) const fn max_response_bytes(self) -> usize {
        if self.is_indexer() { 4_194_304 } else { 262_144 }
    }
}

/// The most rows a `/v1` history page holds.
const MAX_PAGE_ROWS: u32 = 200;

/// The rows a history walk asks for per page.
const HISTORY_PAGE_ROWS: u32 = 50;

/// One request against one endpoint.
///
/// Path and query stay separate because an endpoint's URL may carry a query
/// holding an API key. `jelly_net` merges the two, whereas folding the arguments
/// into the path would overwrite that key or produce two query strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    route: Route,
    path: String,
    query: String,
    body: Option<String>,
}

impl Request {
    pub const fn route(&self) -> Route {
        self.route
    }

    /// The path to append to an endpoint's own address. Never a whole URL.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The query to merge into whatever the endpoint carries. Empty for the
    /// `/wallet` routes, which take their arguments in the body.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The JSON body, or `None` for a GET.
    pub fn body(&self) -> Option<&str> {
        self.body.as_deref()
    }

    fn post(route: Route, path: &'static str, body: &impl Serialize) -> Result<Self, TronError> {
        let body = serde_json::to_string(body)
            .map_err(|_| TronError::Encode { what: "a request body" })?;
        Ok(Self { route, path: path.to_owned(), query: String::new(), body: Some(body) })
    }

    /// `getnowblock` carries every transaction in the block, about half a
    /// megabyte on mainnet, so the header is asked for alone. With no
    /// `id_or_num`, `getblock` answers the head block.
    pub(crate) fn now_block() -> Result<Self, TronError> {
        Self::post(Route::NowBlock, "/wallet/getblock", &HeaderOnly { detail: false })
    }

    pub(crate) fn chain_parameters() -> Result<Self, TronError> {
        Self::post(Route::ChainParameters, "/wallet/getchainparameters", &serde_json::Map::new())
    }

    pub(crate) fn account(address: &str) -> Result<Self, TronError> {
        Self::post(Route::Account, "/wallet/getaccount", &AddressBody { address, visible: true })
    }

    pub(crate) fn account_resource(address: &str) -> Result<Self, TronError> {
        Self::post(
            Route::AccountResource,
            "/wallet/getaccountresource",
            &AddressBody { address, visible: true },
        )
    }

    /// A read-only call. `parameter` is the argument blob with no selector,
    /// because TRON takes the signature separately.
    fn constant_call(
        owner: &str,
        contract: &str,
        signature: &'static str,
        parameter: String,
    ) -> Result<Self, TronError> {
        Self::post(
            Route::ConstantContract,
            "/wallet/triggerconstantcontract",
            &ConstantCallBody {
                owner_address: owner,
                contract_address: contract,
                function_selector: signature,
                parameter,
                visible: true,
            },
        )
    }

    pub(crate) fn balance_of(
        owner: &str,
        owner_body: &[u8; address::BODY_BYTES],
        contract: &str,
    ) -> Result<Self, TronError> {
        Self::constant_call(
            owner,
            contract,
            abi::BALANCE_OF_SIGNATURE,
            abi::balance_of_args(owner_body),
        )
    }

    pub(crate) fn measure_transfer(
        owner: &str,
        contract: &str,
        to: &[u8; address::BODY_BYTES],
        amount: u128,
    ) -> Result<Self, TronError> {
        Self::constant_call(
            owner,
            contract,
            abi::TRANSFER_SIGNATURE,
            abi::transfer_args(to, amount),
        )
    }

    pub(crate) fn transaction_by_id(txid: &str) -> Result<Self, TronError> {
        Self::post(Route::TransactionById, "/wallet/gettransactionbyid", &ById { value: txid })
    }

    pub(crate) fn transaction_info_by_id(txid: &str) -> Result<Self, TronError> {
        Self::post(
            Route::TransactionInfoById,
            "/wallet/gettransactioninfobyid",
            &ById { value: txid },
        )
    }

    /// Transmits exactly the signed bytes, which is why this route is preferred.
    pub(crate) fn broadcast_hex(signed: &[u8]) -> Result<Self, TronError> {
        Self::post(
            Route::BroadcastHex,
            "/wallet/broadcasthex",
            &BroadcastHexBody { transaction: hex::encode(signed) },
        )
    }

    /// The JSON broadcast, whose body [`crate::broadcast`] builds from the same
    /// signed bytes.
    pub(crate) fn broadcast_json(body: String) -> Self {
        Self {
            route: Route::BroadcastTransaction,
            path: String::from("/wallet/broadcasttransaction"),
            query: String::new(),
            body: Some(body),
        }
    }

    /// One page of an indexer history stream, newest first.
    ///
    /// The fingerprint is `TronGrid`'s opaque cursor, sent back verbatim since it's
    /// already URL-safe.
    pub(crate) fn history(stream: Stream, address: &str, fingerprint: Option<&str>) -> Self {
        let list = match stream {
            Stream::Native => "transactions",
            Stream::Token => "transactions/trc20",
            Stream::Internal => "internal-transactions",
        };
        let mut query = format!("limit={HISTORY_PAGE_ROWS}&order_by=block_timestamp,desc");
        if let Some(cursor) = fingerprint {
            query.push_str("&fingerprint=");
            query.push_str(cursor);
        }
        Self {
            route: stream.route(),
            path: format!("/v1/accounts/{address}/{list}"),
            query,
            body: None,
        }
    }

    /// One page of the transactions `address` sent in the block mined at `timestamp_millis`.
    ///
    /// Blocks are three seconds apart, so a millisecond either side holds that
    /// block alone whether the indexer reads its bounds as inclusive or not.
    pub(crate) fn sent_at(address: &str, timestamp_millis: i64, fingerprint: Option<&str>) -> Self {
        let mut query = format!(
            "only_from=true&min_timestamp={}&max_timestamp={}&limit={MAX_PAGE_ROWS}",
            timestamp_millis.saturating_sub(1),
            timestamp_millis.saturating_add(1)
        );
        if let Some(cursor) = fingerprint {
            query.push_str("&fingerprint=");
            query.push_str(cursor);
        }
        Self {
            route: Route::NativeHistory,
            path: format!("/v1/accounts/{address}/transactions"),
            query,
            body: None,
        }
    }
}

#[derive(Debug, Serialize)]
struct HeaderOnly {
    detail: bool,
}

#[derive(Debug, Serialize)]
struct AddressBody<'a> {
    address: &'a str,
    visible: bool,
}

#[derive(Debug, Serialize)]
struct ById<'a> {
    value: &'a str,
}

#[derive(Debug, Serialize)]
struct BroadcastHexBody {
    transaction: String,
}

#[derive(Debug, Serialize)]
struct ConstantCallBody<'a> {
    owner_address: &'a str,
    contract_address: &'a str,
    function_selector: &'static str,
    parameter: String,
    visible: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_route_has_one_spelling_and_a_response_cap() {
        assert_eq!(Route::COUNT, 12);
        for route in Route::ALL {
            assert_eq!(Route::parse(route.as_str()), Some(route), "{route}");
            assert!(route.max_response_bytes() >= 262_144, "{route}");
        }
        let indexer_routes: Vec<Route> =
            Route::ALL.into_iter().filter(|route| route.is_indexer()).collect();
        assert_eq!(
            indexer_routes,
            [Route::NativeHistory, Route::TokenHistory, Route::InternalHistory]
        );
    }

    #[test]
    fn a_wallet_request_carries_the_visible_flag_so_addresses_stay_base58() {
        let request = Request::account("TQLCsShbQNXMTVCjprY64qZmEA4rBarpQp").expect("it builds");
        assert_eq!(request.route(), Route::Account);
        assert_eq!(request.path(), "/wallet/getaccount");
        assert_eq!(request.query(), "", "a /wallet route takes its arguments in the body");
        assert_eq!(
            request.body(),
            Some(r#"{"address":"TQLCsShbQNXMTVCjprY64qZmEA4rBarpQp","visible":true}"#)
        );
    }

    #[test]
    fn the_parameter_read_posts_an_empty_object() {
        let request = Request::chain_parameters().expect("it builds");
        assert_eq!(request.path(), "/wallet/getchainparameters");
        assert_eq!(request.body(), Some("{}"));
    }

    #[test]
    fn a_history_request_keeps_its_arguments_out_of_the_path() {
        // Path and query stay apart, so an endpoint whose own address carries an
        // API key in its query keeps it.
        let first = Request::history(Stream::Native, "TQLC", None);
        assert_eq!(first.body(), None, "a history page is a GET");
        assert_eq!(first.path(), "/v1/accounts/TQLC/transactions");
        assert_eq!(first.query(), "limit=50&order_by=block_timestamp,desc");
        assert!(!first.path().contains('?'), "{}", first.path());

        let next = Request::history(Stream::Token, "TQLC", Some("Fp1"));
        assert_eq!(next.path(), "/v1/accounts/TQLC/transactions/trc20");
        assert_eq!(next.query(), "limit=50&order_by=block_timestamp,desc&fingerprint=Fp1");
    }

    #[test]
    fn internal_transfers_page_like_the_other_streams_and_a_parent_is_read_by_its_block() {
        let page = Request::history(Stream::Internal, "TQLC", Some("Fp1"));
        assert_eq!(page.route(), Route::InternalHistory);
        assert_eq!(page.body(), None);
        assert_eq!(page.path(), "/v1/accounts/TQLC/internal-transactions");
        assert_eq!(page.query(), "limit=50&order_by=block_timestamp,desc&fingerprint=Fp1");

        let parent = Request::sent_at("TQLC", 1_786_585_806_000, None);
        assert_eq!(parent.route(), Route::NativeHistory);
        assert_eq!(parent.path(), "/v1/accounts/TQLC/transactions");
        assert_eq!(
            parent.query(),
            "only_from=true&min_timestamp=1786585805999&max_timestamp=1786585806001&limit=200"
        );
    }

    #[test]
    fn the_head_block_is_read_without_its_transactions() {
        // A head block read with its transactions, as getnowblock answers it, is
        // larger than the cap on this route. Five reads from api.trongrid.io on
        // 2026-10-05 measured 435,286 to 537,643 bytes.
        let request = Request::now_block().expect("it builds");
        assert_eq!(request.route(), Route::NowBlock);
        assert_eq!(request.path(), "/wallet/getblock");
        assert_eq!(request.body(), Some(r#"{"detail":false}"#));
    }
}
