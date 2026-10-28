use jelly_core::wire_enum;

wire_enum! {
    /// Every HTTP JSON-RPC method this crate sends, so no name is spelled twice.
    pub enum Method {
        ChainId => "eth_chainId",
        BlockNumber => "eth_blockNumber",
        GetBalance => "eth_getBalance",
        GetTransactionCount => "eth_getTransactionCount",
        Call => "eth_call",
        EstimateGas => "eth_estimateGas",
        FeeHistory => "eth_feeHistory",
        GetLogs => "eth_getLogs",
        GetTransactionReceipt => "eth_getTransactionReceipt",
        GetTransactionByHash => "eth_getTransactionByHash",
        SendRawTransaction => "eth_sendRawTransaction",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_method_is_an_eth_namespace_call_and_round_trips_through_its_name() {
        for method in Method::ALL {
            assert!(method.as_str().starts_with("eth_"), "{method}");
            assert_eq!(Method::parse(method.as_str()), Some(method));
        }
        assert_eq!(Method::COUNT, 11);
        assert_eq!(Method::parse("eth_gasPrice"), None);
    }
}
