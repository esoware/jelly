use jelly_core::wire_enum;

wire_enum! {
    /// Electrum method names. `get_balance` is absent because balances come from
    /// verified unspents.
    pub enum Method {
        ServerVersion => "server.version",
        ServerFeatures => "server.features",
        HeadersSubscribe => "blockchain.headers.subscribe",
        ScripthashSubscribe => "blockchain.scripthash.subscribe",
        BlockHeader => "blockchain.block.header",
        GetHistory => "blockchain.scripthash.get_history",
        ListUnspent => "blockchain.scripthash.listunspent",
        GetTransaction => "blockchain.transaction.get",
        Broadcast => "blockchain.transaction.broadcast",
        EstimateFee => "blockchain.estimatefee",
        FeeHistogram => "mempool.get_fee_histogram",
        MempoolInfo => "mempool.get_info",
    }
}
