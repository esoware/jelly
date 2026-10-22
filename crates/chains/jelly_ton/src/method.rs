jelly_core::wire_enum! {
    /// Every TON HTTP JSON-RPC method this crate sends, so each name is spelled only here.
    pub enum Method {
        GetMasterchainInfo => "getMasterchainInfo",
        GetAddressInformation => "getAddressInformation",
        RunGet => "runGetMethod",
        EstimateFee => "estimateFee",
        GetTransactions => "getTransactions",
        SendBoc => "sendBoc",
    }
}
