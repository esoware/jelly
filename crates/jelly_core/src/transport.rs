use crate::wire_enum;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// How an endpoint is reached. The pool takes its timeout policy from it, and with the
    /// chain it picks the connector, which also checks which network the endpoint serves.
    pub enum Transport {
        /// Electrum protocol, newline-delimited JSON-RPC over a raw TLS socket.
        ElectrumSsl => "electrum_ssl",
        /// JSON-RPC over HTTP(S), for EVM, Solana, XRP, and TON.
        HttpJsonrpc => "http_jsonrpc",
        /// JSON-RPC over a websocket, for the push subscriptions HTTP can't carry, such as
        /// EVM `eth_subscribe` and Solana `*Subscribe`.
        WsJsonrpc => "ws_jsonrpc",
        /// Monero daemon RPC, a `/json_rpc` envelope plus binary sub-paths.
        MoneroRpc => "monero_rpc",
        /// TRON full-node HTTP API, `POST /wallet/*`.
        TronHttp => "tron_http",
        /// Transparent Zcash history, unspent outputs, and broadcast over HTTPS.
        Blockbook => "blockbook",
        /// mempool.space-family fee and mempool REST API.
        FeeApi => "fee_api",
        /// Etherscan-family transaction history REST API.
        HistoryApi => "history_api",
    }
}
