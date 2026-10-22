jelly_core::wire_enum! {
    /// Every rippled JSON-RPC method this crate sends, so each name is spelled only here.
    pub enum Method {
        ServerInfo => "server_info",
        ServerState => "server_state",
        AccountInfo => "account_info",
        AccountObjects => "account_objects",
        AccountTx => "account_tx",
        DepositAuthorized => "deposit_authorized",
        Fee => "fee",
        Tx => "tx",
        Submit => "submit",
    }
}
