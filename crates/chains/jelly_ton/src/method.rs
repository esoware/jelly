jelly_core::wire_enum! {
    /// The TON HTTP JSON-RPC methods this crate calls, each name spelled only here.
    pub enum Method {
        GetMasterchainInfo => "getMasterchainInfo",
        GetAddressInformation => "getAddressInformation",
        RunGet => "runGetMethod",
        EstimateFee => "estimateFee",
        GetTransactions => "getTransactions",
        SendBoc => "sendBoc",
    }
}
