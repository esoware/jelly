const HISTORY_PAGE_SIZE: u32 = 1_000;

/// A Blockbook read. The query is kept apart from the path because `jelly_net` merges it
/// with any query the endpoint's URL already has.
#[derive(Debug, Clone, Copy)]
pub enum Route<'a> {
    Status,
    BlockHash(u32),
    Block(u32),
    Tx(&'a str),
    Unspent(&'a str),
    History { address: &'a str, page: u32 },
}

impl Route<'_> {
    /// The path relative to the endpoint, and the query.
    pub(crate) fn path_and_query(self) -> (String, String) {
        match self {
            Self::Status => (String::from("api/v2"), String::new()),
            Self::BlockHash(height) => (format!("api/v2/block-index/{height}"), String::new()),
            Self::Block(height) => (format!("api/v2/block/{height}"), String::new()),
            Self::Tx(txid) => (format!("api/v2/tx/{txid}"), String::new()),
            Self::Unspent(address) => {
                (format!("api/v2/utxo/{address}"), String::from("confirmed=false"))
            }
            Self::History { address, page } => (
                format!("api/v2/address/{address}"),
                format!("details=txids&page={page}&pageSize={HISTORY_PAGE_SIZE}"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::OWNER;

    #[test]
    fn every_route_is_the_blockbook_path_and_query_it_names() {
        let txid = "ab".repeat(32);
        for (route, path, query) in [
            (Route::Status, String::from("api/v2"), ""),
            (Route::BlockHash(0), String::from("api/v2/block-index/0"), ""),
            (Route::Block(3_514_158), String::from("api/v2/block/3514158"), ""),
            (Route::Tx(&txid), format!("api/v2/tx/{txid}"), ""),
            (Route::Unspent(OWNER), format!("api/v2/utxo/{OWNER}"), "confirmed=false"),
            (
                Route::History { address: OWNER, page: 2 },
                format!("api/v2/address/{OWNER}"),
                "details=txids&page=2&pageSize=1000",
            ),
        ] {
            assert_eq!(route.path_and_query(), (path, query.to_owned()), "{route:?}");
        }
    }
}
