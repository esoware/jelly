use jelly_core::wire_enum;

wire_enum! {
    /// Every HTTP JSON-RPC method this crate sends, so each name is spelled only
    /// here.
    pub enum Method {
        GetGenesisHash => "getGenesisHash",
        GetSlot => "getSlot",
        GetBlockHeight => "getBlockHeight",
        GetBalance => "getBalance",
        GetAccountInfo => "getAccountInfo",
        GetLatestBlockhash => "getLatestBlockhash",
        GetMinimumBalanceForRentExemption => "getMinimumBalanceForRentExemption",
        GetRecentPrioritizationFees => "getRecentPrioritizationFees",
        GetSignaturesForAddress => "getSignaturesForAddress",
        GetSignatureStatuses => "getSignatureStatuses",
        GetTransaction => "getTransaction",
        SimulateTransaction => "simulateTransaction",
        SendTransaction => "sendTransaction",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_method_round_trips_through_its_name() {
        for method in Method::ALL {
            assert_eq!(Method::parse(method.as_str()), Some(method));
            assert!(method.as_str().starts_with(|c: char| c.is_ascii_lowercase()), "{method}");
        }
        assert_eq!(Method::COUNT, 13);
        assert_eq!(Method::parse("getFeeForMessage"), None);
    }
}
